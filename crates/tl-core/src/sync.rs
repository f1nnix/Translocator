//! Synchronizing two folders: listing both ends and running a reviewed plan
//! through the transfer engine, so a sync between two folders on one server
//! runs as server-side copies. Planning is pure and lives in
//! [`tl_model::sync`].

use std::collections::BTreeMap;
use std::sync::Arc;

pub use tl_model::sync::*;

use crate::cancel::CancelToken;
use crate::error::VfsResult;
use crate::path::VPath;
use crate::progress::Progress;
use crate::transfer::{self, PlannedDir, PlannedItem, TransferMode, TransferSpec};
use crate::vfs::{OpCtx, Vfs, WalkEntry};

/// One end of a sync.
#[derive(Clone)]
pub struct SyncEnd {
    pub vfs: Arc<dyn Vfs>,
    pub dir: VPath,
    pub elevated: bool,
}

fn listing(end: &SyncEnd, recursive: bool, cancel: &CancelToken) -> VfsResult<Vec<WalkEntry>> {
    let ctx = OpCtx::with_cancel(cancel.clone()).elevated(end.elevated);
    if recursive {
        return end.vfs.walk(&end.dir, &ctx);
    }
    Ok(end
        .vfs
        .list(&end.dir, &ctx)?
        .into_iter()
        .map(|e| WalkEntry {
            rel: e.name,
            kind: e.kind,
            size: e.size,
            mtime: e.mtime,
        })
        .collect())
}

/// Lists both folders (recursively when asked) and plans the sync.
pub fn compare(left: &SyncEnd, right: &SyncEnd, options: SyncOptions, cancel: &CancelToken) -> VfsResult<SyncPlan> {
    let l = listing(left, options.recursive, cancel)?;
    let r = listing(right, options.recursive, cancel)?;
    Ok(plan(&l, &r, options))
}

/// The planned copies in one direction, grouped by folder.
fn planned_dirs(plan: &SyncPlan, action: SyncAction) -> Vec<PlannedDir> {
    let mut dirs: BTreeMap<Vec<u8>, Vec<PlannedItem>> = BTreeMap::new();
    for item in plan.items.iter().filter(|i| i.action == action) {
        let (source, existing) = match action {
            SyncAction::ToRight => (&item.left, &item.right),
            SyncAction::ToLeft => (&item.right, &item.left),
            SyncAction::Skip => continue,
        };
        let Some(source) = source else { continue };
        dirs.entry(tl_model::sync::parent_rel(&item.rel).to_vec()).or_default().push(PlannedItem {
            source: source.entry(),
            existing: existing.as_ref().map(SyncSide::entry),
        });
    }
    dirs.into_iter().map(|(rel, items)| PlannedDir { rel, items }).collect()
}

/// Copies what the plan says, left to right first. The folders are compared
/// again first; if anything to be copied changed, nothing is copied.
pub fn apply(
    plan: &SyncPlan,
    left: &SyncEnd,
    right: &SyncEnd,
    cancel: &CancelToken,
    progress: &Progress,
) -> VfsResult<SyncReport> {
    progress.set_phase("Comparing again");
    progress.set_indeterminate(true);
    let fresh = compare(left, right, plan.options, cancel)?;
    check_unchanged(plan, &fresh)?;

    let mut report = SyncReport::default();
    for (action, src, dst) in [(SyncAction::ToRight, left, right), (SyncAction::ToLeft, right, left)] {
        let dirs = planned_dirs(plan, action);
        if dirs.is_empty() {
            continue;
        }
        cancel.check()?;
        progress.set_done(0, 0);
        let spec = TransferSpec {
            mode: TransferMode::Copy,
            src: src.vfs.clone(),
            src_dir: src.dir.clone(),
            names: Vec::new(),
            src_elevated: src.elevated,
            dst: dst.vfs.clone(),
            dst_dir: dst.dir.clone(),
            dst_elevated: dst.elevated,
            target_name: None,
        };
        let done = transfer::run_planned(&spec, dirs, cancel, progress)?;
        report.server_side = done.server_side;
        match action {
            SyncAction::ToRight => report.to_right = done.transferred,
            _ => report.to_left = done.transferred,
        }
    }
    Ok(report)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::EntryKind;

    fn f(rel: &str, size: u64, mtime: i64) -> WalkEntry {
        WalkEntry {
            rel: rel.as_bytes().to_vec(),
            kind: EntryKind::File,
            size,
            mtime: Some(mtime),
        }
    }

    #[test]
    fn planned_copies_are_grouped_by_folder() {
        let left = vec![f("top", 1, 5), f("docs/a.txt", 1, 1), f("docs/b.txt", 2, 9)];
        let right = vec![f("docs/a.txt", 1, 9), f("docs/c.txt", 3, 1), f("docs/b.txt", 2, 1)];
        let plan = plan(&left, &right, SyncOptions::default());
        let dirs = planned_dirs(&plan, SyncAction::ToLeft);
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].rel, b"docs");
        assert!(dirs[0].items.iter().any(|i| i.source.name == b"a.txt" && i.existing.is_some()));
        assert!(dirs[0].items.iter().any(|i| i.source.name == b"c.txt" && i.existing.is_none()));
        let dirs = planned_dirs(&plan, SyncAction::ToRight);
        let rels: Vec<&[u8]> = dirs.iter().map(|d| d.rel.as_slice()).collect();
        assert_eq!(rels, [&b""[..], &b"docs"[..]]);
    }
}
