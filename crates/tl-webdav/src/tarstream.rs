//! Tar streams to and from a WebDAV server.
//!
//! The transfer engine moves data between unlike locations as a tar
//! stream. WebDAV has no tar, so one is built here from GETs and taken
//! apart into PUTs, both streamed: a file never sits in memory whole.

use std::io::{self, PipeReader, PipeWriter, Read, Write};
use std::thread::JoinHandle;

use tl_core::{CancelToken, OpCtx, TarSink, TarSource, VPath, Vfs, VfsError, VfsResult};

use crate::client::{Dav, DavBody, DavTarget};

/// Below this, a file goes up from memory; above it, across a pipe. Most
/// files in a tree are small, and a buffer costs less than the thread a
/// streamed body needs.
const IN_MEMORY: u64 = 8 * 1024 * 1024;
use crate::vfs::DavVfs;

/// What the helper threads need: a connection of their own and the paths.
struct Worker {
    dav: Dav,
}

impl Worker {
    fn new(target: &DavTarget) -> VfsResult<Self> {
        // A separate connection, because the thread outlives the call that
        // started it.
        Ok(Worker {
            dav: Dav::connect(target)?,
        })
    }
}

fn join(worker: Option<JoinHandle<VfsResult<()>>>) -> VfsResult<()> {
    match worker {
        Some(handle) => handle
            .join()
            .unwrap_or_else(|_| Err(VfsError::other("the WebDAV tar worker panicked"))),
        None => Ok(()),
    }
}

fn pipe_err(err: io::Error, cancel: &CancelToken) -> VfsError {
    if cancel.is_cancelled() {
        VfsError::Cancelled
    } else {
        VfsError::other(format!("WebDAV transfer failed: {err}"))
    }
}

// ---------------------------------------------------------------- export

pub struct DavTarSource {
    reader: Option<PipeReader>,
    worker: Option<JoinHandle<VfsResult<()>>>,
}

impl Read for DavTarSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.as_mut().map_or(Ok(0), |r| r.read(buf))
    }
}

impl TarSource for DavTarSource {
    fn finish(mut self: Box<Self>) -> VfsResult<()> {
        drop(self.reader.take());
        join(self.worker.take())
    }
}

/// One archive member: where it lives on the server and what it is.
struct Member {
    path: String,
    url: String,
    size: u64,
    mtime: Option<i64>,
    is_dir: bool,
}

pub fn export(
    vfs: &DavVfs,
    base: &VPath,
    names: &[Vec<u8>],
    ctx: &OpCtx,
) -> VfsResult<Box<dyn TarSource>> {
    // The plan is built here, so a missing path fails before any streaming.
    let mut members = Vec::new();
    for name in names {
        let root = base.child(name);
        let top = String::from_utf8_lossy(name).into_owned();
        let entry = vfs
            .stat(&root, ctx)?
            .ok_or_else(|| VfsError::NotFound(root.display()))?;
        if entry.is_dir() {
            members.push(Member {
                path: format!("./{top}"),
                url: String::new(),
                size: 0,
                mtime: entry.mtime,
                is_dir: true,
            });
            for below in vfs.walk(&root, ctx)? {
                let rel = String::from_utf8_lossy(&below.rel).into_owned();
                let path = root.join(&below.rel);
                members.push(Member {
                    path: format!("./{top}/{rel}"),
                    url: vfs.dav.url(&path),
                    size: below.size,
                    mtime: below.mtime,
                    is_dir: below.kind == tl_core::EntryKind::Dir,
                });
            }
        } else {
            members.push(Member {
                path: format!("./{top}"),
                url: vfs.dav.url(&root),
                size: entry.size,
                mtime: entry.mtime,
                is_dir: false,
            });
        }
    }

    let (reader, writer) = io::pipe()?;
    let worker = Worker::new(&vfs.target)?;
    let cancel = ctx.cancel.clone();
    let handle = std::thread::Builder::new()
        .name("webdav-tar-pack".into())
        .spawn(move || pack(worker, members, writer, cancel))?;
    Ok(Box::new(DavTarSource {
        reader: Some(reader),
        worker: Some(handle),
    }))
}

fn pack(
    worker: Worker,
    members: Vec<Member>,
    writer: PipeWriter,
    cancel: CancelToken,
) -> VfsResult<()> {
    let mut builder = tar::Builder::new(writer);
    for member in members {
        cancel.check()?;
        let mut header = tar::Header::new_gnu();
        header.set_mtime(member.mtime.unwrap_or(0).max(0) as u64);
        if member.is_dir {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_mode(0o755);
            header.set_size(0);
            builder
                .append_data(&mut header, &member.path, io::empty())
                .map_err(|e| pipe_err(e, &cancel))?;
            continue;
        }
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(member.size);
        let response = worker.dav.send_ok(
            "GET",
            &member.url,
            &[],
            DavBody::None,
            &format!("Cannot read {}", member.path),
        )?;
        // tar needs exactly `size` bytes; a file that changed under us
        // would otherwise corrupt the stream.
        builder
            .append_data(&mut header, &member.path, response.take(member.size))
            .map_err(|e| pipe_err(e, &cancel))?;
    }
    builder.finish().map_err(|e| pipe_err(e, &cancel))
}

// ---------------------------------------------------------------- import

pub struct DavTarSink {
    writer: Option<PipeWriter>,
    worker: Option<JoinHandle<VfsResult<()>>>,
}

impl Write for DavTarSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.writer.as_mut() {
            Some(writer) => writer.write(buf),
            None => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.as_mut().map_or(Ok(()), |w| w.flush())
    }
}

impl TarSink for DavTarSink {
    fn finish(mut self: Box<Self>) -> VfsResult<()> {
        drop(self.writer.take());
        join(self.worker.take())
    }
}

pub fn import(vfs: &DavVfs, dir: &VPath, ctx: &OpCtx) -> VfsResult<Box<dyn TarSink>> {
    let (reader, writer) = io::pipe()?;
    let worker = Worker::new(&vfs.target)?;
    let base = dir.clone();
    let cancel = ctx.cancel.clone();
    let handle = std::thread::Builder::new()
        .name("webdav-tar-unpack".into())
        .spawn(move || unpack(worker, base, reader, cancel))?;
    Ok(Box::new(DavTarSink {
        writer: Some(writer),
        worker: Some(handle),
    }))
}

fn unpack(
    worker: Worker,
    base: VPath,
    reader: PipeReader,
    cancel: CancelToken,
) -> VfsResult<()> {
    let mut archive = tar::Archive::new(reader);
    // WebDAV stores files and folders; a link or a device has nowhere to
    // go, and dropping one silently would lose it on a move.
    let mut refused: Vec<String> = Vec::new();
    let entries = archive
        .entries()
        .map_err(|e| VfsError::other(format!("WebDAV transfer failed: {e}")))?;
    for entry in entries {
        cancel.check()?;
        let mut entry = entry.map_err(|e| pipe_err(e, &cancel))?;
        let rel = strip_dot(&entry.path_bytes());
        if rel.is_empty() {
            continue;
        }
        let path = base.join(&rel);
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                mkcol(&worker, &path)?;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                // The parent may not have its own entry in the archive.
                if let Some(parent) = path.parent()
                    && parent != base
                {
                    mkcol(&worker, &parent)?;
                }
                let size = entry.size();
                let url = worker.dav.url(&path);
                let what = format!("Cannot write {path}");
                if size <= IN_MEMORY {
                    let mut buf = Vec::with_capacity(size as usize);
                    entry
                        .by_ref()
                        .take(size)
                        .read_to_end(&mut buf)
                        .map_err(|e| pipe_err(e, &cancel))?;
                    worker.dav.send_ok("PUT", &url, &[], &buf[..], &what)?;
                } else {
                    worker.dav.put_stream(&url, &mut entry, size, &what)?;
                }
            }
            _ => refused.push(String::from_utf8_lossy(&rel).into_owned()),
        }
    }
    let mut rest = archive.into_inner();
    io::copy(&mut rest, &mut io::sink())?;
    if refused.is_empty() {
        return Ok(());
    }
    let shown: Vec<&str> = refused.iter().take(3).map(String::as_str).collect();
    Err(VfsError::Unsupported(format!(
        "WebDAV stores only files and folders, so {} could not be copied: {}{}",
        refused.len(),
        shown.join(", "),
        if refused.len() > shown.len() { ", …" } else { "" }
    )))
}

/// Creating a folder that is already there is not a failure here.
fn mkcol(worker: &Worker, path: &VPath) -> VfsResult<()> {
    let response = worker
        .dav
        .send("MKCOL", &worker.dav.dir_url(path), &[], DavBody::None)?;
    let status = response.status().as_u16();
    if (200..300).contains(&status) || status == 405 {
        Ok(())
    } else {
        Err(crate::client::status_error(
            status,
            &format!("Cannot create {path}"),
        ))
    }
}

/// Archive members are named `./name/...`; paths are not.
fn strip_dot(raw: &[u8]) -> Vec<u8> {
    let mut rel = raw;
    while let Some(rest) = rel.strip_prefix(b"./") {
        rel = rest;
    }
    let rel = rel.strip_prefix(b"/").unwrap_or(rel);
    rel.strip_suffix(b"/").unwrap_or(rel).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_names_lose_their_prefix() {
        assert_eq!(strip_dot(b"./proj/src/main.rs"), b"proj/src/main.rs");
        assert_eq!(strip_dot(b"proj/"), b"proj");
        assert_eq!(strip_dot(b"./"), b"");
    }
}
