//! [`Vfs`] over a WebDAV server.
//!
//! WebDAV is the closest of the remote providers to a real file system: it
//! has collections, and COPY and MOVE run on the server, so a transfer
//! inside one server never passes through this machine.

use std::collections::HashMap;
use std::io::Read;

use tl_core::{
    Capabilities, Entry, EntryKind, MergePolicy, OpCtx, Progress, Resolution, ScanTotals,
    SpaceInfo, TarSink, TarSource, TransferItem, VPath, Vfs, VfsError, VfsResult, WalkEntry,
};
use crate::client::{Dav, DavBody, DavTarget, status_error};
use crate::propfind::{self, DavEntry};

/// WebDAV carries no mode bits, so entries are shown with the permissions
/// a reasonable server would give them; a row of dashes reads as broken.
const FILE_MODE: u32 = 0o644;
const DIR_MODE: u32 = 0o755;

pub struct DavVfs {
    pub(crate) id: String,
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) target: DavTarget,
    pub(crate) dav: Dav,
    /// The path part of the base URL, which every href starts with.
    pub(crate) base_path: Vec<u8>,
    /// Whether `Depth: infinity` worked; servers that refuse it get a
    /// folder-by-folder walk instead.
    pub(crate) deep_walk: std::sync::atomic::AtomicBool,
}

fn entry_from(dav: &DavEntry, name: &[u8]) -> Entry {
    let mut entry = Entry::new(
        name.to_vec(),
        if dav.is_dir { EntryKind::Dir } else { EntryKind::File },
    );
    entry.size = if dav.is_dir { 0 } else { dav.size };
    entry.mtime = dav.mtime;
    entry.mode = if dav.is_dir { DIR_MODE } else { FILE_MODE };
    entry
}

fn last_segment(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|b| *b == b'/') {
        Some(at) => &path[at + 1..],
        None => path,
    }
}

impl DavVfs {
    /// The server path for a pane path: the base URL's own path, then the
    /// pane path below it.
    fn server_path(&self, path: &VPath) -> Vec<u8> {
        let mut out = self.base_path.clone();
        if !path.is_root() {
            out.extend_from_slice(path.as_bytes());
        }
        out
    }

    fn propfind(&self, url: &str, depth: &str, body: &str, what: &str) -> VfsResult<Vec<DavEntry>> {
        let response = self.dav.send(
            "PROPFIND",
            url,
            &[("Depth", depth), ("Content-Type", "application/xml; charset=utf-8")],
            body,
        )?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(status_error(status, what));
        }
        let xml = response
            .bytes()
            .map_err(|e| VfsError::other(format!("{what}: {e}")))?;
        propfind::parse_multistatus(&xml)
    }

    /// Everything below `root`, as paths relative to it.
    fn walk_entries(&self, root: &VPath, ctx: &OpCtx) -> VfsResult<Vec<DavEntry>> {
        use std::sync::atomic::Ordering;
        let root_path = self.server_path(root);
        if self.deep_walk.load(Ordering::Relaxed) {
            match self.propfind(
                &self.dav.dir_url(root),
                "infinity",
                propfind::PROPS,
                &format!("Cannot list {root}"),
            ) {
                Ok(found) => return Ok(strip_root(found, &root_path)),
                Err(err) => {
                    // Apache refuses Depth: infinity by default; walk by
                    // hand from here on rather than asking again each time.
                    log::info!("{} refused a deep PROPFIND ({err}); walking folder by folder", self.label);
                    self.deep_walk.store(false, Ordering::Relaxed);
                }
            }
        }
        let mut out = Vec::new();
        let mut queue = vec![root.clone()];
        while let Some(dir) = queue.pop() {
            ctx.cancel.check()?;
            let found = self.propfind(
                &self.dav.dir_url(&dir),
                "1",
                propfind::PROPS,
                &format!("Cannot list {dir}"),
            )?;
            let dir_path = self.server_path(&dir);
            for entry in found {
                if entry.path == dir_path {
                    continue;
                }
                if entry.is_dir {
                    queue.push(dir.child(last_segment(&entry.path)));
                }
                out.push(entry);
            }
        }
        Ok(strip_root(out, &root_path))
    }

    /// Walk folder by folder from here on, as this connection does on its
    /// own against a server that refuses `Depth: infinity`. Public so that
    /// path can be exercised against a server that does allow it.
    pub fn walk_folder_by_folder(&self) {
        self.deep_walk
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn exists(&self, path: &VPath) -> VfsResult<bool> {
        Ok(self.stat(path, &OpCtx::new())?.is_some())
    }

    fn destination(&self, path: &VPath, is_dir: bool) -> String {
        if is_dir {
            self.dav.dir_url(path)
        } else {
            self.dav.url(path)
        }
    }

    /// COPY or MOVE, which the server carries out itself.
    fn server_side(
        &self,
        method: &str,
        src: &VPath,
        dst: &VPath,
        is_dir: bool,
        overwrite: bool,
    ) -> VfsResult<()> {
        let response = self.dav.send(
            method,
            &self.destination(src, is_dir),
            &[
                ("Destination", &self.destination(dst, is_dir)),
                ("Overwrite", if overwrite { "T" } else { "F" }),
                ("Depth", "infinity"),
            ],
            DavBody::None,
        )?;
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(status_error(status, &format!("Cannot {} {src} to {dst}", method.to_lowercase())))
        }
    }

    pub(crate) fn put(&self, path: &VPath, body: &[u8]) -> VfsResult<()> {
        self.dav
            .send_ok("PUT", &self.dav.url(path), &[], body, &format!("Cannot write {path}"))
            .map(drop)
    }

    pub(crate) fn mkcol(&self, path: &VPath) -> VfsResult<()> {
        let response = self.dav.send("MKCOL", &self.dav.dir_url(path), &[], DavBody::None)?;
        let status = response.status().as_u16();
        match status {
            // 405 means it is already there, which a tar unpack meets
            // constantly and should not treat as a failure.
            s if (200..300).contains(&s) || s == 405 => Ok(()),
            other => Err(status_error(other, &format!("Cannot create {path}"))),
        }
    }

    fn transfer(
        &self,
        items: &[TransferItem],
        ctx: &OpCtx,
        progress: &Progress,
        method: &str,
    ) -> VfsResult<()> {
        progress.set_totals(0, items.len() as u64);
        for item in items {
            ctx.cancel.check()?;
            progress.set_current(item.src.display());
            let is_dir = item.kind == EntryKind::Dir;
            match item.resolution {
                // One request, and the server does the rest.
                Resolution::Create => self.server_side(method, &item.src, &item.dst, is_dir, false)?,
                Resolution::Replace => self.server_side(method, &item.src, &item.dst, is_dir, true)?,
                // A merge needs file-by-file decisions, which no single
                // COPY can express.
                Resolution::Merge(policy) => self.merge(item, policy, ctx, progress, method)?,
            }
            progress.add_items(1);
        }
        Ok(())
    }

    fn merge(
        &self,
        item: &TransferItem,
        policy: MergePolicy,
        ctx: &OpCtx,
        progress: &Progress,
        method: &str,
    ) -> VfsResult<()> {
        let sources = self.walk_entries(&item.src, ctx)?;
        let existing: HashMap<Vec<u8>, DavEntry> = self
            .walk_entries(&item.dst, ctx)?
            .into_iter()
            .map(|e| (e.path.clone(), e))
            .collect();
        let mut moved_any_file = false;
        for source in &sources {
            ctx.cancel.check()?;
            let src = item.src.join(&source.path);
            let dst = item.dst.join(&source.path);
            if source.is_dir {
                self.mkcol(&dst)?;
                continue;
            }
            progress.set_current(src.display());
            if let Some(there) = existing.get(&source.path) {
                let replace = match policy {
                    MergePolicy::Overwrite => true,
                    MergePolicy::SkipExisting => false,
                    MergePolicy::OverwriteOlder => match (source.mtime, there.mtime) {
                        (Some(new), Some(old)) => new > old,
                        _ => false,
                    },
                };
                if !replace {
                    continue;
                }
            }
            self.server_side(method, &src, &dst, false, true)?;
            moved_any_file = true;
        }
        // A move whose files all went across can drop the empty source
        // tree; one that kept files back must leave it standing.
        if method == "MOVE" && moved_any_file {
            let left = self.walk_entries(&item.src, ctx)?;
            if left.iter().all(|e| e.is_dir) {
                let _ = self.delete(std::slice::from_ref(&item.src), ctx, &Progress::new());
            }
        }
        Ok(())
    }
}

/// Turns server paths into paths relative to the walked root.
fn strip_root(entries: Vec<DavEntry>, root_path: &[u8]) -> Vec<DavEntry> {
    entries
        .into_iter()
        .filter_map(|mut entry| {
            let rest = entry.path.strip_prefix(root_path)?;
            let rest = rest.strip_prefix(b"/").unwrap_or(rest);
            if rest.is_empty() {
                return None;
            }
            entry.path = rest.to_vec();
            Some(entry)
        })
        .collect()
}

impl Vfs for DavVfs {
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
        // WebDAV has no permissions, links, shell or trash.
        Capabilities::default()
    }

    fn home(&self) -> VPath {
        if self.target.start.is_empty() {
            VPath::root()
        } else {
            VPath::root().join(&self.target.start)
        }
    }

    fn list(&self, dir: &VPath, ctx: &OpCtx) -> VfsResult<Vec<Entry>> {
        ctx.cancel.check()?;
        let found = self.propfind(
            &self.dav.dir_url(dir),
            "1",
            propfind::PROPS,
            &format!("Cannot list {dir}"),
        )?;
        let dir_path = self.server_path(dir);
        Ok(found
            .iter()
            .filter(|e| e.path != dir_path && !e.path.is_empty())
            .map(|e| entry_from(e, last_segment(&e.path)))
            .collect())
    }

    fn stat(&self, path: &VPath, _ctx: &OpCtx) -> VfsResult<Option<Entry>> {
        let response = self.dav.send(
            "PROPFIND",
            &self.dav.url(path),
            &[("Depth", "0"), ("Content-Type", "application/xml; charset=utf-8")],
            propfind::PROPS,
        )?;
        let status = response.status().as_u16();
        if status == 404 || status == 410 {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            return Err(status_error(status, &format!("Cannot read {path}")));
        }
        let xml = response
            .bytes()
            .map_err(|e| VfsError::other(format!("Cannot read {path}: {e}")))?;
        let found = propfind::parse_multistatus(&xml)?;
        let name = path.file_name().unwrap_or(b"/");
        Ok(found.first().map(|e| entry_from(e, name)))
    }

    fn walk(&self, root: &VPath, ctx: &OpCtx) -> VfsResult<Vec<WalkEntry>> {
        Ok(self
            .walk_entries(root, ctx)?
            .into_iter()
            .map(|e| WalkEntry {
                rel: e.path,
                kind: if e.is_dir { EntryKind::Dir } else { EntryKind::File },
                size: e.size,
                mtime: e.mtime,
            })
            .collect())
    }

    fn scan(&self, paths: &[VPath], ctx: &OpCtx) -> VfsResult<ScanTotals> {
        let mut totals = ScanTotals::default();
        for path in paths {
            match self.stat(path, ctx)? {
                Some(entry) if !entry.is_dir() => {
                    totals.files += 1;
                    totals.bytes += entry.size;
                    continue;
                }
                Some(_) => totals.dirs += 1,
                None => continue,
            }
            for entry in self.walk_entries(path, ctx)? {
                if entry.is_dir {
                    totals.dirs += 1;
                } else {
                    totals.files += 1;
                    totals.bytes += entry.size;
                }
            }
        }
        Ok(totals)
    }

    fn mkdir(&self, path: &VPath, _ctx: &OpCtx) -> VfsResult<()> {
        if self.exists(path)? {
            return Err(VfsError::AlreadyExists(path.display()));
        }
        self.mkcol(path)
    }

    fn create_file(&self, path: &VPath, _ctx: &OpCtx) -> VfsResult<()> {
        if self.exists(path)? {
            return Err(VfsError::AlreadyExists(path.display()));
        }
        self.put(path, &[])
    }

    fn rename(&self, from: &VPath, to: &VPath, ctx: &OpCtx) -> VfsResult<()> {
        let is_dir = self
            .stat(from, ctx)?
            .ok_or_else(|| VfsError::NotFound(from.display()))?
            .is_dir();
        if self.exists(to)? {
            return Err(VfsError::AlreadyExists(to.display()));
        }
        self.server_side("MOVE", from, to, is_dir, false)
    }

    fn delete(&self, paths: &[VPath], ctx: &OpCtx, progress: &Progress) -> VfsResult<()> {
        for path in paths {
            ctx.cancel.check()?;
            if path.is_root() {
                return Err(VfsError::other("Refusing to delete the whole server"));
            }
            progress.set_current(path.display());
            let response = self.dav.send("DELETE", &self.dav.url(path), &[], DavBody::None)?;
            let status = response.status().as_u16();
            // Gone already is the outcome we wanted.
            if !(200..300).contains(&status) && status != 404 {
                return Err(status_error(status, &format!("Cannot delete {path}")));
            }
            progress.add_items(1);
        }
        Ok(())
    }

    fn remove_empty_dirs(&self, dirs: &[VPath], ctx: &OpCtx) -> VfsResult<()> {
        for dir in dirs {
            if dir.is_root() {
                continue;
            }
            // Only remove it when nothing is left inside.
            if let Ok(left) = self.walk_entries(dir, ctx)
                && left.is_empty()
            {
                let _ = self.delete(std::slice::from_ref(dir), ctx, &Progress::new());
            }
        }
        Ok(())
    }

    fn copy_within(&self, items: &[TransferItem], ctx: &OpCtx, progress: &Progress) -> VfsResult<()> {
        self.transfer(items, ctx, progress, "COPY")
    }

    fn move_within(&self, items: &[TransferItem], ctx: &OpCtx, progress: &Progress) -> VfsResult<()> {
        self.transfer(items, ctx, progress, "MOVE")
    }

    fn chmod(&self, _paths: &[VPath], _mode: u32, _recursive: bool, _ctx: &OpCtx) -> VfsResult<()> {
        Err(VfsError::Unsupported("permissions over WebDAV".into()))
    }

    fn symlink(&self, _target: &[u8], _link: &VPath, _ctx: &OpCtx) -> VfsResult<()> {
        Err(VfsError::Unsupported("symbolic links over WebDAV".into()))
    }

    fn read_file(&self, path: &VPath, limit: u64, _ctx: &OpCtx) -> VfsResult<Vec<u8>> {
        let range;
        let mut headers: Vec<(&str, &str)> = Vec::new();
        if limit != u64::MAX && limit > 0 {
            range = format!("bytes=0-{}", limit - 1);
            headers.push(("Range", &range));
        }
        let mut response = self.dav.send_ok(
            "GET",
            &self.dav.url(path),
            &headers,
            DavBody::None,
            &format!("Cannot read {path}"),
        )?;
        if limit == u64::MAX {
            return response
                .bytes()
                .map(|bytes| bytes.to_vec())
                .map_err(|e| VfsError::other(format!("Cannot read {path}: {e}")));
        }
        // A server that takes no notice of Range answers with the whole
        // file, and a preview must not pull a gigabyte into memory because
        // of it.
        let mut out = Vec::new();
        response
            .by_ref()
            .take(limit)
            .read_to_end(&mut out)
            .map_err(|e| VfsError::other(format!("Cannot read {path}: {e}")))?;
        Ok(out)
    }

    fn write_file(&self, path: &VPath, data: &[u8], _ctx: &OpCtx) -> VfsResult<()> {
        self.put(path, data)
    }

    fn free_space(&self, path: &VPath, _ctx: &OpCtx) -> VfsResult<Option<SpaceInfo>> {
        // RFC 4331, which plenty of servers do not answer.
        let Ok(found) = self.propfind(
            &self.dav.dir_url(path),
            "0",
            propfind::QUOTA_PROPS,
            "Cannot read the quota",
        ) else {
            return Ok(None);
        };
        let Some(entry) = found.first() else {
            return Ok(None);
        };
        let Some(available) = entry.quota_available else {
            return Ok(None);
        };
        Ok(Some(SpaceInfo {
            total: available + entry.quota_used.unwrap_or(0),
            available,
        }))
    }

    fn export_tar(&self, base: &VPath, names: &[Vec<u8>], ctx: &OpCtx) -> VfsResult<Box<dyn TarSource>> {
        crate::tarstream::export(self, base, names, ctx)
    }

    fn import_tar(&self, dir: &VPath, ctx: &OpCtx) -> VfsResult<Box<dyn TarSink>> {
        crate::tarstream::import(self, dir, ctx)
    }

    fn server_info(&self) -> Option<String> {
        Some(format!("WebDAV · {}", self.target.base))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, is_dir: bool) -> DavEntry {
        DavEntry {
            path: path.as_bytes().to_vec(),
            is_dir,
            ..Default::default()
        }
    }

    #[test]
    fn walked_paths_become_relative() {
        let found = vec![
            entry("/dav/base", true),
            entry("/dav/base/a.txt", false),
            entry("/dav/base/sub", true),
            entry("/dav/base/sub/b.txt", false),
            // Something outside the root has no place in the result.
            entry("/dav/elsewhere/c.txt", false),
        ];
        let rel: Vec<String> = strip_root(found, b"/dav/base")
            .iter()
            .map(|e| String::from_utf8_lossy(&e.path).into_owned())
            .collect();
        assert_eq!(rel, ["a.txt", "sub", "sub/b.txt"]);
    }

    #[test]
    fn names_come_from_the_last_segment() {
        assert_eq!(last_segment(b"/a/b/c.txt"), b"c.txt");
        assert_eq!(last_segment(b"solo"), b"solo");
        assert_eq!(last_segment(b"/"), b"");
    }
}
