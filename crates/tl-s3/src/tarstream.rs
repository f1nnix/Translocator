//! Tar streams to and from S3.
//!
//! The transfer engine moves data between unlike locations as a tar stream.
//! S3 has no tar, so one is built and taken apart here: a helper thread
//! packs objects into a pipe, or reads a pipe and uploads what it finds.

use std::io::{self, PipeReader, PipeWriter, Read, Write};
use std::thread::JoinHandle;

use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use tl_core::{CancelToken, OpCtx, TarSink, TarSource, VPath, VfsError, VfsResult};

use crate::client::{Worker, map_err};
use crate::keys;
use crate::vfs::S3Vfs;

/// Objects at or above this size are uploaded in parts, so memory stays
/// bounded however large a file is. S3 requires every part except the last
/// to be at least 5 MiB. Both are overridable so the tests can exercise a
/// real multipart upload without moving 64 MB.
fn multipart_threshold() -> u64 {
    static VALUE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| env_size("TL_S3_MULTIPART_THRESHOLD", 64 * 1024 * 1024))
}

fn part_size() -> usize {
    static VALUE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| env_size("TL_S3_PART_SIZE", 16 * 1024 * 1024).max(5 * 1024 * 1024) as usize)
}

fn env_size(name: &str, fallback: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(fallback)
}

fn join(worker: Option<JoinHandle<VfsResult<()>>>) -> VfsResult<()> {
    match worker {
        Some(handle) => handle
            .join()
            .unwrap_or_else(|_| Err(VfsError::other("the S3 tar worker panicked"))),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------- export

pub struct S3TarSource {
    reader: Option<PipeReader>,
    worker: Option<JoinHandle<VfsResult<()>>>,
}

impl Read for S3TarSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.as_mut().map_or(Ok(0), |r| r.read(buf))
    }
}

impl TarSource for S3TarSource {
    fn finish(mut self: Box<Self>) -> VfsResult<()> {
        // Closing this end lets a still-writing packer stop.
        drop(self.reader.take());
        join(self.worker.take())
    }
}

/// What to put in the archive: the member path and the object behind it.
struct Member {
    path: String,
    key: String,
    size: u64,
    mtime: Option<i64>,
    is_dir: bool,
}

pub fn export(
    vfs: &S3Vfs,
    base: &VPath,
    names: &[Vec<u8>],
    ctx: &OpCtx,
) -> VfsResult<Box<dyn TarSource>> {
    let Some((bucket, base_key)) = keys::split(base)? else {
        return Err(VfsError::other("Buckets cannot be copied as a whole"));
    };
    // The plan is built here so a bad path fails before any streaming.
    let base_prefix = keys::dir_prefix(&base_key);
    let mut members = Vec::new();
    for name in names {
        let name = std::str::from_utf8(name)
            .map_err(|_| VfsError::other("S3 names must be valid UTF-8"))?;
        let key = format!("{base_prefix}{name}");
        for object in vfs.objects_under(&bucket, &key, ctx)? {
            let rel = object.key.strip_prefix(&base_prefix).unwrap_or(&object.key);
            members.push(Member {
                path: format!("./{}", rel.trim_end_matches('/')),
                key: object.key.clone(),
                size: object.size,
                mtime: object.mtime,
                is_dir: object.key.ends_with('/'),
            });
        }
    }

    let (reader, writer) = io::pipe()?;
    let worker = vfs.conn.handle();
    let cancel = ctx.cancel.clone();
    let handle = std::thread::Builder::new()
        .name("s3-tar-pack".into())
        .spawn(move || pack(worker, bucket, members, writer, cancel))?;
    Ok(Box::new(S3TarSource {
        reader: Some(reader),
        worker: Some(handle),
    }))
}

fn pack(
    worker: Worker,
    bucket: String,
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
        let out = worker
            .block(
                worker
                    .client
                    .get_object()
                    .bucket(&bucket)
                    .key(&member.key)
                    .send(),
            )
            .map_err(|e| map_err(&format!("Cannot read {bucket}/{}", member.key), e))?;
        let body = BlockingBody {
            worker: worker.clone(),
            stream: out.body,
            buf: Vec::new(),
            at: 0,
            done: false,
            cancel: cancel.clone(),
        };
        builder
            .append_data(&mut header, &member.path, body)
            .map_err(|e| pipe_err(e, &cancel))?;
    }
    builder.finish().map_err(|e| pipe_err(e, &cancel))
}

fn pipe_err(err: io::Error, cancel: &CancelToken) -> VfsError {
    if cancel.is_cancelled() {
        VfsError::Cancelled
    } else {
        VfsError::other(format!("S3 transfer failed: {err}"))
    }
}

/// Reads an object's body from a plain thread, pulling chunks through the
/// connection's runtime.
struct BlockingBody {
    worker: Worker,
    stream: ByteStream,
    buf: Vec<u8>,
    at: usize,
    done: bool,
    cancel: CancelToken,
}

impl Read for BlockingBody {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.at >= self.buf.len() {
            if self.done {
                return Ok(0);
            }
            if self.cancel.is_cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            match self.worker.block(self.stream.next()) {
                Some(Ok(chunk)) => {
                    self.buf = chunk.to_vec();
                    self.at = 0;
                }
                Some(Err(err)) => return Err(io::Error::other(err)),
                None => self.done = true,
            }
        }
        let n = out.len().min(self.buf.len() - self.at);
        out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

// ---------------------------------------------------------------- import

pub struct S3TarSink {
    writer: Option<PipeWriter>,
    worker: Option<JoinHandle<VfsResult<()>>>,
}

impl Write for S3TarSink {
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

impl TarSink for S3TarSink {
    fn finish(mut self: Box<Self>) -> VfsResult<()> {
        drop(self.writer.take());
        join(self.worker.take())
    }
}

pub fn import(vfs: &S3Vfs, dir: &VPath, ctx: &OpCtx) -> VfsResult<Box<dyn TarSink>> {
    let Some((bucket, key)) = keys::split(dir)? else {
        return Err(VfsError::other(
            "Copy into a bucket, not into the bucket list",
        ));
    };
    let (reader, writer) = io::pipe()?;
    let worker = vfs.conn.handle();
    let cancel = ctx.cancel.clone();
    let prefix = keys::dir_prefix(&key);
    let handle = std::thread::Builder::new()
        .name("s3-tar-unpack".into())
        .spawn(move || unpack(worker, bucket, prefix, reader, cancel))?;
    Ok(Box::new(S3TarSink {
        writer: Some(writer),
        worker: Some(handle),
    }))
}

fn unpack(
    worker: Worker,
    bucket: String,
    prefix: String,
    reader: PipeReader,
    cancel: CancelToken,
) -> VfsResult<()> {
    let mut archive = tar::Archive::new(reader);
    // S3 keeps bytes, not links or devices; refusing them is better than
    // dropping them silently, because a move deletes the source.
    let mut refused: Vec<String> = Vec::new();
    let entries = archive
        .entries()
        .map_err(|e| VfsError::other(format!("S3 transfer failed: {e}")))?;
    for entry in entries {
        cancel.check()?;
        let mut entry = entry.map_err(|e| pipe_err(e, &cancel))?;
        let raw = entry.path_bytes().into_owned();
        let rel = String::from_utf8(strip_dot(&raw))
            .map_err(|_| VfsError::other("S3 keys must be valid UTF-8"))?;
        if rel.is_empty() {
            continue;
        }
        let kind = entry.header().entry_type();
        let key = format!("{prefix}{rel}");
        match kind {
            tar::EntryType::Directory => {
                put(&worker, &bucket, &format!("{}/", key.trim_end_matches('/')), Vec::new())?;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let size = entry.size();
                if size >= multipart_threshold() {
                    upload_parts(&worker, &bucket, &key, &mut entry, &cancel)?;
                } else {
                    let mut data = Vec::with_capacity(size as usize);
                    entry.read_to_end(&mut data).map_err(|e| pipe_err(e, &cancel))?;
                    put(&worker, &bucket, &key, data)?;
                }
            }
            _ => refused.push(rel),
        }
    }
    // Drain anything after the last member so the writer never sees a
    // broken pipe while the engine is still finishing its archive.
    let mut rest = archive.into_inner();
    io::copy(&mut rest, &mut io::sink())?;
    if refused.is_empty() {
        return Ok(());
    }
    let shown: Vec<&str> = refused.iter().take(3).map(String::as_str).collect();
    Err(VfsError::Unsupported(format!(
        "S3 stores only files and folders, so {} could not be copied: {}{}",
        refused.len(),
        shown.join(", "),
        if refused.len() > shown.len() { ", …" } else { "" }
    )))
}

/// Archive members are named `./name/...`; S3 keys are not.
fn strip_dot(raw: &[u8]) -> Vec<u8> {
    let mut rel = raw;
    while let Some(rest) = rel.strip_prefix(b"./") {
        rel = rest;
    }
    rel.strip_prefix(b"/").unwrap_or(rel).to_vec()
}

fn put(worker: &Worker, bucket: &str, key: &str, data: Vec<u8>) -> VfsResult<()> {
    worker
        .block(
            worker
                .client
                .put_object()
                .bucket(bucket)
                .key(key)
                .body(ByteStream::from(data))
                .send(),
        )
        .map(drop)
        .map_err(|e| map_err(&format!("Cannot write {bucket}/{key}"), e))
}

/// Uploads one large member in parts, holding only a part in memory.
fn upload_parts(
    worker: &Worker,
    bucket: &str,
    key: &str,
    body: &mut dyn Read,
    cancel: &CancelToken,
) -> VfsResult<()> {
    let started = worker
        .block(
            worker
                .client
                .create_multipart_upload()
                .bucket(bucket)
                .key(key)
                .send(),
        )
        .map_err(|e| map_err(&format!("Cannot start upload of {bucket}/{key}"), e))?;
    let Some(upload_id) = started.upload_id().map(str::to_string) else {
        return Err(VfsError::other(format!(
            "Cannot start upload of {bucket}/{key}: no upload id"
        )));
    };

    let result = (|| -> VfsResult<Vec<CompletedPart>> {
        let mut parts = Vec::new();
        let mut number = 1;
        loop {
            cancel.check()?;
            let limit = part_size();
            let mut chunk = vec![0u8; limit];
            let mut filled = 0;
            while filled < limit {
                match body.read(&mut chunk[filled..]) {
                    Ok(0) => break,
                    Ok(n) => filled += n,
                    Err(err) => return Err(pipe_err(err, cancel)),
                }
            }
            if filled == 0 {
                break;
            }
            chunk.truncate(filled);
            let out = worker
                .block(
                    worker
                        .client
                        .upload_part()
                        .bucket(bucket)
                        .key(key)
                        .upload_id(&upload_id)
                        .part_number(number)
                        .body(ByteStream::from(chunk))
                        .send(),
                )
                .map_err(|e| map_err(&format!("Cannot upload part {number} of {key}"), e))?;
            parts.push(
                CompletedPart::builder()
                    .set_e_tag(out.e_tag().map(str::to_string))
                    .part_number(number)
                    .build(),
            );
            number += 1;
            if filled < limit {
                break;
            }
        }
        Ok(parts)
    })();

    let parts = match result {
        Ok(parts) => parts,
        Err(err) => {
            // Unfinished uploads keep costing money, so clean up.
            let _ = worker.block(
                worker
                    .client
                    .abort_multipart_upload()
                    .bucket(bucket)
                    .key(key)
                    .upload_id(&upload_id)
                    .send(),
            );
            return Err(err);
        }
    };

    worker
        .block(
            worker
                .client
                .complete_multipart_upload()
                .bucket(bucket)
                .key(key)
                .upload_id(&upload_id)
                .multipart_upload(
                    CompletedMultipartUpload::builder()
                        .set_parts(Some(parts))
                        .build(),
                )
                .send(),
        )
        .map(drop)
        .map_err(|e| map_err(&format!("Cannot finish upload of {bucket}/{key}"), e))
}
