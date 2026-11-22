use std::io::{Read, Write};
use std::sync::Arc;

use crate::archive::{ArchiveBackend, ArchiveFormat};
use crate::cancel::CancelToken;
use crate::entry::{Entry, EntryKind};
use crate::error::{VfsError, VfsResult};
use crate::path::VPath;
use crate::progress::Progress;

pub use tl_model::vfs::*;

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

    /// Renames several items one after another, like [`Vfs::rename`],
    /// counting completed steps in `done` so a failed batch can be undone.
    /// Providers that pay per call (servers) do it in one command.
    fn rename_all(&self, steps: &[(VPath, VPath)], ctx: &OpCtx, done: &mut usize) -> VfsResult<()> {
        for (from, to) in steps {
            ctx.cancel.check()?;
            self.rename(from, to, ctx)?;
            *done += 1;
        }
        Ok(())
    }

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

    /// The archive work for an archive file in this file system, used by
    /// [`crate::archive::open`] to show the archive as a folder.
    fn open_archive(&self, _path: &VPath, format: ArchiveFormat) -> VfsResult<Arc<dyn ArchiveBackend>> {
        Err(VfsError::Unsupported(format!("{} archives here", format.name())))
    }

    /// For a file system mounted from a file of another one (an archive):
    /// that file system and the file's path.
    fn host(&self) -> Option<(Arc<dyn Vfs>, VPath)> {
        None
    }

    /// Short description of the server (OS, tools) for the UI.
    fn server_info(&self) -> Option<String> {
        None
    }

    fn disconnect(&self) {}
}
