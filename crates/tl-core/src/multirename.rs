//! Multi-rename, the I/O half: applying planned renames in a folder, and
//! the local calendar fields the masks' date placeholders read. Masks,
//! previews and step planning are [`tl_model::multirename`].

pub use tl_model::multirename::*;

use crate::cancel::CancelToken;
use crate::error::{VfsError, VfsResult};
use crate::path::VPath;
use crate::progress::Progress;
use crate::vfs::{OpCtx, Vfs};

/// Local calendar fields of a Unix time.
pub fn local_time(secs: i64) -> Option<[i64; 6]> {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return None;
    }
    Some([
        tm.tm_year as i64 + 1900,
        tm.tm_mon as i64 + 1,
        tm.tm_mday as i64,
        tm.tm_hour as i64,
        tm.tm_min as i64,
        tm.tm_sec as i64,
    ])
}

/// Performs `steps` in `dir`. If one fails (or the job is cancelled), the
/// renames already done are undone, so the folder is left as it was.
pub fn apply(vfs: &dyn Vfs, dir: &VPath, steps: &[Rename], ctx: &OpCtx, progress: &Progress) -> VfsResult<()> {
    progress.set_totals(0, steps.len() as u64);
    let paths: Vec<(VPath, VPath)> = steps.iter().map(|(a, b)| (dir.child(a), dir.child(b))).collect();
    let mut done = 0;
    let result = vfs.rename_all(&paths, ctx, &mut done);
    progress.set_done(0, done as u64);
    let Err(err) = result else { return Ok(()) };
    let undo: Vec<(VPath, VPath)> = paths[..done.min(paths.len())]
        .iter()
        .rev()
        .map(|(from, to)| (to.clone(), from.clone()))
        .collect();
    let undo_ctx = OpCtx {
        cancel: CancelToken::new(),
        ..ctx.clone()
    };
    let mut undone = 0;
    match vfs.rename_all(&undo, &undo_ctx, &mut undone) {
        Ok(()) => Err(err),
        Err(undo_err) => Err(VfsError::other(format!(
            "{err}\nUndoing the {done} renames already done failed too: {undo_err}"
        ))),
    }
}

