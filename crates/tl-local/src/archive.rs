//! Archives on this Mac, read and written in-process with the `tar`,
//! `flate2` and `zip` crates. Adding or removing members writes a new
//! archive next to the old one and renames it into place, so a failure
//! never leaves a half-written archive behind.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, PipeReader, PipeWriter, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use flate2::Compression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use tl_core::archive::{ArchiveBackend, ArchiveFormat, ArchiveMember, member_below, member_join, normalize_member};
use tl_core::{CancelToken, EntryKind, OpCtx, TarSink, TarSource, VfsError, VfsResult};
use zip::write::SimpleFileOptions;

use crate::tarstream::{spawn_sink, spawn_source};

#[derive(Clone)]
pub struct LocalArchive {
    path: PathBuf,
    format: ArchiveFormat,
}

type TarReader = tar::Archive<Box<dyn Read + Send>>;
type ZipReader = zip::ZipArchive<BufReader<File>>;

fn bytes_path(bytes: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(bytes))
}

/// `./<rel>`, how exported members are named.
fn dot_path(rel: &[u8]) -> PathBuf {
    let mut path = b"./".to_vec();
    path.extend_from_slice(rel);
    PathBuf::from(OsStr::from_bytes(&path))
}

fn tar_kind(kind: tar::EntryType) -> Option<EntryKind> {
    Some(match kind {
        tar::EntryType::Directory => EntryKind::Dir,
        tar::EntryType::Symlink => EntryKind::Symlink,
        tar::EntryType::Regular | tar::EntryType::Continuous | tar::EntryType::Link | tar::EntryType::GNUSparse => EntryKind::File,
        tar::EntryType::Fifo => EntryKind::Fifo,
        tar::EntryType::Char => EntryKind::CharDevice,
        tar::EntryType::Block => EntryKind::BlockDevice,
        _ => return None,
    })
}

fn zip_kind<R: Read + ?Sized>(file: &zip::read::ZipFile<'_, R>) -> EntryKind {
    if file.is_dir() {
        EntryKind::Dir
    } else if file.is_symlink() {
        EntryKind::Symlink
    } else {
        EntryKind::File
    }
}

/// The local time zone's reading of calendar fields.
fn local_epoch(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Option<i64> {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year as i32 - 1900;
    tm.tm_mon = month as i32 - 1;
    tm.tm_mday = day as i32;
    tm.tm_hour = hour as i32;
    tm.tm_min = minute as i32;
    tm.tm_sec = second as i32;
    tm.tm_isdst = -1;
    let t = unsafe { libc::mktime(&mut tm) };
    (t != -1).then_some(t as i64)
}

/// Zip times are local wall-clock times (DOS format), unless an extended
/// timestamp field gives the Unix time.
fn zip_mtime<R: Read>(file: &zip::read::ZipFile<'_, R>) -> Option<i64> {
    for field in file.extra_data_fields() {
        if let zip::ExtraField::ExtendedTimestamp(ts) = field
            && let Some(t) = ts.mod_time()
        {
            return Some(t as i64);
        }
    }
    let t = file.last_modified()?;
    local_epoch(t.year(), t.month(), t.day(), t.hour(), t.minute(), t.second())
}

fn zip_datetime(epoch: u64) -> zip::DateTime {
    let t = epoch as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return zip::DateTime::default();
    }
    zip::DateTime::from_date_and_time(
        (tm.tm_year + 1900).clamp(1980, 2107) as u16,
        (tm.tm_mon + 1) as u8,
        tm.tm_mday as u8,
        tm.tm_hour as u8,
        tm.tm_min as u8,
        tm.tm_sec as u8,
    )
    .unwrap_or_default()
}

/// The part of `rel` below `base` when it is, or is inside, one of `names`.
fn selected<'a>(rel: &'a [u8], base: &[u8], names: &HashSet<Vec<u8>>) -> Option<&'a [u8]> {
    let below = member_below(rel, base).filter(|b| !b.is_empty())?;
    let top = below.split(|b| *b == b'/').next()?;
    names.contains(top).then_some(below)
}

/// `a/b/c` → `a`, `a/b`.
fn ancestors(rel: &[u8]) -> impl Iterator<Item = &[u8]> {
    rel.iter()
        .enumerate()
        .filter(|(_, b)| **b == b'/')
        .map(move |(i, _)| &rel[..i])
}

/// Old members a rewrite drops: replaced by an added path, below a path
/// that became a file, or below a removed path.
struct Dropped<'a> {
    added: HashSet<Vec<u8>>,
    added_files: HashSet<Vec<u8>>,
    removed: &'a [Vec<u8>],
}

impl Dropped<'_> {
    fn contains(&self, rel: &[u8]) -> bool {
        self.added.contains(rel)
            || ancestors(rel).any(|a| self.added_files.contains(a))
            || self.removed.iter().any(|r| member_below(rel, r).is_some())
    }

    fn add(&mut self, rel: Vec<u8>, is_dir: bool) {
        if !is_dir {
            self.added_files.insert(rel.clone());
        }
        self.added.insert(rel);
    }
}

/// A new archive being written next to the old one; removed unless it
/// replaces the old one.
struct TempArchive {
    path: PathBuf,
    done: bool,
}

impl TempArchive {
    fn create(next_to: &Path) -> VfsResult<(TempArchive, File)> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = next_to.parent().unwrap_or(Path::new("/"));
        let name = next_to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!(".{name}.tl-{}-{n}", std::process::id()));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok((TempArchive { path, done: false }, file)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(VfsError::io(e, dir.display())),
            }
        }
    }

    fn replace(mut self, target: &Path) -> VfsResult<()> {
        if let Ok(meta) = fs::metadata(target) {
            let _ = fs::set_permissions(&self.path, meta.permissions());
        }
        fs::rename(&self.path, target).map_err(|e| VfsError::io(e, target.display()))?;
        self.done = true;
        Ok(())
    }
}

impl Drop for TempArchive {
    fn drop(&mut self) {
        if !self.done {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl LocalArchive {
    pub fn new(path: PathBuf, format: ArchiveFormat) -> Self {
        LocalArchive { path, format }
    }

    fn err(&self, e: impl std::fmt::Display) -> VfsError {
        VfsError::other(format!("{}: {e}", self.path.display()))
    }

    /// An I/O error while streaming: cancellation explains it best.
    fn stream_err(&self, e: io::Error, cancel: &CancelToken) -> VfsError {
        if cancel.is_cancelled() { VfsError::Cancelled } else { self.err(e) }
    }

    fn open_file(&self) -> VfsResult<File> {
        File::open(&self.path).map_err(|e| VfsError::io(e, self.path.display()))
    }

    fn open_tar(&self) -> VfsResult<TarReader> {
        let file = BufReader::new(self.open_file()?);
        let reader: Box<dyn Read + Send> = match self.format {
            ArchiveFormat::TarGz => Box::new(MultiGzDecoder::new(file)),
            _ => Box::new(file),
        };
        Ok(tar::Archive::new(reader))
    }

    fn open_zip(&self) -> VfsResult<ZipReader> {
        zip::ZipArchive::new(BufReader::new(self.open_file()?)).map_err(|e| self.err(e))
    }

    fn list_tar(&self, cancel: &CancelToken) -> VfsResult<Vec<ArchiveMember>> {
        let mut archive = self.open_tar()?;
        let mut out = Vec::new();
        for entry in archive.entries().map_err(|e| self.err(e))? {
            cancel.check()?;
            let entry = entry.map_err(|e| self.err(e))?;
            let header = entry.header();
            let Some(kind) = tar_kind(header.entry_type()) else { continue };
            let mut member = ArchiveMember::new(entry.path_bytes().into_owned(), kind);
            member.size = if header.entry_type().is_file() { entry.size() } else { 0 };
            member.mtime = header.mtime().ok().map(|t| t as i64);
            member.mode = header.mode().unwrap_or(0) & 0o7777;
            member.link_target = entry.link_name_bytes().map(|t| t.into_owned());
            out.push(member);
        }
        Ok(out)
    }

    fn list_zip(&self, cancel: &CancelToken) -> VfsResult<Vec<ArchiveMember>> {
        let mut zip = self.open_zip()?;
        let mut out = Vec::with_capacity(zip.len());
        for i in 0..zip.len() {
            cancel.check()?;
            let mut member = {
                let file = zip.by_index_raw(i).map_err(|e| self.err(e))?;
                let kind = zip_kind(&file);
                let mut member = ArchiveMember::new(file.name_raw().to_vec(), kind);
                member.size = if kind == EntryKind::File { file.size() } else { 0 };
                member.mtime = zip_mtime(&file);
                member.mode = file.unix_mode().unwrap_or(0) & 0o7777;
                member
            };
            if member.kind == EntryKind::Symlink {
                // The link target is the member's content.
                let file = zip.by_index(i).map_err(|e| self.err(e))?;
                let mut target = Vec::new();
                file.take(4096).read_to_end(&mut target).map_err(|e| self.err(e))?;
                member.link_target = Some(target);
            }
            out.push(member);
        }
        Ok(out)
    }

    fn export_from_tar(&self, base: &[u8], names: &HashSet<Vec<u8>>, writer: PipeWriter, cancel: &CancelToken) -> VfsResult<()> {
        let mut archive = self.open_tar()?;
        let mut builder = tar::Builder::new(writer);
        for entry in archive.entries().map_err(|e| self.err(e))? {
            cancel.check()?;
            let mut entry = entry.map_err(|e| self.err(e))?;
            let raw = entry.path_bytes().into_owned();
            let Some(rel) = normalize_member(&raw) else { continue };
            let Some(below) = selected(&rel, base, names) else { continue };
            let path = dot_path(below);
            let mut header = entry.header().clone();
            let written = match header.entry_type() {
                tar::EntryType::Symlink => {
                    let target = entry.link_name_bytes().map(|t| t.into_owned()).unwrap_or_default();
                    builder.append_link(&mut header, &path, bytes_path(&target))
                }
                tar::EntryType::Link => {
                    // A hard link names another member; it can only come
                    // along when that member does too.
                    let target = entry.link_name_bytes().and_then(|t| normalize_member(&t));
                    match target.as_deref().and_then(|t| selected(t, base, names)) {
                        Some(t) => builder.append_link(&mut header, &path, dot_path(t)),
                        None => {
                            log::warn!("skipping hard link {} to a member outside the selection", String::from_utf8_lossy(&raw));
                            continue;
                        }
                    }
                }
                kind if tar_kind(kind).is_some() => builder.append_data(&mut header, &path, &mut entry),
                _ => continue,
            };
            written.map_err(|e| self.stream_err(e, cancel))?;
        }
        builder.finish().map_err(|e| self.stream_err(e, cancel))
    }

    fn export_from_zip(&self, base: &[u8], names: &HashSet<Vec<u8>>, writer: PipeWriter, cancel: &CancelToken) -> VfsResult<()> {
        let mut zip = self.open_zip()?;
        let mut builder = tar::Builder::new(writer);
        for i in 0..zip.len() {
            cancel.check()?;
            let mut file = zip.by_index(i).map_err(|e| self.err(e))?;
            let Some(rel) = normalize_member(file.name_raw()) else { continue };
            let Some(below) = selected(&rel, base, names) else { continue };
            let path = dot_path(below);
            let kind = zip_kind(&file);
            let mut header = tar::Header::new_gnu();
            header.set_mode(match file.unix_mode().unwrap_or(0) & 0o7777 {
                0 if kind == EntryKind::Dir => 0o755,
                0 => 0o644,
                mode => mode,
            });
            header.set_mtime(zip_mtime(&file).unwrap_or(0).max(0) as u64);
            let written = match kind {
                EntryKind::Dir => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_size(0);
                    builder.append_data(&mut header, &path, io::empty())
                }
                EntryKind::Symlink => {
                    let mut target = Vec::new();
                    file.read_to_end(&mut target).map_err(|e| self.err(e))?;
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_size(0);
                    builder.append_link(&mut header, &path, bytes_path(&target))
                }
                _ => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(file.size());
                    builder.append_data(&mut header, &path, &mut file)
                }
            };
            written.map_err(|e| self.stream_err(e, cancel))?;
        }
        builder.finish().map_err(|e| self.stream_err(e, cancel))
    }

    /// Writes a new archive — `incoming` members placed below `dir`, then
    /// the old members that are neither replaced nor below `removed` — and
    /// puts it in place of the old one.
    fn rewrite(&self, incoming: Option<(&[u8], &mut tar::Archive<PipeReader>)>, removed: &[Vec<u8>], cancel: &CancelToken) -> VfsResult<()> {
        let (temp, file) = TempArchive::create(&self.path)?;
        let file = match self.format {
            ArchiveFormat::Tar => self.write_tar(BufWriter::new(file), incoming, removed, cancel)?.into_inner().map_err(|e| self.err(e.into_error()))?,
            ArchiveFormat::TarGz => {
                let gz = GzEncoder::new(BufWriter::new(file), Compression::default());
                let gz = self.write_tar(gz, incoming, removed, cancel)?;
                gz.finish().map_err(|e| self.err(e))?.into_inner().map_err(|e| self.err(e.into_error()))?
            }
            ArchiveFormat::Zip => self.write_zip(file, incoming, removed, cancel)?,
        };
        file.sync_all().map_err(|e| self.err(e))?;
        drop(file);
        cancel.check()?;
        temp.replace(&self.path)
    }

    fn write_tar<W: Write>(
        &self,
        out: W,
        incoming: Option<(&[u8], &mut tar::Archive<PipeReader>)>,
        removed: &[Vec<u8>],
        cancel: &CancelToken,
    ) -> VfsResult<W> {
        let mut builder = tar::Builder::new(out);
        let mut dropped = Dropped {
            added: HashSet::new(),
            added_files: HashSet::new(),
            removed,
        };
        let io_err = |e: io::Error| self.stream_err(e, cancel);
        if let Some((dir, incoming)) = incoming {
            for entry in incoming.entries().map_err(io_err)? {
                cancel.check()?;
                let mut entry = entry.map_err(io_err)?;
                let Some(rel) = normalize_member(&entry.path_bytes()) else { continue };
                let path = member_join(dir, &rel);
                let mut header = entry.header().clone();
                let kind = header.entry_type();
                match kind {
                    tar::EntryType::Symlink | tar::EntryType::Link => {
                        let mut target = entry.link_name_bytes().map(|t| t.into_owned()).unwrap_or_default();
                        if kind == tar::EntryType::Link {
                            // Hard links name members of the incoming stream.
                            target = member_join(dir, &normalize_member(&target).unwrap_or_default());
                        }
                        builder.append_link(&mut header, bytes_path(&path), bytes_path(&target)).map_err(io_err)?;
                    }
                    k if tar_kind(k).is_some() => builder.append_data(&mut header, bytes_path(&path), &mut entry).map_err(io_err)?,
                    _ => continue,
                }
                dropped.add(path, kind == tar::EntryType::Directory);
            }
        }
        let mut old = self.open_tar()?;
        for entry in old.entries().map_err(|e| self.err(e))? {
            cancel.check()?;
            let mut entry = entry.map_err(|e| self.err(e))?;
            let raw = entry.path_bytes().into_owned();
            let Some(rel) = normalize_member(&raw) else { continue };
            if dropped.contains(&rel) {
                continue;
            }
            let path = raw.strip_prefix(b"/").unwrap_or(&raw).to_vec();
            let mut header = entry.header().clone();
            match header.entry_type() {
                tar::EntryType::Symlink | tar::EntryType::Link => {
                    let target = entry.link_name_bytes().map(|t| t.into_owned()).unwrap_or_default();
                    builder.append_link(&mut header, bytes_path(&path), bytes_path(&target)).map_err(io_err)?;
                }
                k if tar_kind(k).is_some() => builder.append_data(&mut header, bytes_path(&path), &mut entry).map_err(io_err)?,
                _ => {}
            }
        }
        builder.into_inner().map_err(io_err)
    }

    fn write_zip(
        &self,
        file: File,
        incoming: Option<(&[u8], &mut tar::Archive<PipeReader>)>,
        removed: &[Vec<u8>],
        cancel: &CancelToken,
    ) -> VfsResult<File> {
        let mut writer = zip::ZipWriter::new(BufWriter::new(file));
        let mut dropped = Dropped {
            added: HashSet::new(),
            added_files: HashSet::new(),
            removed,
        };
        let io_err = |e: io::Error| self.stream_err(e, cancel);
        let zip_err = |e: zip::result::ZipError| self.err(e);
        if let Some((dir, incoming)) = incoming {
            for entry in incoming.entries().map_err(io_err)? {
                cancel.check()?;
                let mut entry = entry.map_err(io_err)?;
                let Some(rel) = normalize_member(&entry.path_bytes()) else { continue };
                let path = member_join(dir, &rel);
                let name = String::from_utf8_lossy(&path).into_owned();
                let header = entry.header();
                let kind = header.entry_type();
                let options = SimpleFileOptions::default()
                    .unix_permissions(header.mode().unwrap_or(0o644) & 0o7777)
                    .last_modified_time(zip_datetime(header.mtime().unwrap_or(0)));
                match kind {
                    tar::EntryType::Directory => writer.add_directory(name, options).map_err(zip_err)?,
                    tar::EntryType::Symlink => {
                        let target = entry.link_name_bytes().map(|t| String::from_utf8_lossy(&t).into_owned()).unwrap_or_default();
                        writer.add_symlink(name, target, options).map_err(zip_err)?;
                    }
                    tar::EntryType::Regular | tar::EntryType::Continuous => {
                        let options = options
                            .compression_method(zip::CompressionMethod::Deflated)
                            .large_file(entry.size() >= u32::MAX as u64);
                        writer.start_file(name, options).map_err(zip_err)?;
                        io::copy(&mut entry, &mut writer).map_err(io_err)?;
                    }
                    _ => {
                        log::warn!("zip archives cannot hold {}; skipped", name);
                        continue;
                    }
                }
                dropped.add(path, kind == tar::EntryType::Directory);
            }
        }
        let mut old = self.open_zip()?;
        for i in 0..old.len() {
            cancel.check()?;
            let file = old.by_index_raw(i).map_err(zip_err)?;
            if normalize_member(file.name_raw()).is_none_or(|rel| dropped.contains(&rel)) {
                continue;
            }
            writer.raw_copy_file(file).map_err(zip_err)?;
        }
        let buf = writer.finish().map_err(zip_err)?;
        buf.into_inner().map_err(|e| self.err(e.into_error()))
    }

    fn read_member(&self, path: &[u8], limit: u64) -> VfsResult<Vec<u8>> {
        let mut data = Vec::new();
        match self.format {
            ArchiveFormat::Zip => {
                let mut zip = self.open_zip()?;
                let index = (0..zip.len()).rev().find(|i| {
                    zip.by_index_raw(*i)
                        .is_ok_and(|f| normalize_member(f.name_raw()).as_deref() == Some(path))
                });
                let index = index.ok_or_else(|| VfsError::NotFound(String::from_utf8_lossy(path).into_owned()))?;
                let file = zip.by_index(index).map_err(|e| self.err(e))?;
                file.take(limit).read_to_end(&mut data).map_err(|e| self.err(e))?;
            }
            _ => {
                let mut archive = self.open_tar()?;
                let mut found = false;
                for entry in archive.entries().map_err(|e| self.err(e))? {
                    let entry = entry.map_err(|e| self.err(e))?;
                    if entry.header().entry_type().is_file() && normalize_member(&entry.path_bytes()).as_deref() == Some(path) {
                        // A later member of the same name wins.
                        data.clear();
                        entry.take(limit).read_to_end(&mut data).map_err(|e| self.err(e))?;
                        found = true;
                    }
                }
                if !found {
                    return Err(VfsError::NotFound(String::from_utf8_lossy(path).into_owned()));
                }
            }
        }
        Ok(data)
    }
}

impl ArchiveBackend for LocalArchive {
    fn list(&self, ctx: &OpCtx) -> VfsResult<Vec<ArchiveMember>> {
        match self.format {
            ArchiveFormat::Zip => self.list_zip(&ctx.cancel),
            _ => self.list_tar(&ctx.cancel),
        }
    }

    fn export_tar(&self, base: &[u8], names: &[Vec<u8>], ctx: &OpCtx) -> VfsResult<Box<dyn TarSource>> {
        let this = self.clone();
        let base = base.to_vec();
        let names: HashSet<Vec<u8>> = names.iter().cloned().collect();
        let cancel = ctx.cancel.clone();
        let source = spawn_source("archive-export", move |writer| match this.format {
            ArchiveFormat::Zip => this.export_from_zip(&base, &names, writer, &cancel),
            _ => this.export_from_tar(&base, &names, writer, &cancel),
        })?;
        Ok(Box::new(source))
    }

    fn import_tar(&self, dir: &[u8], ctx: &OpCtx) -> VfsResult<Box<dyn TarSink>> {
        // Fail before any data flows when the archive cannot be rewritten.
        OpenOptions::new()
            .write(true)
            .open(&self.path)
            .map_err(|e| VfsError::io(e, self.path.display()))?;
        let this = self.clone();
        let dir = dir.to_vec();
        let cancel = ctx.cancel.clone();
        let sink = spawn_sink("archive-import", move |reader| {
            let mut incoming = tar::Archive::new(reader);
            let result = this.rewrite(Some((&dir, &mut incoming)), &[], &cancel);
            // Drain the rest so the writer never sees a broken pipe.
            let _ = io::copy(&mut incoming.into_inner(), &mut io::sink());
            result
        })?;
        Ok(Box::new(sink))
    }

    fn delete(&self, paths: &[Vec<u8>], ctx: &OpCtx) -> VfsResult<()> {
        self.rewrite(None, paths, &ctx.cancel)
    }

    fn read_file(&self, path: &[u8], limit: u64, _ctx: &OpCtx) -> VfsResult<Vec<u8>> {
        self.read_member(path, limit)
    }
}
