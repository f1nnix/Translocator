//! The local file system, implemented natively with `std::fs`.

mod archive;
mod fsops;
mod tarstream;
mod users;

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

use tl_core::archive::{ArchiveBackend, ArchiveFormat};
use tl_core::plugin::FieldKind;
use tl_core::{
    Bookmark, CancelToken, Capabilities, Entry, EntryKind, ExecOutput, FieldSpec, OpCtx,
    PluginEnv, Progress, ProviderPlugin, Resolution, ScanTotals, SpaceInfo, TarSink, TarSource,
    TransferItem, VPath, Vfs, VfsError, VfsResult, WalkEntry,
};

use fsops::Ctx;
use users::NameCache;

pub const PLUGIN_ID: &str = "local";

/// The single local file system instance.
pub fn local_vfs() -> Arc<dyn Vfs> {
    static VFS: OnceLock<Arc<LocalVfs>> = OnceLock::new();
    VFS.get_or_init(|| Arc::new(LocalVfs::new())).clone()
}

pub struct LocalVfs {
    home: VPath,
    names: NameCache,
}

impl LocalVfs {
    fn new() -> Self {
        let home = dirs_home().map(|h| VPath::from_local(&h)).unwrap_or_else(VPath::root);
        LocalVfs {
            home,
            names: NameCache::default(),
        }
    }

    fn entry(&self, name: Vec<u8>, path: &Path, meta: &fs::Metadata) -> Entry {
        let kind = EntryKind::from_mode(meta.mode());
        let mut entry = Entry::new(name, kind);
        entry.size = meta.len();
        entry.mtime = Some(meta.mtime());
        entry.mode = meta.mode() & 0o7777;
        entry.uid = Some(meta.uid());
        entry.gid = Some(meta.gid());
        entry.user = self.names.user(meta.uid());
        entry.group = self.names.group(meta.gid());
        #[cfg(target_os = "macos")]
        {
            use std::os::macos::fs::MetadataExt as _;
            entry.hidden_flag = meta.st_flags() & libc::UF_HIDDEN != 0;
        }
        if kind == EntryKind::Symlink {
            use std::os::unix::ffi::OsStrExt;
            entry.link_target = fs::read_link(path)
                .ok()
                .map(|t| t.as_os_str().as_bytes().to_vec());
            entry.target_kind = fs::metadata(path)
                .ok()
                .map(|m| EntryKind::from_mode(m.mode()));
        }
        entry
    }
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(Into::into)
}

fn io_err(e: io::Error, path: &VPath) -> VfsError {
    VfsError::io(e, path)
}

impl Vfs for LocalVfs {
    fn id(&self) -> &str {
        "local"
    }

    fn plugin_id(&self) -> &str {
        PLUGIN_ID
    }

    fn label(&self) -> String {
        "Local".into()
    }

    fn location_key(&self) -> &str {
        "local:/"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            elevation: false,
            trash: true,
            exec: true,
            chmod: true,
            chown: true,
            symlink: true,
            case_insensitive: cfg!(target_os = "macos"),
        }
    }

    fn home(&self) -> VPath {
        self.home.clone()
    }

    fn list(&self, dir: &VPath, ctx: &OpCtx) -> VfsResult<Vec<Entry>> {
        use std::os::unix::ffi::OsStrExt;
        let local = dir.to_local();
        let read = fs::read_dir(&local).map_err(|e| io_err(e, dir))?;
        let mut out = Vec::new();
        for item in read {
            ctx.cancel.check()?;
            let item = item.map_err(|e| io_err(e, dir))?;
            let path = item.path();
            // DirEntry::metadata does not follow symlinks.
            let meta = match item.metadata() {
                Ok(meta) => meta,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io_err(e, dir)),
            };
            out.push(self.entry(item.file_name().as_bytes().to_vec(), &path, &meta));
        }
        Ok(out)
    }

    fn stat(&self, path: &VPath, _ctx: &OpCtx) -> VfsResult<Option<Entry>> {
        let local = path.to_local();
        match fs::symlink_metadata(&local) {
            Ok(meta) => {
                let name = path.file_name().unwrap_or(b"/").to_vec();
                Ok(Some(self.entry(name, &local, &meta)))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(e, path)),
        }
    }

    fn walk(&self, root: &VPath, ctx: &OpCtx) -> VfsResult<Vec<WalkEntry>> {
        let mut out = Vec::new();
        fsops::walk_into(&root.to_local(), &mut Vec::new(), &mut out, &ctx.cancel)?;
        Ok(out)
    }

    fn scan(&self, paths: &[VPath], ctx: &OpCtx) -> VfsResult<ScanTotals> {
        let mut totals = ScanTotals::default();
        for path in paths {
            fsops::scan_into(&path.to_local(), &mut totals, &ctx.cancel)?;
        }
        Ok(totals)
    }

    fn mkdir(&self, path: &VPath, _ctx: &OpCtx) -> VfsResult<()> {
        fs::create_dir(path.to_local()).map_err(|e| io_err(e, path))
    }

    fn create_file(&self, path: &VPath, _ctx: &OpCtx) -> VfsResult<()> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path.to_local())
            .map(drop)
            .map_err(|e| io_err(e, path))
    }

    fn rename(&self, from: &VPath, to: &VPath, _ctx: &OpCtx) -> VfsResult<()> {
        let (src, dst) = (from.to_local(), to.to_local());
        if let Ok(existing) = fs::symlink_metadata(&dst) {
            // On case-insensitive volumes "a" → "A" finds the file itself.
            let same = fs::symlink_metadata(&src)
                .is_ok_and(|m| m.ino() == existing.ino() && m.dev() == existing.dev());
            if !same {
                return Err(VfsError::AlreadyExists(to.display()));
            }
        }
        fs::rename(&src, &dst).map_err(|e| io_err(e, from))
    }

    fn delete(&self, paths: &[VPath], ctx: &OpCtx, progress: &Progress) -> VfsResult<()> {
        let fctx = Ctx {
            cancel: &ctx.cancel,
            progress,
        };
        for path in paths {
            progress.set_current(path.display());
            fsops::remove_tree(&path.to_local(), &fctx)?;
        }
        Ok(())
    }

    fn trash(&self, paths: &[VPath], ctx: &OpCtx, progress: &Progress) -> VfsResult<()> {
        // Only the macOS branch below mutates it.
        #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
        let mut trash = trash::TrashContext::default();
        #[cfg(target_os = "macos")]
        {
            use trash::macos::{DeleteMethod, TrashContextExtMacos};
            // NSFileManager needs no Finder automation permission.
            trash.set_delete_method(DeleteMethod::NsFileManager);
        }
        for path in paths {
            ctx.cancel.check()?;
            progress.set_current(path.display());
            trash
                .delete(path.to_local())
                .map_err(|e| VfsError::other(format!("Cannot move {path} to Trash: {e}")))?;
            progress.add_items(1);
        }
        Ok(())
    }

    fn remove_empty_dirs(&self, dirs: &[VPath], _ctx: &OpCtx) -> VfsResult<()> {
        for dir in dirs {
            let _ = fs::remove_dir(dir.to_local());
        }
        Ok(())
    }

    fn copy_within(
        &self,
        items: &[TransferItem],
        ctx: &OpCtx,
        progress: &Progress,
    ) -> VfsResult<()> {
        let paths: Vec<VPath> = items.iter().map(|i| i.src.clone()).collect();
        let totals = self.scan(&paths, ctx)?;
        progress.set_totals(totals.bytes, totals.files + totals.dirs);
        let fctx = Ctx {
            cancel: &ctx.cancel,
            progress,
        };
        for item in items {
            let (src, dst) = (item.src.to_local(), item.dst.to_local());
            match item.resolution {
                Resolution::Create => fsops::copy_tree(&src, &dst, &fctx)?,
                Resolution::Replace => {
                    fsops::remove_tree(&dst, &Ctx {
                        cancel: &ctx.cancel,
                        progress: &Progress::new(),
                    })?;
                    fsops::copy_tree(&src, &dst, &fctx)?;
                }
                Resolution::Merge(policy) => fsops::merge_tree(&src, &dst, policy, &fctx)?,
            }
        }
        Ok(())
    }

    fn move_within(
        &self,
        items: &[TransferItem],
        ctx: &OpCtx,
        progress: &Progress,
    ) -> VfsResult<()> {
        progress.set_totals(0, items.len() as u64);
        let fctx = Ctx {
            cancel: &ctx.cancel,
            progress,
        };
        for item in items {
            let (src, dst) = (item.src.to_local(), item.dst.to_local());
            match item.resolution {
                Resolution::Create => fsops::move_tree(&src, &dst, &fctx)?,
                Resolution::Replace => {
                    fsops::remove_tree(&dst, &Ctx {
                        cancel: &ctx.cancel,
                        progress: &Progress::new(),
                    })?;
                    fsops::move_tree(&src, &dst, &fctx)?;
                }
                Resolution::Merge(policy) => fsops::merge_move(&src, &dst, policy, &fctx)?,
            }
        }
        Ok(())
    }

    fn chmod(&self, paths: &[VPath], mode: u32, recursive: bool, ctx: &OpCtx) -> VfsResult<()> {
        fn apply(path: &Path, mode: u32, recursive: bool, cancel: &CancelToken) -> VfsResult<()> {
            cancel.check()?;
            let meta = fs::symlink_metadata(path).map_err(|e| VfsError::io(e, path.display()))?;
            if meta.file_type().is_symlink() {
                return Ok(());
            }
            let is_dir = meta.is_dir();
            fs::set_permissions(path, fs::Permissions::from_mode(effective_mode(mode, is_dir, recursive)))
                .map_err(|e| VfsError::io(e, path.display()))?;
            if recursive && is_dir {
                for name in fsops::read_dir_names(path)? {
                    apply(&path.join(name), mode, true, cancel)?;
                }
            }
            Ok(())
        }
        for path in paths {
            apply(&path.to_local(), mode, recursive, &ctx.cancel)?;
        }
        Ok(())
    }

    fn chown(
        &self,
        paths: &[VPath],
        owner: Option<&str>,
        group: Option<&str>,
        recursive: bool,
        ctx: &OpCtx,
    ) -> VfsResult<()> {
        let uid = match owner.filter(|o| !o.is_empty()) {
            Some(o) => users::uid_of(o).ok_or_else(|| VfsError::other(format!("Unknown user {o}")))?,
            None => u32::MAX,
        };
        let gid = match group.filter(|g| !g.is_empty()) {
            Some(g) => users::gid_of(g).ok_or_else(|| VfsError::other(format!("Unknown group {g}")))?,
            None => u32::MAX,
        };
        fn apply(path: &Path, uid: u32, gid: u32, recursive: bool, cancel: &CancelToken) -> VfsResult<()> {
            use std::os::unix::ffi::OsStrExt;
            cancel.check()?;
            let c = std::ffi::CString::new(path.as_os_str().as_bytes())
                .map_err(|_| VfsError::other("invalid path"))?;
            if unsafe { libc::lchown(c.as_ptr(), uid, gid) } != 0 {
                return Err(VfsError::io(io::Error::last_os_error(), path.display()));
            }
            if recursive && fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
                for name in fsops::read_dir_names(path)? {
                    apply(&path.join(name), uid, gid, true, cancel)?;
                }
            }
            Ok(())
        }
        for path in paths {
            apply(&path.to_local(), uid, gid, recursive, &ctx.cancel)?;
        }
        Ok(())
    }

    fn symlink(&self, target: &[u8], link: &VPath, _ctx: &OpCtx) -> VfsResult<()> {
        use std::os::unix::ffi::OsStrExt;
        std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(target), link.to_local())
            .map_err(|e| io_err(e, link))
    }

    fn read_file(&self, path: &VPath, limit: u64, _ctx: &OpCtx) -> VfsResult<Vec<u8>> {
        let file = fs::File::open(path.to_local()).map_err(|e| io_err(e, path))?;
        let mut data = Vec::new();
        file.take(limit)
            .read_to_end(&mut data)
            .map_err(|e| io_err(e, path))?;
        Ok(data)
    }

    fn write_file(&self, path: &VPath, data: &[u8], _ctx: &OpCtx) -> VfsResult<()> {
        // Truncate in place: owner, mode and hard links stay intact.
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path.to_local())
            .map_err(|e| io_err(e, path))?;
        file.write_all(data).map_err(|e| io_err(e, path))?;
        file.sync_all().map_err(|e| io_err(e, path))
    }

    fn export_tar(
        &self,
        base: &VPath,
        names: &[Vec<u8>],
        ctx: &OpCtx,
    ) -> VfsResult<Box<dyn TarSource>> {
        Ok(Box::new(tarstream::export(
            base.to_local(),
            names.to_vec(),
            ctx.cancel.clone(),
        )?))
    }

    fn import_tar(&self, dir: &VPath, ctx: &OpCtx) -> VfsResult<Box<dyn TarSink>> {
        Ok(Box::new(tarstream::import(dir.to_local(), ctx.cancel.clone())?))
    }

    fn free_space(&self, path: &VPath, _ctx: &OpCtx) -> VfsResult<Option<SpaceInfo>> {
        let c = std::ffi::CString::new(path.as_bytes()).map_err(|_| VfsError::other("invalid path"))?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return Ok(None);
        }
        let frsize = st.f_frsize as u64;
        Ok(Some(SpaceInfo {
            total: st.f_blocks as u64 * frsize,
            available: st.f_bavail as u64 * frsize,
        }))
    }

    fn find(
        &self,
        root: &VPath,
        masks: &[String],
        ctx: &OpCtx,
        found: &mut dyn FnMut(VPath, EntryKind) -> bool,
    ) -> VfsResult<()> {
        fn visit(
            dir: &VPath,
            masks: &str,
            ctx: &OpCtx,
            found: &mut dyn FnMut(VPath, EntryKind) -> bool,
        ) -> VfsResult<bool> {
            use std::os::unix::ffi::OsStrExt;
            let Ok(read) = fs::read_dir(dir.to_local()) else {
                return Ok(true);
            };
            for item in read.flatten() {
                ctx.cancel.check()?;
                let Ok(meta) = item.metadata() else { continue };
                let name = item.file_name();
                let path = dir.child(name.as_bytes());
                let kind = EntryKind::from_mode(meta.mode());
                if tl_core::util::wildcard_match(masks, &name.to_string_lossy()) && !found(path.clone(), kind) {
                    return Ok(false);
                }
                if kind == EntryKind::Dir && !visit(&path, masks, ctx, found)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        visit(root, &masks.join(";"), ctx, found).map(drop)
    }

    fn open_archive(&self, path: &VPath, format: ArchiveFormat) -> VfsResult<Arc<dyn ArchiveBackend>> {
        Ok(Arc::new(archive::LocalArchive::new(path.to_local(), format)))
    }

    fn exec(&self, cwd: &VPath, command: &str, ctx: &OpCtx) -> VfsResult<ExecOutput> {
        let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
        let child = Command::new(shell)
            .arg("-c")
            .arg(command)
            .current_dir(cwd.to_local())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| VfsError::other(format!("Cannot start shell: {e}")))?;
        let pid = child.id() as libc::pid_t;
        let _guard = ctx.cancel.on_cancel(move || unsafe {
            libc::kill(pid, libc::SIGTERM);
        });
        let out = child
            .wait_with_output()
            .map_err(|e| VfsError::other(e.to_string()))?;
        ctx.cancel.check()?;
        Ok(ExecOutput {
            stdout: out.stdout,
            stderr: out.stderr,
            status: out.status.code().unwrap_or(-1),
        })
    }
}

/// Recursive chmod gives folders the search bit wherever the mode grants
/// read, so `644` applied recursively leaves them traversable.
pub fn effective_mode(mode: u32, is_dir: bool, recursive: bool) -> u32 {
    if is_dir && recursive {
        tl_core::util::recursive_dir_mode(mode)
    } else {
        mode
    }
}

/// Mounted volumes worth offering in the location menu.
pub fn volumes() -> Vec<(String, VPath)> {
    let mut out = vec![("/".to_string(), VPath::root())];
    #[cfg(target_os = "macos")]
    let roots = ["/Volumes"];
    #[cfg(not(target_os = "macos"))]
    let roots = ["/media", "/mnt", "/run/media"];
    for root in roots {
        let Ok(read) = fs::read_dir(root) else { continue };
        for item in read.flatten() {
            let path = item.path();
            // Skip the boot volume's alias in /Volumes.
            if fs::canonicalize(&path).is_ok_and(|p| p == Path::new("/")) {
                continue;
            }
            if path.is_dir() {
                out.push((
                    item.file_name().to_string_lossy().into_owned(),
                    VPath::from_local(&path),
                ));
            }
        }
    }
    out
}

/// "Local folder" bookmarks.
pub struct LocalPlugin;

impl ProviderPlugin for LocalPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn name(&self) -> &'static str {
        "Local folder"
    }

    fn description(&self) -> &'static str {
        "A folder on this Mac."
    }

    fn fields(&self) -> Vec<FieldSpec> {
        vec![
            FieldSpec::text("path", "Folder")
                .kind(FieldKind::LocalPath)
                .placeholder("~/Projects")
                .required(),
        ]
    }

    fn summary(&self, bookmark: &Bookmark) -> String {
        bookmark.get("path").to_string()
    }

    fn connect(
        &self,
        _bookmark: &Bookmark,
        _env: &PluginEnv,
        _cancel: &CancelToken,
    ) -> VfsResult<Arc<dyn Vfs>> {
        Ok(local_vfs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tl_core::transfer::{self, ConflictChoice, FixedResolver, TransferMode, TransferSpec};

    fn tree(root: &Path) {
        fs::create_dir_all(root.join("docs/deep")).unwrap();
        fs::write(root.join("docs/a.txt"), "alpha").unwrap();
        fs::write(root.join("docs/deep/b.txt"), "beta").unwrap();
        fs::write(root.join("single.txt"), "single").unwrap();
    }

    #[test]
    fn listing_and_stat() {
        let tmp = tempfile::tempdir().unwrap();
        tree(tmp.path());
        let vfs = local_vfs();
        let dir = VPath::from_local(tmp.path());
        let ctx = OpCtx::new();
        let mut names: Vec<String> = vfs.list(&dir, &ctx).unwrap().iter().map(|e| e.name_lossy()).collect();
        names.sort();
        assert_eq!(names, ["docs", "single.txt"]);
        let docs = vfs.stat(&dir.child(b"docs"), &ctx).unwrap().unwrap();
        assert!(docs.is_dir());
        assert!(vfs.stat(&dir.child(b"missing"), &ctx).unwrap().is_none());
    }

    #[test]
    fn tar_roundtrip_through_engine() {
        // Local → local through the streaming path: force it by wrapping the
        // destination so the location keys differ.
        struct Other(Arc<dyn Vfs>);
        impl Vfs for Other {
            fn id(&self) -> &str { "other" }
            fn plugin_id(&self) -> &str { "other" }
            fn label(&self) -> String { "other".into() }
            fn location_key(&self) -> &str { "other" }
            fn capabilities(&self) -> Capabilities { self.0.capabilities() }
            fn home(&self) -> VPath { self.0.home() }
            fn list(&self, d: &VPath, c: &OpCtx) -> VfsResult<Vec<Entry>> { self.0.list(d, c) }
            fn stat(&self, p: &VPath, c: &OpCtx) -> VfsResult<Option<Entry>> { self.0.stat(p, c) }
            fn walk(&self, r: &VPath, c: &OpCtx) -> VfsResult<Vec<WalkEntry>> { self.0.walk(r, c) }
            fn scan(&self, p: &[VPath], c: &OpCtx) -> VfsResult<ScanTotals> { self.0.scan(p, c) }
            fn mkdir(&self, p: &VPath, c: &OpCtx) -> VfsResult<()> { self.0.mkdir(p, c) }
            fn create_file(&self, p: &VPath, c: &OpCtx) -> VfsResult<()> { self.0.create_file(p, c) }
            fn rename(&self, a: &VPath, b: &VPath, c: &OpCtx) -> VfsResult<()> { self.0.rename(a, b, c) }
            fn delete(&self, p: &[VPath], c: &OpCtx, g: &Progress) -> VfsResult<()> { self.0.delete(p, c, g) }
            fn remove_empty_dirs(&self, d: &[VPath], c: &OpCtx) -> VfsResult<()> { self.0.remove_empty_dirs(d, c) }
            fn copy_within(&self, i: &[TransferItem], c: &OpCtx, g: &Progress) -> VfsResult<()> { self.0.copy_within(i, c, g) }
            fn move_within(&self, i: &[TransferItem], c: &OpCtx, g: &Progress) -> VfsResult<()> { self.0.move_within(i, c, g) }
            fn chmod(&self, p: &[VPath], m: u32, r: bool, c: &OpCtx) -> VfsResult<()> { self.0.chmod(p, m, r, c) }
            fn symlink(&self, t: &[u8], l: &VPath, c: &OpCtx) -> VfsResult<()> { self.0.symlink(t, l, c) }
            fn read_file(&self, p: &VPath, l: u64, c: &OpCtx) -> VfsResult<Vec<u8>> { self.0.read_file(p, l, c) }
            fn write_file(&self, p: &VPath, d: &[u8], c: &OpCtx) -> VfsResult<()> { self.0.write_file(p, d, c) }
            fn export_tar(&self, b: &VPath, n: &[Vec<u8>], c: &OpCtx) -> VfsResult<Box<dyn TarSource>> { self.0.export_tar(b, n, c) }
            fn import_tar(&self, d: &VPath, c: &OpCtx) -> VfsResult<Box<dyn TarSink>> { self.0.import_tar(d, c) }
            fn free_space(&self, p: &VPath, c: &OpCtx) -> VfsResult<Option<SpaceInfo>> { self.0.free_space(p, c) }
        }

        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        tree(&src);
        fs::create_dir_all(dst.join("docs")).unwrap();
        fs::write(dst.join("docs/a.txt"), "keep me").unwrap();

        let spec = TransferSpec {
            mode: TransferMode::Copy,
            src: local_vfs(),
            src_dir: VPath::from_local(&src),
            names: vec![b"docs".to_vec(), b"single.txt".to_vec()],
            src_elevated: false,
            dst: Arc::new(Other(local_vfs())),
            dst_dir: VPath::from_local(&dst),
            dst_elevated: false,
            target_name: None,
        };
        let progress = Progress::new();
        let report = transfer::run(
            &spec,
            &FixedResolver(ConflictChoice::SkipExisting),
            &CancelToken::new(),
            &progress,
        )
        .unwrap();
        assert!(!report.server_side);
        assert_eq!(fs::read_to_string(dst.join("docs/a.txt")).unwrap(), "keep me");
        assert_eq!(fs::read_to_string(dst.join("docs/deep/b.txt")).unwrap(), "beta");
        assert_eq!(fs::read_to_string(dst.join("single.txt")).unwrap(), "single");

        // Move with "keep both": renamed on the fly, sources removed.
        let spec = TransferSpec {
            mode: TransferMode::Move,
            names: vec![b"docs".to_vec()],
            ..spec
        };
        transfer::run(&spec, &FixedResolver(ConflictChoice::KeepBoth), &CancelToken::new(), &progress).unwrap();
        assert_eq!(fs::read_to_string(dst.join("docs (2)/deep/b.txt")).unwrap(), "beta");
        assert!(!src.join("docs").exists());
    }

    #[test]
    fn server_side_copy_onto_itself_makes_sibling() {
        let tmp = tempfile::tempdir().unwrap();
        tree(tmp.path());
        let dir = VPath::from_local(tmp.path());
        let spec = TransferSpec {
            mode: TransferMode::Copy,
            src: local_vfs(),
            src_dir: dir.clone(),
            names: vec![b"single.txt".to_vec()],
            src_elevated: false,
            dst: local_vfs(),
            dst_dir: dir,
            dst_elevated: false,
            target_name: None,
        };
        let report = transfer::run(&spec, &FixedResolver(ConflictChoice::Cancel), &CancelToken::new(), &Progress::new()).unwrap();
        assert!(report.server_side);
        assert_eq!(fs::read_to_string(tmp.path().join("single (2).txt")).unwrap(), "single");
    }

    #[test]
    fn chmod_x_semantics() {
        assert_eq!(effective_mode(0o644, true, true), 0o755);
        assert_eq!(effective_mode(0o600, true, true), 0o700);
        assert_eq!(effective_mode(0o644, false, true), 0o644);
        assert_eq!(effective_mode(0o644, true, false), 0o644);
    }
}
