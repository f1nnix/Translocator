//! [`Vfs`] over an S3 bucket list.
//!
//! The root is the bucket list, so one connection can reach every bucket an
//! account has — which also means a copy between two buckets is a
//! server-side `CopyObject` rather than a download and an upload.

use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{
    BucketLocationConstraint, CreateBucketConfiguration, Delete, ObjectIdentifier,
};
use tl_core::{
    Capabilities, Entry, EntryKind, MergePolicy, OpCtx, Progress, Resolution, ScanTotals,
    SpaceInfo, TarSink, TarSource, TransferItem, VPath, Vfs, VfsError, VfsResult, WalkEntry,
};

use crate::client::{Blocking, S3Target, map_err};
use crate::keys;

/// Objects are shown with these permissions: S3 has no mode bits, and a row
/// of dashes reads as broken rather than as "not applicable".
const FILE_MODE: u32 = 0o644;
const DIR_MODE: u32 = 0o755;

/// One object from a listing.
#[derive(Clone, Debug)]
pub(crate) struct ObjectInfo {
    pub key: String,
    pub size: u64,
    pub mtime: Option<i64>,
}

impl ObjectInfo {
    /// A zero-byte object whose key ends in `/`: the marker that keeps an
    /// otherwise empty folder visible.
    fn is_marker(&self) -> bool {
        self.key.ends_with('/')
    }
}

pub struct S3Vfs {
    pub(crate) id: String,
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) target: S3Target,
    pub(crate) conn: Blocking,
}

fn file_entry(name: &str, size: u64, mtime: Option<i64>) -> Entry {
    let mut entry = Entry::new(name.as_bytes().to_vec(), EntryKind::File);
    entry.size = size;
    entry.mtime = mtime;
    entry.mode = FILE_MODE;
    entry
}

fn dir_entry(name: &str, mtime: Option<i64>) -> Entry {
    let mut entry = Entry::new(name.as_bytes().to_vec(), EntryKind::Dir);
    entry.mtime = mtime;
    entry.mode = DIR_MODE;
    entry
}

impl S3Vfs {
    fn split(&self, path: &VPath) -> VfsResult<Option<(String, String)>> {
        keys::split(path)
    }

    /// Buckets, as the folders of the root.
    fn list_buckets(&self) -> VfsResult<Vec<Entry>> {
        let out = self
            .conn
            .block(self.conn.client.list_buckets().send())
            .map_err(|e| map_err("Cannot list buckets", e))?;
        Ok(out
            .buckets()
            .iter()
            .filter_map(|b| {
                let name = b.name()?;
                Some(dir_entry(name, b.creation_date().map(|d| d.secs())))
            })
            .collect())
    }

    /// One level of a bucket: common prefixes become folders, objects
    /// become files.
    fn list_level(&self, bucket: &str, key: &str, ctx: &OpCtx) -> VfsResult<Vec<Entry>> {
        let prefix = keys::dir_prefix(key);
        let mut entries = Vec::new();
        let mut token: Option<String> = None;
        loop {
            ctx.cancel.check()?;
            let out = self
                .conn
                .block(
                    self.conn
                        .client
                        .list_objects_v2()
                        .bucket(bucket)
                        .prefix(&prefix)
                        .delimiter("/")
                        .set_continuation_token(token.clone())
                        .send(),
                )
                .map_err(|e| map_err(&format!("Cannot list {bucket}/{prefix}"), e))?;
            for common in out.common_prefixes() {
                if let Some(p) = common.prefix() {
                    entries.push(dir_entry(keys::base_name(p), None));
                }
            }
            for object in out.contents() {
                let Some(object_key) = object.key() else { continue };
                // The folder's own marker is not one of its children.
                if object_key == prefix {
                    continue;
                }
                entries.push(file_entry(
                    keys::base_name(object_key),
                    object.size().unwrap_or(0).max(0) as u64,
                    object.last_modified().map(|d| d.secs()),
                ));
            }
            token = out.next_continuation_token().map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        Ok(entries)
    }

    /// Every object under a prefix, following continuation tokens.
    pub(crate) fn list_recursive(
        &self,
        bucket: &str,
        prefix: &str,
        ctx: &OpCtx,
    ) -> VfsResult<Vec<ObjectInfo>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            ctx.cancel.check()?;
            let page = self
                .conn
                .block(
                    self.conn
                        .client
                        .list_objects_v2()
                        .bucket(bucket)
                        .prefix(prefix)
                        .set_continuation_token(token.clone())
                        .send(),
                )
                .map_err(|e| map_err(&format!("Cannot list {bucket}/{prefix}"), e))?;
            for object in page.contents() {
                if let Some(key) = object.key() {
                    out.push(ObjectInfo {
                        key: key.to_string(),
                        size: object.size().unwrap_or(0).max(0) as u64,
                        mtime: object.last_modified().map(|d| d.secs()),
                    });
                }
            }
            token = page.next_continuation_token().map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        Ok(out)
    }

    /// Size and time of one object, or `None` when there is no such key.
    fn head(&self, bucket: &str, key: &str) -> VfsResult<Option<(u64, Option<i64>)>> {
        match self.conn.block(
            self.conn
                .client
                .head_object()
                .bucket(bucket)
                .key(key)
                .send(),
        ) {
            Ok(out) => Ok(Some((
                out.content_length().unwrap_or(0).max(0) as u64,
                out.last_modified().map(|d| d.secs()),
            ))),
            Err(err) if err.as_service_error().is_some_and(|e| e.is_not_found()) => Ok(None),
            Err(err) => Err(map_err(&format!("Cannot read {bucket}/{key}"), err)),
        }
    }

    /// True when anything lives under `key/`, which is what makes a key
    /// prefix look like a folder.
    fn prefix_exists(&self, bucket: &str, key: &str) -> VfsResult<bool> {
        let prefix = keys::dir_prefix(key);
        if prefix.is_empty() {
            return Ok(true);
        }
        let out = self
            .conn
            .block(
                self.conn
                    .client
                    .list_objects_v2()
                    .bucket(bucket)
                    .prefix(&prefix)
                    .max_keys(1)
                    .send(),
            )
            .map_err(|e| map_err(&format!("Cannot list {bucket}/{prefix}"), e))?;
        Ok(!out.contents().is_empty() || !out.common_prefixes().is_empty())
    }

    fn bucket_exists(&self, bucket: &str) -> VfsResult<bool> {
        match self
            .conn
            .block(self.conn.client.head_bucket().bucket(bucket).send())
        {
            Ok(_) => Ok(true),
            Err(err) if err.as_service_error().is_some_and(|e| e.is_not_found()) => Ok(false),
            Err(err) => Err(map_err(&format!("Cannot reach bucket {bucket}"), err)),
        }
    }

    fn exists(&self, bucket: &str, key: &str) -> VfsResult<bool> {
        if key.is_empty() {
            return self.bucket_exists(bucket);
        }
        Ok(self.head(bucket, key)?.is_some() || self.prefix_exists(bucket, key)?)
    }

    pub(crate) fn put_bytes(&self, bucket: &str, key: &str, data: Vec<u8>) -> VfsResult<()> {
        self.conn
            .block(
                self.conn
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

    /// Deletes keys in batches, which is the only affordable way to remove
    /// a folder holding thousands of objects.
    pub(crate) fn delete_keys(
        &self,
        bucket: &str,
        object_keys: &[String],
        ctx: &OpCtx,
        progress: &Progress,
    ) -> VfsResult<()> {
        for chunk in object_keys.chunks(1000) {
            ctx.cancel.check()?;
            let mut ids = Vec::with_capacity(chunk.len());
            for key in chunk {
                ids.push(
                    ObjectIdentifier::builder()
                        .key(key)
                        .build()
                        .map_err(|e| VfsError::other(format!("Cannot delete {key}: {e}")))?,
                );
            }
            let delete = Delete::builder()
                .set_objects(Some(ids))
                .quiet(true)
                .build()
                .map_err(|e| VfsError::other(format!("Cannot delete from {bucket}: {e}")))?;
            let out = self
                .conn
                .block(
                    self.conn
                        .client
                        .delete_objects()
                        .bucket(bucket)
                        .delete(delete)
                        .send(),
                )
                .map_err(|e| map_err(&format!("Cannot delete from {bucket}"), e))?;
            if let Some(failed) = out.errors().first() {
                return Err(VfsError::other(format!(
                    "Cannot delete {}: {}",
                    failed.key().unwrap_or("object"),
                    failed.message().unwrap_or("rejected")
                )));
            }
            progress.add_items(chunk.len() as u64);
        }
        Ok(())
    }

    pub(crate) fn copy_key(
        &self,
        src_bucket: &str,
        src_key: &str,
        dst_bucket: &str,
        dst_key: &str,
    ) -> VfsResult<()> {
        self.conn
            .block(
                self.conn
                    .client
                    .copy_object()
                    .bucket(dst_bucket)
                    .key(dst_key)
                    .copy_source(keys::copy_source(src_bucket, src_key))
                    .send(),
            )
            .map(drop)
            .map_err(|e| {
                map_err(
                    &format!("Cannot copy {src_bucket}/{src_key} to {dst_bucket}/{dst_key}"),
                    e,
                )
            })
    }

    /// Everything under a path, as objects. A file yields just itself.
    pub(crate) fn objects_under(
        &self,
        bucket: &str,
        key: &str,
        ctx: &OpCtx,
    ) -> VfsResult<Vec<ObjectInfo>> {
        if !key.is_empty()
            && let Some((size, mtime)) = self.head(bucket, key)?
        {
            return Ok(vec![ObjectInfo {
                key: key.to_string(),
                size,
                mtime,
            }]);
        }
        self.list_recursive(bucket, &keys::dir_prefix(key), ctx)
    }
}

impl Vfs for S3Vfs {
    fn id(&self) -> &str {
        &self.id
    }

    fn plugin_id(&self) -> &str {
        crate::PLUGIN_ID
    }

    fn label(&self) -> String {
        self.label.clone()
    }

    fn location_key(&self) -> &str {
        &self.key
    }

    fn capabilities(&self) -> Capabilities {
        // S3 has no permissions, no links, no shell and no trash.
        Capabilities {
            elevation: false,
            trash: false,
            exec: false,
            chmod: false,
            chown: false,
            symlink: false,
            case_insensitive: false,
        }
    }

    fn home(&self) -> VPath {
        let mut path = VPath::root();
        if !self.target.bucket.is_empty() {
            path = path.child(self.target.bucket.as_bytes());
            if !self.target.prefix.is_empty() {
                path = path.join(&self.target.prefix);
            }
        }
        path
    }

    fn list(&self, dir: &VPath, ctx: &OpCtx) -> VfsResult<Vec<Entry>> {
        match self.split(dir)? {
            None => self.list_buckets(),
            Some((bucket, key)) => self.list_level(&bucket, &key, ctx),
        }
    }

    fn stat(&self, path: &VPath, _ctx: &OpCtx) -> VfsResult<Option<Entry>> {
        let Some((bucket, key)) = self.split(path)? else {
            return Ok(Some(dir_entry("/", None)));
        };
        if key.is_empty() {
            return Ok(self
                .bucket_exists(&bucket)?
                .then(|| dir_entry(&bucket, None)));
        }
        let name = keys::base_name(&key);
        if let Some((size, mtime)) = self.head(&bucket, &key)? {
            return Ok(Some(file_entry(name, size, mtime)));
        }
        Ok(self
            .prefix_exists(&bucket, &key)?
            .then(|| dir_entry(name, None)))
    }

    fn walk(&self, root: &VPath, ctx: &OpCtx) -> VfsResult<Vec<WalkEntry>> {
        let Some((bucket, key)) = self.split(root)? else {
            // The root's children are buckets, which nothing walks into.
            return Ok(self
                .list_buckets()?
                .into_iter()
                .map(|b| WalkEntry {
                    rel: b.name.clone(),
                    kind: EntryKind::Dir,
                    size: 0,
                    mtime: b.mtime,
                })
                .collect());
        };
        let prefix = keys::dir_prefix(&key);
        let mut out = Vec::new();
        let mut seen_dirs = std::collections::HashSet::new();
        for object in self.list_recursive(&bucket, &prefix, ctx)? {
            let rel = object.key[prefix.len()..].to_string();
            if rel.is_empty() {
                continue;
            }
            // Folders exist only as pieces of a key, so build them up.
            let mut at = 0;
            while let Some(slash) = rel[at..].find('/') {
                let end = at + slash;
                let dir = &rel[..end];
                if !dir.is_empty() && seen_dirs.insert(dir.to_string()) {
                    out.push(WalkEntry {
                        rel: dir.as_bytes().to_vec(),
                        kind: EntryKind::Dir,
                        size: 0,
                        mtime: None,
                    });
                }
                at = end + 1;
            }
            if !object.is_marker() {
                out.push(WalkEntry {
                    rel: rel.into_bytes(),
                    kind: EntryKind::File,
                    size: object.size,
                    mtime: object.mtime,
                });
            }
        }
        Ok(out)
    }

    fn scan(&self, paths: &[VPath], ctx: &OpCtx) -> VfsResult<ScanTotals> {
        let mut totals = ScanTotals::default();
        for path in paths {
            let Some((bucket, key)) = self.split(path)? else {
                continue;
            };
            for object in self.objects_under(&bucket, &key, ctx)? {
                if object.is_marker() {
                    totals.dirs += 1;
                } else {
                    totals.files += 1;
                    totals.bytes += object.size;
                }
            }
        }
        Ok(totals)
    }

    fn mkdir(&self, path: &VPath, _ctx: &OpCtx) -> VfsResult<()> {
        let Some((bucket, key)) = self.split(path)? else {
            return Err(VfsError::other("The root holds buckets, not folders"));
        };
        if key.is_empty() {
            keys::check_bucket_name(&bucket)?;
            if self.bucket_exists(&bucket)? {
                return Err(VfsError::AlreadyExists(format!("Bucket {bucket}")));
            }
            let mut request = self.conn.client.create_bucket().bucket(&bucket);
            // Only us-east-1 must not carry a location constraint.
            if self.target.region != "us-east-1" {
                request = request.create_bucket_configuration(
                    CreateBucketConfiguration::builder()
                        .location_constraint(BucketLocationConstraint::from(
                            self.target.region.as_str(),
                        ))
                        .build(),
                );
            }
            return self
                .conn
                .block(request.send())
                .map(drop)
                .map_err(|e| map_err(&format!("Cannot create bucket {bucket}"), e));
        }
        if self.exists(&bucket, &key)? {
            return Err(VfsError::AlreadyExists(path.display()));
        }
        self.put_bytes(&bucket, &keys::dir_prefix(&key), Vec::new())
    }

    fn create_file(&self, path: &VPath, _ctx: &OpCtx) -> VfsResult<()> {
        let Some((bucket, key)) = self.split(path)? else {
            return Err(VfsError::other("The root holds buckets, not files"));
        };
        if key.is_empty() {
            return Err(VfsError::other("A bucket cannot be a file"));
        }
        if self.exists(&bucket, &key)? {
            return Err(VfsError::AlreadyExists(path.display()));
        }
        self.put_bytes(&bucket, &key, Vec::new())
    }

    fn rename(&self, from: &VPath, to: &VPath, ctx: &OpCtx) -> VfsResult<()> {
        let (Some((src_bucket, src_key)), Some((dst_bucket, dst_key))) =
            (self.split(from)?, self.split(to)?)
        else {
            return Err(VfsError::other("Buckets are renamed by creating a new one"));
        };
        if src_key.is_empty() || dst_key.is_empty() {
            return Err(VfsError::Unsupported(
                "renaming a bucket (S3 has no such operation)".into(),
            ));
        }
        if self.exists(&dst_bucket, &dst_key)? {
            return Err(VfsError::AlreadyExists(to.display()));
        }
        let objects = self.objects_under(&src_bucket, &src_key, ctx)?;
        if objects.is_empty() {
            return Err(VfsError::NotFound(from.display()));
        }
        let src_prefix = keys::dir_prefix(&src_key);
        for object in &objects {
            ctx.cancel.check()?;
            // A single object renames to the new key; everything under a
            // prefix keeps its position below it.
            let target = if object.key == src_key {
                dst_key.clone()
            } else {
                format!("{}{}", keys::dir_prefix(&dst_key), &object.key[src_prefix.len()..])
            };
            self.copy_key(&src_bucket, &object.key, &dst_bucket, &target)?;
        }
        let old: Vec<String> = objects.into_iter().map(|o| o.key).collect();
        self.delete_keys(&src_bucket, &old, ctx, &Progress::new())
    }

    fn delete(&self, paths: &[VPath], ctx: &OpCtx, progress: &Progress) -> VfsResult<()> {
        for path in paths {
            let Some((bucket, key)) = self.split(path)? else {
                return Err(VfsError::other("Refusing to delete every bucket"));
            };
            progress.set_current(path.display());
            let objects = self.objects_under(&bucket, &key, ctx)?;
            let object_keys: Vec<String> = objects.into_iter().map(|o| o.key).collect();
            if !object_keys.is_empty() {
                self.delete_keys(&bucket, &object_keys, ctx, progress)?;
            }
            if key.is_empty() {
                // Emptied above, so the bucket itself can go.
                self.conn
                    .block(self.conn.client.delete_bucket().bucket(&bucket).send())
                    .map(drop)
                    .map_err(|e| map_err(&format!("Cannot delete bucket {bucket}"), e))?;
            }
        }
        Ok(())
    }

    fn remove_empty_dirs(&self, dirs: &[VPath], ctx: &OpCtx) -> VfsResult<()> {
        for dir in dirs {
            let Some((bucket, key)) = self.split(dir)? else {
                continue;
            };
            if key.is_empty() {
                continue;
            }
            let prefix = keys::dir_prefix(&key);
            // Only the marker may remain, or the folder is not empty.
            let left = self.list_recursive(&bucket, &prefix, ctx)?;
            if left.iter().all(|o| o.key == prefix) && !left.is_empty() {
                self.delete_keys(&bucket, &[prefix], ctx, &Progress::new())?;
            }
        }
        Ok(())
    }

    fn chmod(&self, _paths: &[VPath], _mode: u32, _recursive: bool, _ctx: &OpCtx) -> VfsResult<()> {
        Err(VfsError::Unsupported("permissions on S3".into()))
    }

    fn symlink(&self, _target: &[u8], _link: &VPath, _ctx: &OpCtx) -> VfsResult<()> {
        Err(VfsError::Unsupported("symbolic links on S3".into()))
    }

    fn read_file(&self, path: &VPath, limit: u64, _ctx: &OpCtx) -> VfsResult<Vec<u8>> {
        let Some((bucket, key)) = self.split(path)? else {
            return Err(VfsError::other("The root is not a file"));
        };
        let mut request = self.conn.client.get_object().bucket(&bucket).key(&key);
        if limit != u64::MAX && limit > 0 {
            request = request.range(format!("bytes=0-{}", limit - 1));
        }
        let out = self
            .conn
            .block(request.send())
            .map_err(|e| map_err(&format!("Cannot read {path}"), e))?;
        let body = self
            .conn
            .block(out.body.collect())
            .map_err(|e| VfsError::other(format!("Cannot read {path}: {e}")))?;
        Ok(body.into_bytes().to_vec())
    }

    fn write_file(&self, path: &VPath, data: &[u8], _ctx: &OpCtx) -> VfsResult<()> {
        let Some((bucket, key)) = self.split(path)? else {
            return Err(VfsError::other("The root is not a file"));
        };
        if key.is_empty() {
            return Err(VfsError::other("A bucket cannot be a file"));
        }
        self.put_bytes(&bucket, &key, data.to_vec())
    }

    fn free_space(&self, _path: &VPath, _ctx: &OpCtx) -> VfsResult<Option<SpaceInfo>> {
        // Object stores do not report capacity.
        Ok(None)
    }

    fn copy_within(
        &self,
        items: &[TransferItem],
        ctx: &OpCtx,
        progress: &Progress,
    ) -> VfsResult<()> {
        self.transfer(items, ctx, progress, false)
    }

    fn move_within(
        &self,
        items: &[TransferItem],
        ctx: &OpCtx,
        progress: &Progress,
    ) -> VfsResult<()> {
        self.transfer(items, ctx, progress, true)
    }

    fn export_tar(
        &self,
        base: &VPath,
        names: &[Vec<u8>],
        ctx: &OpCtx,
    ) -> VfsResult<Box<dyn TarSource>> {
        crate::tarstream::export(self, base, names, ctx)
    }

    fn import_tar(&self, dir: &VPath, ctx: &OpCtx) -> VfsResult<Box<dyn TarSink>> {
        crate::tarstream::import(self, dir, ctx)
    }

    fn server_info(&self) -> Option<String> {
        Some(format!(
            "S3 · {} · {}",
            self.target
                .endpoint
                .as_deref()
                .unwrap_or("Amazon S3"),
            self.target.region
        ))
    }
}

impl S3Vfs {
    /// Copy or move inside this account. Every byte stays in the service:
    /// objects are duplicated with CopyObject and, for a move, the sources
    /// are deleted afterwards.
    fn transfer(
        &self,
        items: &[TransferItem],
        ctx: &OpCtx,
        progress: &Progress,
        delete_source: bool,
    ) -> VfsResult<()> {
        let mut planned = 0u64;
        for item in items {
            planned += self.objects_under_item(item, ctx)?.len() as u64;
        }
        progress.set_totals(0, planned.max(1));

        for item in items {
            ctx.cancel.check()?;
            let (Some((src_bucket, src_key)), Some((dst_bucket, dst_key))) =
                (self.split(&item.src)?, self.split(&item.dst)?)
            else {
                return Err(VfsError::other("Buckets cannot be copied as a whole"));
            };
            progress.set_current(item.src.display());

            if let Resolution::Replace = item.resolution {
                let existing: Vec<String> = self
                    .objects_under(&dst_bucket, &dst_key, ctx)?
                    .into_iter()
                    .map(|o| o.key)
                    .collect();
                if !existing.is_empty() {
                    self.delete_keys(&dst_bucket, &existing, ctx, &Progress::new())?;
                }
            }

            // What the destination already holds, for a merge.
            let existing = match item.resolution {
                Resolution::Merge(_) => self
                    .objects_under(&dst_bucket, &dst_key, ctx)?
                    .into_iter()
                    .map(|o| (o.key.clone(), o))
                    .collect(),
                _ => std::collections::HashMap::new(),
            };

            let sources = self.objects_under(&src_bucket, &src_key, ctx)?;
            let src_prefix = keys::dir_prefix(&src_key);
            let mut copied = Vec::new();
            for object in &sources {
                ctx.cancel.check()?;
                let target = if object.key == src_key {
                    dst_key.clone()
                } else {
                    format!(
                        "{}{}",
                        keys::dir_prefix(&dst_key),
                        &object.key[src_prefix.len()..]
                    )
                };
                if src_bucket == dst_bucket && object.key == target {
                    continue;
                }
                if let Resolution::Merge(policy) = item.resolution
                    && let Some(there) = existing.get(&target)
                {
                    let replace = match policy {
                        MergePolicy::Overwrite => true,
                        MergePolicy::SkipExisting => false,
                        MergePolicy::OverwriteOlder => match (object.mtime, there.mtime) {
                            (Some(new), Some(old)) => new > old,
                            _ => false,
                        },
                    };
                    if !replace {
                        progress.add_items(1);
                        continue;
                    }
                }
                self.copy_key(&src_bucket, &object.key, &dst_bucket, &target)?;
                copied.push(object.key.clone());
                progress.add_items(1);
            }
            if delete_source && !copied.is_empty() {
                self.delete_keys(&src_bucket, &copied, ctx, &Progress::new())?;
            }
        }
        Ok(())
    }

    /// Proves the endpoint and credentials work while the user is still
    /// looking at the bookmark dialog.
    pub(crate) fn check_reachable(&self) -> VfsResult<()> {
        if self.target.bucket.is_empty() {
            return self.list_buckets().map(drop);
        }
        // A key may be scoped to one bucket and unable to list them all.
        let bucket = self.target.bucket.clone();
        if self.bucket_exists(&bucket)? {
            Ok(())
        } else {
            Err(VfsError::NotFound(format!("Bucket {bucket}")))
        }
    }

    fn objects_under_item(&self, item: &TransferItem, ctx: &OpCtx) -> VfsResult<Vec<ObjectInfo>> {
        match self.split(&item.src)? {
            Some((bucket, key)) => self.objects_under(&bucket, &key, ctx),
            None => Ok(Vec::new()),
        }
    }
}
