//! The blocking halves of the effects: what runs on a worker thread for a
//! file system query, a job or a platform request. Nothing here touches the
//! core; results go back to the runtime thread.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tl_app::conn::ConnInfo;
use tl_app::{ConnId, FsOp, FsOutput, JobId, JobOutput, LocalFolder, Work};
use tl_core::transfer::{self, ConflictChoice, FixedResolver, TransferSpec};
use tl_core::{CancelToken, OpCtx, PluginEnv, PluginRegistry, Progress, VPath, Vfs, VfsError, VfsResult, archive};

use crate::Ui;
use crate::ask::JobResolver;
use crate::exec::Inbox;

/// The error for a file system the runtime does not hold.
pub(crate) fn not_connected() -> VfsError {
    VfsError::Connection("Not connected".into())
}

pub(crate) fn crashed() -> VfsError {
    VfsError::other("worker thread crashed")
}

/// The user's home directory on this machine.
pub(crate) fn local_home() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| dirs::home_dir().map(|h| h.to_string_lossy().into_owned()))
}

/// What the core learns about a file system when it is opened.
pub(crate) fn conn_info(id: ConnId, vfs: &dyn Vfs, bookmark: Option<String>, host: Option<(ConnId, VPath)>) -> ConnInfo {
    let plugin = vfs.plugin_id().to_string();
    let local = plugin == tl_local::PLUGIN_ID;
    ConnInfo {
        id,
        label: vfs.label(),
        location_key: vfs.location_key().to_string(),
        caps: vfs.capabilities(),
        home: vfs.home(),
        bookmark,
        host,
        server_info: vfs.server_info(),
        local_home: if local { local_home() } else { None },
        plugin,
    }
}

// ----- file system queries ---------------------------------------------------------

/// The conn a query is about.
pub(crate) fn fs_conn(op: &FsOp) -> ConnId {
    match op {
        FsOp::List { conn, .. }
        | FsOp::Stat { conn, .. }
        | FsOp::Read { conn, .. }
        | FsOp::PrepareElevation { conn }
        | FsOp::OpenArchive { conn, .. }
        | FsOp::InitialPath { conn, .. }
        | FsOp::Release { conn } => *conn,
    }
}

/// The answer to a query that could not run.
pub(crate) fn fs_failure(op: &FsOp, err: VfsError) -> FsOutput {
    match op {
        FsOp::List { .. } => FsOutput::Listing {
            entries: Err(err),
            space: None,
        },
        FsOp::Stat { .. } => FsOutput::Stat(Err(err)),
        FsOp::Read { .. } => FsOutput::Read(Err(err)),
        FsOp::OpenArchive { .. } => FsOutput::Opened(Err(err)),
        FsOp::InitialPath { .. } => FsOutput::Path(VPath::root()),
        FsOp::PrepareElevation { .. } | FsOp::Release { .. } => FsOutput::Done(Err(err)),
    }
}

/// A query's answer, and for an opened archive the file system to keep.
pub(crate) struct FsDone {
    pub output: FsOutput,
    pub register: Option<(ConnId, Arc<dyn Vfs>)>,
}

impl From<FsOutput> for FsDone {
    fn from(output: FsOutput) -> Self {
        FsDone { output, register: None }
    }
}

pub(crate) fn fs(op: &FsOp, vfs: Arc<dyn Vfs>, registry: &PluginRegistry) -> FsDone {
    match op {
        FsOp::List { path, elevated, .. } => {
            let ctx = OpCtx::new().elevated(*elevated);
            let entries = vfs.list(path, &ctx);
            let space = match entries {
                Ok(_) => vfs.free_space(path, &ctx).ok().flatten(),
                Err(_) => None,
            };
            FsOutput::Listing { entries, space }.into()
        }
        FsOp::Stat { path, elevated, .. } => FsOutput::Stat(vfs.stat(path, &OpCtx::new().elevated(*elevated))).into(),
        FsOp::Read {
            path, limit, elevated, ..
        } => {
            let ctx = OpCtx::new().elevated(*elevated);
            let read = vfs.read_file(path, *limit, &ctx).map(|data| {
                let size = match vfs.stat(path, &ctx) {
                    Ok(Some(entry)) => entry.size,
                    _ => data.len() as u64,
                };
                (data, size)
            });
            FsOutput::Read(read).into()
        }
        FsOp::PrepareElevation { .. } => FsOutput::Done(vfs.prepare_elevation(&OpCtx::new())).into(),
        FsOp::OpenArchive {
            conn,
            file,
            elevated,
            new,
        } => match archive::open(vfs, file.clone(), &OpCtx::new().elevated(*elevated)) {
            Ok(opened) => {
                let opened: Arc<dyn Vfs> = opened;
                let info = conn_info(*new, &*opened, None, Some((*conn, file.clone())));
                FsDone {
                    output: FsOutput::Opened(Ok(info)),
                    register: Some((*new, opened)),
                }
            }
            Err(err) => FsOutput::Opened(Err(err)).into(),
        },
        FsOp::InitialPath { bookmark, .. } => {
            let path = match registry.get(&bookmark.plugin) {
                Some(plugin) => plugin.initial_path(bookmark, &*vfs),
                None => vfs.home(),
            };
            FsOutput::Path(path).into()
        }
        FsOp::Release { .. } => FsOutput::Done(Ok(())).into(),
    }
}

// ----- jobs ------------------------------------------------------------------------------

/// The file systems a job works on.
pub(crate) fn work_conns(work: &Work) -> Vec<ConnId> {
    match work {
        Work::Transfer(req) => vec![req.src, req.dst],
        Work::Delete { conn, .. }
        | Work::MakeDirs { conn, .. }
        | Work::CreateFile { conn, .. }
        | Work::Rename { conn, .. }
        | Work::Renames { conn, .. }
        | Work::WriteFile { conn, .. }
        | Work::Attributes { conn, .. }
        | Work::Scan { conn, .. }
        | Work::Find { conn, .. }
        | Work::Exec { conn, .. }
        | Work::OpenCopy { conn, .. } => vec![*conn],
        Work::Connect { .. } => Vec::new(),
        Work::SyncCompare { left, right, .. } | Work::SyncApply { left, right, .. } => vec![left.conn, right.conn],
    }
}

/// Everything a running job needs.
pub(crate) struct JobCtx {
    pub job: JobId,
    pub cancel: CancelToken,
    pub progress: Progress,
    pub conns: HashMap<ConnId, Arc<dyn Vfs>>,
    pub registry: PluginRegistry,
    pub env: PluginEnv,
    pub inbox: Inbox,
    pub cache_dir: PathBuf,
    pub ui: Arc<dyn Ui>,
}

/// A job's output, and for a connection the file system to keep.
pub(crate) struct JobDone {
    pub output: JobOutput,
    pub register: Option<(ConnId, Arc<dyn Vfs>)>,
}

impl From<JobOutput> for JobDone {
    fn from(output: JobOutput) -> Self {
        JobDone { output, register: None }
    }
}

impl JobCtx {
    fn vfs(&self, conn: ConnId) -> VfsResult<Arc<dyn Vfs>> {
        self.conns.get(&conn).cloned().ok_or_else(not_connected)
    }

    fn op(&self, elevated: bool) -> OpCtx {
        OpCtx {
            elevated,
            cancel: self.cancel.clone(),
            ..Default::default()
        }
    }

    fn sync_end(&self, end: &tl_app::SyncEnd) -> VfsResult<tl_core::sync::SyncEnd> {
        Ok(tl_core::sync::SyncEnd {
            vfs: self.vfs(end.conn)?,
            dir: end.dir.clone(),
            elevated: end.elevated,
        })
    }
}

pub(crate) fn job(work: Work, cx: &JobCtx) -> VfsResult<JobDone> {
    let progress = &cx.progress;
    match work {
        Work::Transfer(req) => {
            let spec = TransferSpec {
                mode: req.mode,
                src: cx.vfs(req.src)?,
                src_dir: req.src_dir,
                names: req.names,
                src_elevated: req.src_elevated,
                dst: cx.vfs(req.dst)?,
                dst_dir: req.dst_dir,
                dst_elevated: req.dst_elevated,
                target_name: req.target_name,
            };
            let report = match req.conflicts {
                tl_app::Conflicts::Ask => {
                    let resolver = JobResolver::new(cx.job, cx.inbox.clone(), cx.cancel.clone());
                    transfer::run(&spec, &resolver, &cx.cancel, progress)?
                }
                tl_app::Conflicts::Overwrite => {
                    transfer::run(&spec, &FixedResolver(ConflictChoice::Overwrite), &cx.cancel, progress)?
                }
            };
            Ok(JobOutput::Transfer(report).into())
        }
        Work::Delete {
            conn,
            paths,
            trash,
            elevated,
        } => {
            let vfs = cx.vfs(conn)?;
            let ctx = cx.op(elevated);
            if trash {
                vfs.trash(&paths, &ctx, progress)?;
            } else {
                vfs.delete(&paths, &ctx, progress)?;
            }
            Ok(JobOutput::Done.into())
        }
        Work::MakeDirs {
            conn,
            base,
            parts,
            elevated,
        } => {
            let vfs = cx.vfs(conn)?;
            let ctx = cx.op(elevated);
            let mut path = base;
            for part in parts.iter().filter(|p| !p.is_empty()) {
                cx.cancel.check()?;
                path = path.child(part);
                if vfs.stat(&path, &ctx)?.is_none() {
                    vfs.mkdir(&path, &ctx)?;
                }
            }
            Ok(JobOutput::Done.into())
        }
        Work::CreateFile { conn, path, elevated } => {
            cx.vfs(conn)?.create_file(&path, &cx.op(elevated))?;
            Ok(JobOutput::Done.into())
        }
        Work::Rename {
            conn,
            from,
            to,
            elevated,
        } => {
            cx.vfs(conn)?.rename(&from, &to, &cx.op(elevated))?;
            Ok(JobOutput::Done.into())
        }
        Work::Renames {
            conn,
            dir,
            steps,
            elevated,
        } => {
            let vfs = cx.vfs(conn)?;
            tl_core::multirename::apply(&*vfs, &dir, &steps, &cx.op(elevated), progress)?;
            Ok(JobOutput::Done.into())
        }
        Work::WriteFile {
            conn,
            path,
            data,
            elevated,
        } => {
            cx.vfs(conn)?.write_file(&path, &data, &cx.op(elevated))?;
            Ok(JobOutput::Done.into())
        }
        Work::Attributes {
            conn,
            paths,
            mode,
            owner,
            group,
            recursive,
            elevated,
        } => {
            let vfs = cx.vfs(conn)?;
            let ctx = cx.op(elevated);
            if owner.is_some() || group.is_some() {
                vfs.chown(&paths, owner.as_deref(), group.as_deref(), recursive, &ctx)?;
            }
            if let Some(mode) = mode {
                vfs.chmod(&paths, mode, recursive, &ctx)?;
            }
            Ok(JobOutput::Done.into())
        }
        Work::Scan {
            conn,
            dir,
            names,
            elevated,
        } => {
            let vfs = cx.vfs(conn)?;
            let ctx = cx.op(elevated);
            let mut sizes = Vec::with_capacity(names.len());
            for name in names {
                cx.cancel.check()?;
                progress.set_current(String::from_utf8_lossy(&name).into_owned());
                let totals = vfs.scan(&[dir.child(&name)], &ctx)?;
                sizes.push((name, totals));
            }
            Ok(JobOutput::Sizes(sizes).into())
        }
        Work::Find {
            conn,
            root,
            masks,
            limit,
            elevated,
        } => {
            let vfs = cx.vfs(conn)?;
            let mut hits = Vec::new();
            vfs.find(&root, &masks, &cx.op(elevated), &mut |path, kind| {
                hits.push((path, kind));
                progress.set_done(0, hits.len() as u64);
                hits.len() < limit
            })?;
            hits.truncate(limit);
            Ok(JobOutput::Found(hits).into())
        }
        Work::Exec {
            conn,
            cwd,
            command,
            elevated,
        } => {
            let output = cx.vfs(conn)?.exec(&cwd, &command, &cx.op(elevated))?;
            Ok(JobOutput::Exec(output).into())
        }
        Work::Connect { conn, bookmark, sudo } => {
            let plugin = cx
                .registry
                .get(&bookmark.plugin)
                .ok_or_else(|| VfsError::other(format!("Unknown connection type “{}”", bookmark.plugin)))?;
            progress.set_phase("Connecting");
            progress.set_indeterminate(true);
            let vfs = plugin.connect(&bookmark, &cx.env, &cx.cancel)?;
            let initial = plugin.initial_path(&bookmark, &*vfs);
            if sudo && let Err(err) = vfs.prepare_elevation(&OpCtx::with_cancel(cx.cancel.clone())) {
                disconnect(vfs);
                return Err(err);
            }
            let info = conn_info(conn, &*vfs, Some(bookmark.id.clone()), None);
            Ok(JobDone {
                output: JobOutput::Connected { info, initial },
                register: Some((conn, vfs)),
            })
        }
        Work::SyncCompare { left, right, options } => {
            let (left, right) = (cx.sync_end(&left)?, cx.sync_end(&right)?);
            progress.set_phase("Comparing");
            progress.set_indeterminate(true);
            let plan = tl_core::sync::compare(&left, &right, options, &cx.cancel)?;
            Ok(JobOutput::Plan(plan).into())
        }
        Work::SyncApply { plan, left, right } => {
            let (left, right) = (cx.sync_end(&left)?, cx.sync_end(&right)?);
            let report = tl_core::sync::apply(&plan, &left, &right, &cx.cancel, progress)?;
            Ok(JobOutput::Synced(report).into())
        }
        Work::OpenCopy {
            conn,
            dir,
            name,
            elevated,
        } => {
            let vfs = cx.vfs(conn)?;
            let cache = cx
                .cache_dir
                .join(sanitize(&vfs.label()))
                .join(dir.display().trim_start_matches('/'));
            std::fs::create_dir_all(&cache).map_err(|err| VfsError::other(format!("Cannot create cache folder: {err}")))?;
            let local = cache.join(String::from_utf8_lossy(&name).as_ref());
            let _ = std::fs::remove_file(&local);
            let spec = TransferSpec {
                mode: tl_core::transfer::TransferMode::Copy,
                src: vfs,
                src_dir: dir,
                names: vec![name],
                src_elevated: elevated,
                dst: tl_local::local_vfs(),
                dst_dir: VPath::from_local(&cache),
                dst_elevated: false,
                target_name: None,
            };
            transfer::run(&spec, &FixedResolver(ConflictChoice::Overwrite), &cx.cancel, progress)?;
            cx.ui.open(&local);
            Ok(JobOutput::Done.into())
        }
    }
}

/// Closes a server connection without holding anyone up.
pub(crate) fn disconnect(vfs: Arc<dyn Vfs>) {
    let spawned = std::thread::Builder::new()
        .name("tl-disconnect".into())
        .spawn(move || vfs.disconnect());
    if let Err(err) = spawned {
        log::error!("cannot start a thread to disconnect: {err}");
    }
}

/// A connection label as a folder name in the cache.
pub(crate) fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || "-_.@".contains(c) { c } else { '_' })
        .collect()
}

// ----- the desktop -----------------------------------------------------------------------

/// Home, Desktop, Documents, Downloads (the folders that exist), then the
/// mounted volumes.
pub(crate) fn local_folders() -> Vec<LocalFolder> {
    let mut out = Vec::new();
    if let Some(home) = local_home().map(PathBuf::from) {
        let home_path = VPath::from_local(&home);
        out.push(LocalFolder {
            label: "Home".into(),
            path: home_path.clone(),
            volume: false,
        });
        for name in ["Desktop", "Documents", "Downloads"] {
            if home.join(name).is_dir() {
                out.push(LocalFolder {
                    label: name.into(),
                    path: home_path.child(name.as_bytes()),
                    volume: false,
                });
            }
        }
    }
    for (name, path) in tl_local::volumes() {
        let label = if path.is_root() { "Root (/)".to_string() } else { name };
        out.push(LocalFolder {
            label,
            path,
            volume: true,
        });
    }
    out
}

/// Opens a terminal in `path` of `vfs`: a local window there, or the
/// provider's login command.
pub(crate) fn open_terminal(
    vfs: &dyn Vfs,
    path: &VPath,
    elevated: bool,
    preferred: &str,
    scripts: &std::path::Path,
) -> std::io::Result<()> {
    let preferred = Some(preferred);
    if vfs.plugin_id() == tl_local::PLUGIN_ID && vfs.host().is_none() {
        crate::terminal::open(Some(&path.to_local()), None, preferred, scripts)
    } else {
        match vfs.terminal_command(path, elevated) {
            Some(argv) => crate::terminal::open(None, Some(&argv), preferred, scripts),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_become_folder_names() {
        assert_eq!(sanitize("deploy@web-1:2222/x y"), "deploy@web-1_2222_x_y");
    }
}
