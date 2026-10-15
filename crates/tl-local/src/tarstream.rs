//! Tar streams for cross-provider transfers. Local files are packed and
//! unpacked in-process on a helper thread connected through a pipe.

use std::fs;
use std::io::{self, PipeReader, PipeWriter, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

use tl_core::{CancelToken, TarSink, TarSource, VfsError, VfsResult};

use crate::fsops::read_dir_names;

pub struct LocalTarSource {
    reader: Option<PipeReader>,
    worker: Option<JoinHandle<VfsResult<()>>>,
}

impl Read for LocalTarSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.as_mut().map_or(Ok(0), |r| r.read(buf))
    }
}

impl TarSource for LocalTarSource {
    fn finish(mut self: Box<Self>) -> VfsResult<()> {
        // Closing our end unblocks a packer that is still writing.
        drop(self.reader.take());
        join(self.worker.take())
    }
}

pub struct LocalTarSink {
    writer: Option<PipeWriter>,
    worker: Option<JoinHandle<VfsResult<()>>>,
}

impl Write for LocalTarSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.writer.as_mut() {
            Some(w) => w.write(buf),
            None => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.as_mut().map_or(Ok(()), |w| w.flush())
    }
}

impl TarSink for LocalTarSink {
    fn finish(mut self: Box<Self>) -> VfsResult<()> {
        drop(self.writer.take());
        join(self.worker.take())
    }
}

fn join(worker: Option<JoinHandle<VfsResult<()>>>) -> VfsResult<()> {
    match worker {
        Some(handle) => handle
            .join()
            .unwrap_or_else(|_| Err(VfsError::other("tar worker panicked"))),
        None => Ok(()),
    }
}

pub fn export(base: PathBuf, names: Vec<Vec<u8>>, cancel: CancelToken) -> VfsResult<LocalTarSource> {
    let (reader, writer) = io::pipe()?;
    let worker = std::thread::Builder::new()
        .name("tar-pack".into())
        .spawn(move || pack(&base, &names, writer, &cancel))?;
    Ok(LocalTarSource {
        reader: Some(reader),
        worker: Some(worker),
    })
}

fn pack(base: &Path, names: &[Vec<u8>], writer: PipeWriter, cancel: &CancelToken) -> VfsResult<()> {
    let mut builder = tar::Builder::new(writer);
    for name in names {
        let mut rel = b"./".to_vec();
        rel.extend_from_slice(name);
        append(&mut builder, base, &mut rel, cancel)?;
    }
    builder.finish().map_err(|e| pipe_err(e, cancel))
}

fn pipe_err(e: io::Error, cancel: &CancelToken) -> VfsError {
    if cancel.is_cancelled() {
        VfsError::Cancelled
    } else {
        VfsError::other(format!("packing failed: {e}"))
    }
}

fn append(
    builder: &mut tar::Builder<PipeWriter>,
    base: &Path,
    rel: &mut Vec<u8>,
    cancel: &CancelToken,
) -> VfsResult<()> {
    cancel.check()?;
    let rel_path = Path::new(std::ffi::OsStr::from_bytes(rel)).to_path_buf();
    let path = base.join(&rel_path);
    let meta = fs::symlink_metadata(&path).map_err(|e| VfsError::io(e, path.display()))?;
    let ft = meta.file_type();
    let mut header = tar::Header::new_gnu();
    header.set_metadata_in_mode(&meta, tar::HeaderMode::Complete);
    if ft.is_dir() {
        header.set_size(0);
        builder
            .append_data(&mut header, &rel_path, io::empty())
            .map_err(|e| pipe_err(e, cancel))?;
        for child in read_dir_names(&path)? {
            let len = rel.len();
            rel.push(b'/');
            rel.extend_from_slice(child.as_bytes());
            append(builder, base, rel, cancel)?;
            rel.truncate(len);
        }
    } else if ft.is_symlink() {
        let target = fs::read_link(&path).map_err(|e| VfsError::io(e, path.display()))?;
        header.set_size(0);
        builder
            .append_link(&mut header, &rel_path, &target)
            .map_err(|e| pipe_err(e, cancel))?;
    } else if ft.is_file() {
        let file = fs::File::open(&path).map_err(|e| VfsError::io(e, path.display()))?;
        builder
            .append_data(&mut header, &rel_path, file)
            .map_err(|e| pipe_err(e, cancel))?;
    } else {
        use std::os::unix::fs::FileTypeExt;
        if ft.is_fifo() || ft.is_char_device() || ft.is_block_device() {
            header.set_size(0);
            builder
                .append_data(&mut header, &rel_path, io::empty())
                .map_err(|e| pipe_err(e, cancel))?;
        } else {
            log::warn!("not archiving socket {}", path.display());
        }
    }
    Ok(())
}

pub fn import(dir: PathBuf, cancel: CancelToken) -> VfsResult<LocalTarSink> {
    let (reader, writer) = io::pipe()?;
    let worker = std::thread::Builder::new()
        .name("tar-unpack".into())
        .spawn(move || unpack(&dir, reader, &cancel))?;
    Ok(LocalTarSink {
        writer: Some(writer),
        worker: Some(worker),
    })
}

fn unpack(dir: &Path, reader: PipeReader, cancel: &CancelToken) -> VfsResult<()> {
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(true);
    archive.set_overwrite(true);
    archive.set_unpack_xattrs(false);
    let entries = archive
        .entries()
        .map_err(|e| VfsError::other(format!("unpacking failed: {e}")))?;
    for entry in entries {
        cancel.check()?;
        let mut entry = entry.map_err(|e| VfsError::other(format!("unpacking failed: {e}")))?;
        let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        entry
            .unpack_in(dir)
            .map_err(|e| VfsError::io(e, dir.join(name.trim_start_matches("./")).display()))?;
    }
    // Drain the trailer so the writer never sees a broken pipe.
    let mut rest = archive.into_inner();
    io::copy(&mut rest, &mut io::sink())?;
    Ok(())
}
