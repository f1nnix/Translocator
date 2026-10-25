//! Archives (tar, tar.gz, zip) browsed as folders.
//!
//! [`ArchiveVfs`] shows an archive file as a file system whose paths
//! continue the host path (`/home/me/site.tar.gz/www/index.html`), so it
//! plugs into panes and the transfer engine like any other location:
//! copying out of it extracts, copying into it adds. The archive work
//! itself is done by an [`ArchiveBackend`] from the provider that holds the
//! file — Rust crates for local files, `tar` / `unzip` on servers.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::entry::{Entry, EntryKind};
use crate::error::{VfsError, VfsResult};
use crate::path::VPath;
use crate::progress::Progress;
use crate::vfs::{
    Capabilities, OpCtx, ScanTotals, SpaceInfo, TarSink, TarSource, TransferItem, Vfs, WalkEntry,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArchiveFormat {
    Tar,
    TarGz,
    Zip,
}

impl ArchiveFormat {
    /// The format of a file, by its name.
    pub fn detect(name: &[u8]) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(b".tar.gz") || lower.ends_with(b".tgz") {
            Some(ArchiveFormat::TarGz)
        } else if lower.ends_with(b".tar") {
            Some(ArchiveFormat::Tar)
        } else if lower.ends_with(b".zip") {
            Some(ArchiveFormat::Zip)
        } else {
            None
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ArchiveFormat::Tar => "tar",
            ArchiveFormat::TarGz => "tar.gz",
            ArchiveFormat::Zip => "zip",
        }
    }

    /// A valid archive with nothing in it, to add files to.
    pub fn empty_archive(self) -> Vec<u8> {
        // Two zero blocks end a tar archive.
        let tar = vec![0u8; 1024];
        match self {
            ArchiveFormat::Tar => tar,
            ArchiveFormat::TarGz => {
                // gzip with one stored (uncompressed) deflate block.
                let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3, 1];
                let len = tar.len() as u16;
                out.extend_from_slice(&len.to_le_bytes());
                out.extend_from_slice(&(!len).to_le_bytes());
                out.extend_from_slice(&tar);
                out.extend_from_slice(&crc32(&tar).to_le_bytes());
                out.extend_from_slice(&(tar.len() as u32).to_le_bytes());
                out
            }
            // Just the end-of-central-directory record.
            ArchiveFormat::Zip => {
                let mut out = vec![0x50, 0x4b, 0x05, 0x06];
                out.resize(22, 0);
                out
            }
        }
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// One member of an archive, as listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveMember {
    /// The path as stored (`./www/`, `www/index.html`).
    pub raw: Vec<u8>,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime: Option<i64>,
    /// Permission bits, `0` when the archive does not record them.
    pub mode: u32,
    pub link_target: Option<Vec<u8>>,
}

impl ArchiveMember {
    pub fn new(raw: impl Into<Vec<u8>>, kind: EntryKind) -> Self {
        ArchiveMember {
            raw: raw.into(),
            kind,
            size: 0,
            mtime: None,
            mode: 0,
            link_target: None,
        }
    }
}

/// A stored member path as a clean relative path: `./a//b/` → `a/b`.
/// `None` for the archive root itself and for paths that would escape it.
pub fn normalize_member(raw: &[u8]) -> Option<Vec<u8>> {
    let mut parts: Vec<&[u8]> = Vec::new();
    for part in raw.split(|b| *b == b'/') {
        match part {
            b"" | b"." => {}
            b".." => return None,
            p => parts.push(p),
        }
    }
    (!parts.is_empty()).then(|| parts.join(&b'/'))
}

/// The part of `path` below `dir` (both normalized; `dir` empty is the root).
pub fn member_below<'a>(path: &'a [u8], dir: &[u8]) -> Option<&'a [u8]> {
    if dir.is_empty() {
        return Some(path);
    }
    let rest = path.strip_prefix(dir)?;
    if rest.is_empty() {
        Some(rest)
    } else {
        rest.strip_prefix(b"/")
    }
}

/// `dir` + `/` + `name`, for normalized member paths.
pub fn member_join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    if dir.is_empty() {
        return name.to_vec();
    }
    let mut path = dir.to_vec();
    path.push(b'/');
    path.extend_from_slice(name);
    path
}

/// The archive work for files a provider holds. Paths are normalized
/// member paths (see [`normalize_member`]); the empty path is the root.
pub trait ArchiveBackend: Send + Sync {
    /// Every member, as stored.
    fn list(&self, ctx: &OpCtx) -> VfsResult<Vec<ArchiveMember>>;

    /// A tar stream of `names` (files or whole folders, relative to the
    /// folder `base`), with members named `./<name>/...`.
    fn export_tar(&self, base: &[u8], names: &[Vec<u8>], ctx: &OpCtx) -> VfsResult<Box<dyn TarSource>>;

    /// A sink whose tar stream is added below the folder `dir`, replacing
    /// members with the same paths. The archive is rewritten when the sink
    /// finishes.
    fn import_tar(&self, dir: &[u8], ctx: &OpCtx) -> VfsResult<Box<dyn TarSink>>;

    /// Removes members and everything below them.
    fn delete(&self, paths: &[Vec<u8>], ctx: &OpCtx) -> VfsResult<()>;

    /// Reads up to `limit` bytes of a file member.
    fn read_file(&self, path: &[u8], limit: u64, ctx: &OpCtx) -> VfsResult<Vec<u8>>;
}

/// The members of an archive as a tree.
struct Index {
    /// Size and time of the archive file when it was listed.
    stamp: (u64, Option<i64>),
    entries: HashMap<Vec<u8>, Entry>,
    children: HashMap<Vec<u8>, Vec<Vec<u8>>>,
}

fn last_component(rel: &[u8]) -> &[u8] {
    rel.rsplit(|b| *b == b'/').next().unwrap_or(rel)
}

fn parent_of(rel: &[u8]) -> &[u8] {
    match rel.iter().rposition(|b| *b == b'/') {
        Some(i) => &rel[..i],
        None => &[],
    }
}

impl Index {
    fn build(members: Vec<ArchiveMember>, stamp: (u64, Option<i64>)) -> Self {
        let mut index = Index {
            stamp,
            entries: HashMap::new(),
            children: HashMap::new(),
        };
        index.children.insert(Vec::new(), Vec::new());
        let mut explicit: HashSet<Vec<u8>> = HashSet::new();
        for member in members {
            let Some(rel) = normalize_member(&member.raw) else { continue };
            index.add_parents(&rel);
            if member.kind == EntryKind::Dir && explicit.contains(&rel) && index.entries.get(&rel).is_some_and(Entry::is_dir) {
                continue;
            }
            explicit.insert(rel.clone());
            let mut entry = Entry::new(last_component(&rel).to_vec(), member.kind);
            entry.size = if member.kind == EntryKind::File { member.size } else { 0 };
            entry.mtime = member.mtime;
            entry.mode = match member.mode & 0o7777 {
                0 if member.kind == EntryKind::Dir => 0o755,
                0 => 0o644,
                m => m,
            };
            entry.link_target = member.link_target;
            if member.kind == EntryKind::Dir {
                index.children.entry(rel.clone()).or_default();
            } else {
                // A file replacing a folder of the same path (later wins).
                index.children.remove(&rel);
            }
            index.insert(rel, entry);
        }
        index
    }

    fn insert(&mut self, rel: Vec<u8>, entry: Entry) {
        if !self.entries.contains_key(&rel) {
            self.children.entry(parent_of(&rel).to_vec()).or_default().push(rel.clone());
        }
        self.entries.insert(rel, entry);
    }

    /// Folders implied by a member path that have no member of their own.
    fn add_parents(&mut self, rel: &[u8]) {
        let mut end = 0;
        while let Some(i) = rel[end..].iter().position(|b| *b == b'/') {
            let dir = &rel[..end + i];
            if !self.entries.get(dir).is_some_and(Entry::is_dir) {
                let mut entry = Entry::new(last_component(dir).to_vec(), EntryKind::Dir);
                entry.mode = 0o755;
                self.children.entry(dir.to_vec()).or_default();
                self.insert(dir.to_vec(), entry);
            }
            end += i + 1;
        }
    }

    fn is_dir(&self, rel: &[u8]) -> bool {
        rel.is_empty() || self.entries.get(rel).is_some_and(|e| e.kind == EntryKind::Dir)
    }

    /// Every path below `rel` (not including it).
    fn descendants(&self, rel: &[u8]) -> Vec<&Vec<u8>> {
        let mut out = Vec::new();
        let mut stack = vec![rel.to_vec()];
        while let Some(dir) = stack.pop() {
            for child in self.children.get(&dir).into_iter().flatten() {
                out.push(child);
                if self.is_dir(child) {
                    stack.push(child.clone());
                }
            }
        }
        out
    }
}

/// An archive file shown as a folder.
pub struct ArchiveVfs {
    id: String,
    key: String,
    label: String,
    host: Arc<dyn Vfs>,
    file: VPath,
    format: ArchiveFormat,
    backend: Arc<dyn ArchiveBackend>,
    index: Arc<Mutex<Option<Arc<Index>>>>,
}

/// Opens `file` on `host` as an archive. Reads the member list once, so a
/// file that is not a valid archive fails here.
pub fn open(host: Arc<dyn Vfs>, file: VPath, ctx: &OpCtx) -> VfsResult<Arc<ArchiveVfs>> {
    let name = file.file_name().unwrap_or_default();
    let format = ArchiveFormat::detect(name)
        .ok_or_else(|| VfsError::Unsupported(format!("{} is not a tar, tar.gz or zip archive", file.file_name_lossy())))?;
    if host.host().is_some() {
        return Err(VfsError::Unsupported("archives inside archives".into()));
    }
    let backend = host.open_archive(&file, format)?;
    let vfs = Arc::new(ArchiveVfs {
        id: format!("archive:{}:{}", host.id(), file),
        key: format!("archive:{}:{}", host.location_key(), file),
        label: file.file_name_lossy(),
        host,
        file,
        format,
        backend,
        index: Arc::new(Mutex::new(None)),
    });
    vfs.index(ctx)?;
    Ok(vfs)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Clears the cached listing once the archive was rewritten.
struct RefreshingSink {
    inner: Box<dyn TarSink>,
    index: Arc<Mutex<Option<Arc<Index>>>>,
}

impl Write for RefreshingSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl TarSink for RefreshingSink {
    fn finish(self: Box<Self>) -> VfsResult<()> {
        let result = self.inner.finish();
        *self.index.lock() = None;
        result
    }
}

impl ArchiveVfs {
    pub fn format(&self) -> ArchiveFormat {
        self.format
    }

    /// The archive file on the host.
    pub fn file(&self) -> &VPath {
        &self.file
    }

    fn index(&self, ctx: &OpCtx) -> VfsResult<Arc<Index>> {
        let meta = self
            .host
            .stat(&self.file, ctx)?
            .ok_or_else(|| VfsError::NotFound(self.file.display()))?;
        let stamp = (meta.size, meta.mtime);
        if let Some(index) = self.index.lock().as_ref().filter(|i| i.stamp == stamp) {
            return Ok(index.clone());
        }
        let index = Arc::new(Index::build(self.backend.list(ctx)?, stamp));
        *self.index.lock() = Some(index.clone());
        Ok(index)
    }

    fn invalidate(&self) {
        *self.index.lock() = None;
    }

    /// The member path of `path`.
    fn rel(&self, path: &VPath) -> VfsResult<Vec<u8>> {
        path.relative_to(&self.file)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| VfsError::NotFound(path.display()))
    }

    fn root_entry(&self) -> Entry {
        let mut entry = Entry::new(self.file.file_name().unwrap_or(b"/").to_vec(), EntryKind::Dir);
        entry.mode = 0o755;
        entry
    }

    /// Adds one entry to the archive (mkdir, new file, saved file).
    fn add_one(&self, path: &VPath, kind: EntryKind, mode: u32, data: &[u8], ctx: &OpCtx) -> VfsResult<()> {
        let rel = self.rel(path)?;
        if rel.is_empty() {
            return Err(VfsError::AlreadyExists(path.display()));
        }
        let name = last_component(&rel).to_vec();
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(if kind == EntryKind::Dir { tar::EntryType::Directory } else { tar::EntryType::Regular });
        header.set_mode(mode);
        header.set_mtime(now());
        header.set_size(data.len() as u64);
        let mut member = b"./".to_vec();
        member.extend_from_slice(&name);
        let mut sink = self.backend.import_tar(parent_of(&rel), ctx)?;
        let written = {
            let mut builder = tar::Builder::new(&mut sink);
            let path = std::path::Path::new(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(&member));
            builder.append_data(&mut header, path, data).and_then(|_| builder.finish())
        };
        let finished = sink.finish();
        self.invalidate();
        finished?;
        written.map_err(|e| VfsError::other(format!("Cannot add {} to the archive: {e}", path.file_name_lossy())))
    }
}

impl Vfs for ArchiveVfs {
    fn id(&self) -> &str {
        &self.id
    }

    fn plugin_id(&self) -> &str {
        "archive"
    }

    fn label(&self) -> String {
        self.label.clone()
    }

    fn location_key(&self) -> &str {
        &self.key
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            elevation: self.host.capabilities().elevation,
            ..Default::default()
        }
    }

    fn is_connected(&self) -> bool {
        self.host.is_connected()
    }

    fn home(&self) -> VPath {
        self.file.clone()
    }

    fn host(&self) -> Option<(Arc<dyn Vfs>, VPath)> {
        Some((self.host.clone(), self.file.clone()))
    }

    fn list(&self, dir: &VPath, ctx: &OpCtx) -> VfsResult<Vec<Entry>> {
        let rel = self.rel(dir)?;
        let index = self.index(ctx)?;
        if !index.is_dir(&rel) {
            return Err(VfsError::NotFound(dir.display()));
        }
        Ok(index
            .children
            .get(&rel)
            .into_iter()
            .flatten()
            .filter_map(|child| index.entries.get(child).cloned())
            .collect())
    }

    fn stat(&self, path: &VPath, ctx: &OpCtx) -> VfsResult<Option<Entry>> {
        let rel = self.rel(path)?;
        if rel.is_empty() {
            return Ok(Some(self.root_entry()));
        }
        Ok(self.index(ctx)?.entries.get(&rel).cloned())
    }

    fn walk(&self, root: &VPath, ctx: &OpCtx) -> VfsResult<Vec<WalkEntry>> {
        let rel = self.rel(root)?;
        let index = self.index(ctx)?;
        if !index.is_dir(&rel) {
            return Err(VfsError::NotFound(root.display()));
        }
        let mut out: Vec<WalkEntry> = index
            .descendants(&rel)
            .into_iter()
            .filter_map(|path| {
                let entry = index.entries.get(path)?;
                Some(WalkEntry {
                    rel: member_below(path, &rel)?.to_vec(),
                    kind: entry.kind,
                    size: entry.size,
                    mtime: entry.mtime,
                })
            })
            .collect();
        out.sort_by(|a, b| a.rel.cmp(&b.rel));
        Ok(out)
    }

    fn scan(&self, paths: &[VPath], ctx: &OpCtx) -> VfsResult<ScanTotals> {
        let index = self.index(ctx)?;
        let mut totals = ScanTotals::default();
        let mut count = |entry: &Entry| match entry.kind {
            EntryKind::Dir => totals.dirs += 1,
            EntryKind::File => {
                totals.files += 1;
                totals.bytes += entry.size;
            }
            _ => totals.files += 1,
        };
        for path in paths {
            let rel = self.rel(path)?;
            if rel.is_empty() {
                count(&self.root_entry());
            } else {
                count(index.entries.get(&rel).ok_or_else(|| VfsError::NotFound(path.display()))?);
            }
            for child in index.descendants(&rel) {
                if let Some(entry) = index.entries.get(child) {
                    count(entry);
                }
            }
        }
        Ok(totals)
    }

    fn mkdir(&self, path: &VPath, ctx: &OpCtx) -> VfsResult<()> {
        if self.stat(path, ctx)?.is_some() {
            return Err(VfsError::AlreadyExists(path.display()));
        }
        self.add_one(path, EntryKind::Dir, 0o755, &[], ctx)
    }

    fn create_file(&self, path: &VPath, ctx: &OpCtx) -> VfsResult<()> {
        if self.stat(path, ctx)?.is_some() {
            return Err(VfsError::AlreadyExists(path.display()));
        }
        self.add_one(path, EntryKind::File, 0o644, &[], ctx)
    }

    fn rename(&self, _from: &VPath, _to: &VPath, _ctx: &OpCtx) -> VfsResult<()> {
        Err(VfsError::Unsupported("renaming inside archives".into()))
    }

    fn delete(&self, paths: &[VPath], ctx: &OpCtx, progress: &Progress) -> VfsResult<()> {
        let rels = paths.iter().map(|p| self.rel(p)).collect::<VfsResult<Vec<_>>>()?;
        if rels.iter().any(Vec::is_empty) {
            return Err(VfsError::other("Delete the archive itself from the folder that holds it"));
        }
        progress.set_phase("Rewriting archive");
        let result = self.backend.delete(&rels, ctx);
        self.invalidate();
        result?;
        progress.add_items(rels.len() as u64);
        Ok(())
    }

    fn remove_empty_dirs(&self, dirs: &[VPath], ctx: &OpCtx) -> VfsResult<()> {
        let index = self.index(ctx)?;
        // Deepest first: a folder whose children all go goes too.
        let mut gone: HashSet<Vec<u8>> = HashSet::new();
        for dir in dirs {
            let Ok(rel) = self.rel(dir) else { continue };
            if rel.is_empty() || !index.is_dir(&rel) {
                continue;
            }
            if index.children.get(&rel).into_iter().flatten().all(|c| gone.contains(c)) {
                gone.insert(rel);
            }
        }
        // Only the topmost folders need naming.
        let top: Vec<Vec<u8>> = gone
            .iter()
            .filter(|rel| {
                let parent = parent_of(rel);
                parent.is_empty() || !gone.contains(parent)
            })
            .cloned()
            .collect();
        if top.is_empty() {
            return Ok(());
        }
        let result = self.backend.delete(&top, ctx);
        self.invalidate();
        result
    }

    fn copy_within(&self, _items: &[TransferItem], _ctx: &OpCtx, _progress: &Progress) -> VfsResult<()> {
        Err(VfsError::Unsupported("copying inside an archive; copy to a folder first".into()))
    }

    fn move_within(&self, _items: &[TransferItem], _ctx: &OpCtx, _progress: &Progress) -> VfsResult<()> {
        Err(VfsError::Unsupported("moving inside an archive".into()))
    }

    fn chmod(&self, _paths: &[VPath], _mode: u32, _recursive: bool, _ctx: &OpCtx) -> VfsResult<()> {
        Err(VfsError::Unsupported("changing permissions inside archives".into()))
    }

    fn symlink(&self, _target: &[u8], _link: &VPath, _ctx: &OpCtx) -> VfsResult<()> {
        Err(VfsError::Unsupported("creating links inside archives".into()))
    }

    fn read_file(&self, path: &VPath, limit: u64, ctx: &OpCtx) -> VfsResult<Vec<u8>> {
        let rel = self.rel(path)?;
        match self.index(ctx)?.entries.get(&rel) {
            Some(e) if e.kind == EntryKind::File => {
                let data = self.backend.read_file(&rel, limit, ctx)?;
                if (data.len() as u64) < e.size.min(limit) {
                    return Err(VfsError::other(format!("Cannot read {} from the archive", path.file_name_lossy())));
                }
                Ok(data)
            }
            Some(e) if e.is_dir() => Err(VfsError::other(format!("Is a folder: {path}"))),
            Some(_) => Err(VfsError::other(format!("Not a regular file: {path}"))),
            None => Err(VfsError::NotFound(path.display())),
        }
    }

    fn write_file(&self, path: &VPath, data: &[u8], ctx: &OpCtx) -> VfsResult<()> {
        let mode = match self.stat(path, ctx)? {
            Some(e) if e.kind == EntryKind::File => e.mode,
            Some(_) => return Err(VfsError::other(format!("Not a regular file: {path}"))),
            None => 0o644,
        };
        self.add_one(path, EntryKind::File, mode, data, ctx)
    }

    fn export_tar(&self, base: &VPath, names: &[Vec<u8>], ctx: &OpCtx) -> VfsResult<Box<dyn TarSource>> {
        let rel = self.rel(base)?;
        // The backend needs a current listing (stored names).
        self.index(ctx)?;
        self.backend.export_tar(&rel, names, ctx)
    }

    fn import_tar(&self, dir: &VPath, ctx: &OpCtx) -> VfsResult<Box<dyn TarSink>> {
        let rel = self.rel(dir)?;
        self.index(ctx)?;
        let inner = self.backend.import_tar(&rel, ctx)?;
        Ok(Box::new(RefreshingSink {
            inner,
            index: self.index.clone(),
        }))
    }

    fn free_space(&self, _path: &VPath, _ctx: &OpCtx) -> VfsResult<Option<SpaceInfo>> {
        Ok(None)
    }

    fn prepare_elevation(&self, ctx: &OpCtx) -> VfsResult<()> {
        self.host.prepare_elevation(ctx)
    }

    fn disconnect(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_paths() {
        assert_eq!(ArchiveFormat::detect(b"site.TAR.GZ"), Some(ArchiveFormat::TarGz));
        assert_eq!(ArchiveFormat::detect(b"a.tgz"), Some(ArchiveFormat::TarGz));
        assert_eq!(ArchiveFormat::detect(b"a.tar"), Some(ArchiveFormat::Tar));
        assert_eq!(ArchiveFormat::detect(b"a.zip"), Some(ArchiveFormat::Zip));
        assert_eq!(ArchiveFormat::detect(b"a.gz"), None);
        assert_eq!(normalize_member(b"./a//b/"), Some(b"a/b".to_vec()));
        assert_eq!(normalize_member(b"/etc/passwd"), Some(b"etc/passwd".to_vec()));
        assert_eq!(normalize_member(b"./"), None);
        assert_eq!(normalize_member(b"a/../../x"), None);
        assert_eq!(member_below(b"a/b/c", b"a"), Some(&b"b/c"[..]));
        assert_eq!(member_below(b"a/b", b"a/b"), Some(&b""[..]));
        assert_eq!(member_below(b"ab/c", b"a"), None);
        assert_eq!(member_below(b"x", b""), Some(&b"x"[..]));
        assert_eq!(member_join(b"", b"x"), b"x");
        assert_eq!(member_join(b"a", b"x"), b"a/x");
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(ArchiveFormat::Zip.empty_archive().len(), 22);
    }

    fn member(raw: &str, kind: EntryKind, size: u64) -> ArchiveMember {
        ArchiveMember {
            size,
            ..ArchiveMember::new(raw, kind)
        }
    }

    #[test]
    fn index_builds_a_tree() {
        let mut link = member("./www/latest", EntryKind::Symlink, 0);
        link.link_target = Some(b"v2".to_vec());
        let index = Index::build(
            vec![
                member("./www/", EntryKind::Dir, 0),
                member("./www/v2/index.html", EntryKind::File, 10),
                member("./www/v2/../../evil", EntryKind::File, 1),
                member("README", EntryKind::File, 5),
                member("README", EntryKind::File, 6),
                link,
            ],
            (0, None),
        );
        let mut root: Vec<&Vec<u8>> = index.children[&Vec::new()].iter().collect();
        root.sort();
        assert_eq!(root, [&b"README".to_vec(), &b"www".to_vec()]);
        assert_eq!(index.entries[&b"README".to_vec()].size, 6, "later member wins");
        assert!(index.is_dir(b"www/v2"), "implicit folder");
        assert_eq!(index.children[&b"www".to_vec()].len(), 2);
        assert!(!index.entries.contains_key(b"evil".as_slice()));
        assert_eq!(index.entries[&b"www/latest".to_vec()].link_target.as_deref(), Some(&b"v2"[..]));
        let mut all: Vec<&Vec<u8>> = index.descendants(b"www");
        all.sort();
        assert_eq!(all, [&b"www/latest".to_vec(), &b"www/v2".to_vec(), &b"www/v2/index.html".to_vec()]);
    }
}
