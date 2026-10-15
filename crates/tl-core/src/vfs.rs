use std::io::{Read, Write};

use crate::cancel::CancelToken;
use crate::entry::{Entry, EntryKind};
use crate::error::{VfsError, VfsResult};
use crate::path::VPath;
use crate::progress::Progress;

/// Per-operation context passed to every provider call.
#[derive(Clone, Default)]
pub struct OpCtx {
    /// Run with elevated privileges (sudo). Providers without the
    /// `elevation` capability ignore it.
    pub elevated: bool,
    pub owner_hint: OwnerHint,
    pub cancel: CancelToken,
}

impl OpCtx {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_cancel(cancel: CancelToken) -> Self {
        OpCtx {
            cancel,
            ..Default::default()
        }
    }

    pub fn elevated(mut self, on: bool) -> Self {
        self.elevated = on;
        self
    }
}

/// Who should own files created by an elevated copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OwnerHint {
    /// Whatever the operation naturally produces (`cp -a` keeps the source
    /// owner when run as root).
    #[default]
    Natural,
    /// The copy ran elevated only because the *source* needed it; hand the
    /// new files to the login user, as if they had copied them themselves.
    LoginUser,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Capabilities {
    /// Operations can run as another user (sudo).
    pub elevation: bool,
    /// Deleting can move items to a recoverable trash.
    pub trash: bool,
    /// Arbitrary shell commands can run in a directory (command line).
    pub exec: bool,
    pub chmod: bool,
    pub chown: bool,
    pub symlink: bool,
    /// Names differing only in case refer to the same file.
    pub case_insensitive: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SpaceInfo {
    pub total: u64,
    pub available: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: i32,
}

/// Totals of a recursive scan, for progress reporting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanTotals {
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
}

/// One node of a recursive listing; `rel` is relative to the walk root.
#[derive(Clone, Debug)]
pub struct WalkEntry {
    pub rel: Vec<u8>,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime: Option<i64>,
}

/// How a transfer item treats what already exists at its destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Nothing exists at the destination.
    Create,
    /// Remove the destination, then put the source in its place.
    Replace,
    /// Both are directories: merge the source into the destination.
    Merge(MergePolicy),
}

/// What a merge does with files that exist on both sides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergePolicy {
    Overwrite,
    /// Overwrite only when the source file is newer.
    OverwriteOlder,
    /// Keep existing files, add only new ones.
    SkipExisting,
}

/// One top-level item of a copy/move, already resolved against the
/// destination by the transfer engine.
#[derive(Clone, Debug)]
pub struct TransferItem {
    pub src: VPath,
    /// Full destination path (may carry a new name).
    pub dst: VPath,
    pub kind: EntryKind,
    pub resolution: Resolution,
}

/// A tar stream produced by a provider (e.g. `ssh host tar -cf - ...`).
pub trait TarSource: Read + Send {
    /// Call after reading to EOF: waits for the producer and reports errors
    /// it printed (unreadable files, ...).
    fn finish(self: Box<Self>) -> VfsResult<()>;
}

/// A tar stream consumer that unpacks into a directory.
pub trait TarSink: Write + Send {
    /// Call after the whole archive was written: waits for the extraction to
    /// complete and reports its errors.
    fn finish(self: Box<Self>) -> VfsResult<()>;
}

/// A mounted file system: the local disk, one SSH connection, later SFTP,
/// S3... Every method blocks, so callers run them on worker threads.
pub trait Vfs: Send + Sync {
    /// Unique id of this connection instance.
    fn id(&self) -> &str;

    /// Plugin that created it ("local", "ssh").
    fn plugin_id(&self) -> &str;

    /// Short label for UI ("Local", "deploy@web-1").
    fn label(&self) -> String;

    /// Two providers with equal keys see the same file namespace with the
    /// same identity, so copy/move between them runs server-side
    /// (`cp`/`mv` on the remote) instead of streaming through this machine.
    fn location_key(&self) -> &str;

    fn capabilities(&self) -> Capabilities;

    fn is_connected(&self) -> bool {
        true
    }

    fn home(&self) -> VPath;

    fn list(&self, dir: &VPath, ctx: &OpCtx) -> VfsResult<Vec<Entry>>;

    /// `lstat` of one path, `Ok(None)` if it does not exist.
    fn stat(&self, path: &VPath, ctx: &OpCtx) -> VfsResult<Option<Entry>>;

    /// Recursive listing below `root` (excluding it), parents before children.
    fn walk(&self, root: &VPath, ctx: &OpCtx) -> VfsResult<Vec<WalkEntry>>;

    /// Recursive totals of the given paths (symlinks are not followed).
    fn scan(&self, paths: &[VPath], ctx: &OpCtx) -> VfsResult<ScanTotals>;

    fn mkdir(&self, path: &VPath, ctx: &OpCtx) -> VfsResult<()>;

    /// Creates an empty file; fails if it exists.
    fn create_file(&self, path: &VPath, ctx: &OpCtx) -> VfsResult<()>;

    /// Renames within the same file system; fails if `to` exists.
    fn rename(&self, from: &VPath, to: &VPath, ctx: &OpCtx) -> VfsResult<()>;

    /// Permanently deletes paths (recursively).
    fn delete(&self, paths: &[VPath], ctx: &OpCtx, progress: &Progress) -> VfsResult<()>;

    /// Moves paths to the trash, if the provider has one.
    fn trash(&self, _paths: &[VPath], _ctx: &OpCtx, _progress: &Progress) -> VfsResult<()> {
        Err(VfsError::Unsupported("trash".into()))
    }

    /// Best-effort removal of directories that became empty (deepest
    /// first); used after a filtered cross-location move.
    fn remove_empty_dirs(&self, dirs: &[VPath], ctx: &OpCtx) -> VfsResult<()>;

    /// Copies items inside this file system (server-side for remotes).
    fn copy_within(&self, items: &[TransferItem], ctx: &OpCtx, progress: &Progress)
    -> VfsResult<()>;

    /// Moves items inside this file system (server-side for remotes).
    fn move_within(&self, items: &[TransferItem], ctx: &OpCtx, progress: &Progress)
    -> VfsResult<()>;

    fn chmod(&self, paths: &[VPath], mode: u32, recursive: bool, ctx: &OpCtx) -> VfsResult<()>;

    fn chown(
        &self,
        _paths: &[VPath],
        _owner: Option<&str>,
        _group: Option<&str>,
        _recursive: bool,
        _ctx: &OpCtx,
    ) -> VfsResult<()> {
        Err(VfsError::Unsupported("changing ownership".into()))
    }

    fn symlink(&self, target: &[u8], link: &VPath, ctx: &OpCtx) -> VfsResult<()>;

    /// Reads up to `limit` bytes from the start of a file.
    fn read_file(&self, path: &VPath, limit: u64, ctx: &OpCtx) -> VfsResult<Vec<u8>>;

    /// Replaces a file's contents. Existing files are rewritten in place so
    /// owner, mode and hard links survive (important for config files).
    fn write_file(&self, path: &VPath, data: &[u8], ctx: &OpCtx) -> VfsResult<()>;

    /// Streams `names` (relative to `base`) as a tar archive whose members
    /// are named `./<name>/...`.
    fn export_tar(&self, base: &VPath, names: &[Vec<u8>], ctx: &OpCtx)
    -> VfsResult<Box<dyn TarSource>>;

    /// Returns a sink that unpacks a tar stream into `dir`.
    fn import_tar(&self, dir: &VPath, ctx: &OpCtx) -> VfsResult<Box<dyn TarSink>>;

    fn free_space(&self, path: &VPath, ctx: &OpCtx) -> VfsResult<Option<SpaceInfo>>;

    /// Finds entries below `root` whose names match `masks` (wildcards,
    /// case-insensitive). Calls `found` for each match until it returns
    /// false. The default walks the whole tree; providers that can search
    /// server-side override it.
    fn find(
        &self,
        root: &VPath,
        masks: &[String],
        ctx: &OpCtx,
        found: &mut dyn FnMut(VPath, EntryKind) -> bool,
    ) -> VfsResult<()> {
        let joined = masks.join(";");
        for entry in self.walk(root, ctx)? {
            ctx.cancel.check()?;
            let name = entry.rel.rsplit(|b| *b == b'/').next().unwrap_or(&entry.rel);
            if crate::util::wildcard_match(&joined, &String::from_utf8_lossy(name))
                && !found(root.join(&entry.rel), entry.kind)
            {
                break;
            }
        }
        Ok(())
    }

    /// Runs a shell command line in `cwd` (the command line under the panes).
    fn exec(&self, _cwd: &VPath, _command: &str, _ctx: &OpCtx) -> VfsResult<ExecOutput> {
        Err(VfsError::Unsupported("running commands".into()))
    }

    /// Prepares elevated operations (e.g. asks for and verifies the sudo
    /// password). Called when the user turns elevation on for a pane.
    fn prepare_elevation(&self, _ctx: &OpCtx) -> VfsResult<()> {
        Err(VfsError::Unsupported("elevation".into()))
    }

    /// argv of an interactive terminal session opened at `cwd`, for "Open
    /// in Terminal". `None` if the provider has no such notion.
    fn terminal_command(&self, _cwd: &VPath, _elevated: bool) -> Option<Vec<String>> {
        None
    }

    /// Short description of the server (OS, tools) for the UI.
    fn server_info(&self) -> Option<String> {
        None
    }

    fn disconnect(&self) {}
}
