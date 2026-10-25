//! Synchronizing two folders: a preview of what would be copied each way,
//! then the copies themselves through the transfer engine, so a sync
//! between two folders on one server runs as server-side copies.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::cancel::CancelToken;
use crate::compare::{Diff, diff_files};
use crate::entry::{Entry, EntryKind};
use crate::error::{VfsError, VfsResult};
use crate::path::VPath;
use crate::progress::Progress;
use crate::transfer::{self, PlannedDir, PlannedItem, TransferMode, TransferSpec};
use crate::util::name_key;
use crate::vfs::{OpCtx, Vfs, WalkEntry};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SyncDirection {
    #[default]
    Both,
    LeftToRight,
    RightToLeft,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncOptions {
    /// Compare subfolders too. Without it, folders that exist on both
    /// sides are left alone.
    pub recursive: bool,
    pub direction: SyncDirection,
    /// Names that differ only in case are the same file on that side.
    pub left_case_insensitive: bool,
    pub right_case_insensitive: bool,
}

impl SyncOptions {
    fn matching_case_insensitive(&self) -> bool {
        self.left_case_insensitive && self.right_case_insensitive
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    OnlyLeft,
    OnlyRight,
    LeftNewer,
    RightNewer,
    /// Same time but different size, a file facing a folder, or a name
    /// that would collide with a differently-cased one on the other side.
    Conflict,
    Same,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncAction {
    Skip,
    ToRight,
    ToLeft,
}

/// One side of a [`SyncItem`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncSide {
    /// The name on this side (case can differ from the other side's).
    pub name: Vec<u8>,
    pub kind: EntryKind,
    /// For a folder that exists on this side only: the bytes below it.
    pub size: u64,
    pub mtime: Option<i64>,
    /// For a folder that exists on this side only: the files below it.
    pub files: u64,
}

impl SyncSide {
    fn entry(&self) -> Entry {
        let mut entry = Entry::new(self.name.clone(), self.kind);
        entry.size = self.size;
        entry.mtime = self.mtime;
        entry
    }

    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }
}

#[derive(Clone, Debug)]
pub struct SyncItem {
    /// Path below both roots (with the left side's spelling when both
    /// sides have it).
    pub rel: Vec<u8>,
    pub left: Option<SyncSide>,
    pub right: Option<SyncSide>,
    pub state: SyncState,
    pub action: SyncAction,
}

impl SyncItem {
    /// Whether `action` is possible: copying needs a source.
    pub fn allows(&self, action: SyncAction) -> bool {
        match action {
            SyncAction::Skip => true,
            SyncAction::ToRight => self.left.is_some(),
            SyncAction::ToLeft => self.right.is_some(),
        }
    }

    /// The side that would be copied.
    pub fn source(&self) -> Option<&SyncSide> {
        match self.action {
            SyncAction::Skip => None,
            SyncAction::ToRight => self.left.as_ref(),
            SyncAction::ToLeft => self.right.as_ref(),
        }
    }

    /// Cycles to the next possible action (for clicking the direction).
    pub fn next_action(&self) -> SyncAction {
        let order = [SyncAction::ToRight, SyncAction::ToLeft, SyncAction::Skip];
        let start = order.iter().position(|a| *a == self.action).unwrap_or(2);
        (1..=3)
            .map(|i| order[(start + i) % 3])
            .find(|a| self.allows(*a))
            .unwrap_or(SyncAction::Skip)
    }

    pub fn rel_lossy(&self) -> String {
        String::from_utf8_lossy(&self.rel).into_owned()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncTotals {
    pub to_right: usize,
    pub to_right_bytes: u64,
    pub to_left: usize,
    pub to_left_bytes: u64,
    pub conflicts: usize,
    pub same: usize,
}

#[derive(Clone, Debug, Default)]
pub struct SyncPlan {
    pub options: SyncOptions,
    pub items: Vec<SyncItem>,
}

impl SyncPlan {
    pub fn totals(&self) -> SyncTotals {
        let mut t = SyncTotals::default();
        for item in &self.items {
            match item.state {
                SyncState::Conflict => t.conflicts += 1,
                SyncState::Same => t.same += 1,
                _ => {}
            }
            let bytes = item.source().map(|s| s.size).unwrap_or(0);
            match item.action {
                SyncAction::ToRight => {
                    t.to_right += 1;
                    t.to_right_bytes += bytes;
                }
                SyncAction::ToLeft => {
                    t.to_left += 1;
                    t.to_left_bytes += bytes;
                }
                SyncAction::Skip => {}
            }
        }
        t
    }

    /// Resets every item to the action the plan suggests for `direction`.
    pub fn set_direction(&mut self, direction: SyncDirection) {
        self.options.direction = direction;
        for item in &mut self.items {
            item.action = default_action(item.state, direction);
        }
    }
}

fn default_action(state: SyncState, direction: SyncDirection) -> SyncAction {
    let to_right = direction != SyncDirection::RightToLeft;
    let to_left = direction != SyncDirection::LeftToRight;
    match state {
        SyncState::OnlyLeft | SyncState::LeftNewer if to_right => SyncAction::ToRight,
        SyncState::OnlyRight | SyncState::RightNewer if to_left => SyncAction::ToLeft,
        _ => SyncAction::Skip,
    }
}

fn last_component(rel: &[u8]) -> &[u8] {
    rel.rsplit(|b| *b == b'/').next().unwrap_or(rel)
}

fn parent_rel(rel: &[u8]) -> &[u8] {
    match rel.iter().rposition(|b| *b == b'/') {
        Some(i) => &rel[..i],
        None => &[],
    }
}

/// `a/b/c` → `a`, `a/b`.
fn ancestors(rel: &[u8]) -> impl Iterator<Item = &[u8]> {
    rel.iter()
        .enumerate()
        .filter(|(_, b)| **b == b'/')
        .map(move |(i, _)| &rel[..i])
}

/// Bytes and file counts below each of `dirs` (relative paths).
fn subtree_totals(entries: &[WalkEntry], dirs: &HashSet<Vec<u8>>, ci: bool) -> HashMap<Vec<u8>, (u64, u64)> {
    let mut totals: HashMap<Vec<u8>, (u64, u64)> = HashMap::new();
    if dirs.is_empty() {
        return totals;
    }
    for entry in entries.iter().filter(|e| e.kind != EntryKind::Dir) {
        let key = name_key(&entry.rel, ci);
        for ancestor in ancestors(&key) {
            if dirs.contains(ancestor) {
                let t = totals.entry(ancestor.to_vec()).or_default();
                if entry.kind == EntryKind::File {
                    t.0 += entry.size;
                }
                t.1 += 1;
            }
        }
    }
    totals
}

/// Plans a sync from the two sides' listings (recursive `walk` output, or
/// plain listings for a shallow sync). Folders that exist on one side only
/// become a single item that copies the whole folder.
pub fn plan(left: &[WalkEntry], right: &[WalkEntry], options: SyncOptions) -> SyncPlan {
    let ci = options.matching_case_insensitive();
    let left_map: HashMap<Vec<u8>, &WalkEntry> = left.iter().map(|e| (name_key(&e.rel, ci), e)).collect();
    let right_map: HashMap<Vec<u8>, &WalkEntry> = right.iter().map(|e| (name_key(&e.rel, ci), e)).collect();
    let mut keys: Vec<&Vec<u8>> = left_map.keys().chain(right_map.keys().filter(|k| !left_map.contains_key(*k))).collect();
    // Parents sort before their children.
    keys.sort();

    // Lower-cased names on a case-insensitive side whose counterpart is
    // matched case-sensitively: a new file there could land on another.
    let folded = |entries: &[WalkEntry], side_ci: bool| -> HashSet<Vec<u8>> {
        if side_ci && !ci {
            entries.iter().map(|e| name_key(&e.rel, true)).collect()
        } else {
            HashSet::new()
        }
    };
    let left_folded = folded(left, options.left_case_insensitive);
    let right_folded = folded(right, options.right_case_insensitive);

    let mut collapsed: HashSet<Vec<u8>> = HashSet::new();
    let mut one_sided_left: HashSet<Vec<u8>> = HashSet::new();
    let mut one_sided_right: HashSet<Vec<u8>> = HashSet::new();
    let mut items = Vec::new();
    let side = |e: &WalkEntry| SyncSide {
        name: last_component(&e.rel).to_vec(),
        kind: e.kind,
        size: if e.kind == EntryKind::Dir { 0 } else { e.size },
        mtime: e.mtime,
        files: 0,
    };
    for key in keys {
        if ancestors(key).any(|a| collapsed.contains(a)) {
            continue;
        }
        let (l, r) = (left_map.get(key), right_map.get(key));
        let state = match (l, r) {
            (Some(l), None) => {
                if l.kind == EntryKind::Dir {
                    collapsed.insert(key.clone());
                    one_sided_left.insert(key.clone());
                }
                let lower = name_key(&l.rel, true);
                if right_folded.contains(&lower) { SyncState::Conflict } else { SyncState::OnlyLeft }
            }
            (None, Some(r)) => {
                if r.kind == EntryKind::Dir {
                    collapsed.insert(key.clone());
                    one_sided_right.insert(key.clone());
                }
                let lower = name_key(&r.rel, true);
                if left_folded.contains(&lower) { SyncState::Conflict } else { SyncState::OnlyRight }
            }
            (Some(l), Some(r)) => match (l.kind == EntryKind::Dir, r.kind == EntryKind::Dir) {
                (true, true) => continue,
                (false, false) => match diff_files(l.size, l.mtime, r.size, r.mtime) {
                    Diff::Newer => SyncState::LeftNewer,
                    Diff::Older => SyncState::RightNewer,
                    Diff::Same | Diff::Unique => SyncState::Same,
                    Diff::Different => SyncState::Conflict,
                },
                _ => {
                    collapsed.insert(key.clone());
                    SyncState::Conflict
                }
            },
            (None, None) => continue,
        };
        items.push(SyncItem {
            rel: l.or(r).map(|e| e.rel.clone()).unwrap_or_default(),
            left: l.map(|e| side(e)),
            right: r.map(|e| side(e)),
            state,
            action: default_action(state, options.direction),
        });
    }

    let left_totals = subtree_totals(left, &one_sided_left, ci);
    let right_totals = subtree_totals(right, &one_sided_right, ci);
    for item in &mut items {
        let key = name_key(&item.rel, ci);
        for (side, totals) in [(&mut item.left, &left_totals), (&mut item.right, &right_totals)] {
            if let (Some(side), Some((bytes, files))) = (side.as_mut(), totals.get(&key)) {
                side.size = *bytes;
                side.files = *files;
            }
        }
    }
    SyncPlan { options, items }
}

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub to_right: usize,
    pub to_left: usize,
    /// Copies ran inside the provider (on the server).
    pub server_side: bool,
}

/// Fails if any item that is about to be copied changed since `plan` was
/// made: a stale preview must never overwrite something newer.
pub fn check_unchanged(plan: &SyncPlan, fresh: &SyncPlan) -> VfsResult<()> {
    let ci = plan.options.matching_case_insensitive();
    let now: HashMap<Vec<u8>, &SyncItem> = fresh.items.iter().map(|i| (name_key(&i.rel, ci), i)).collect();
    for item in plan.items.iter().filter(|i| i.action != SyncAction::Skip) {
        let same = now
            .get(&name_key(&item.rel, ci))
            .is_some_and(|n| n.left == item.left && n.right == item.right);
        if !same {
            return Err(VfsError::other(format!(
                "“{}” changed since the comparison. Compare again before synchronizing.",
                item.rel_lossy()
            )));
        }
    }
    Ok(())
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
        dirs.entry(parent_rel(&item.rel).to_vec()).or_default().push(PlannedItem {
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

    fn f(rel: &str, size: u64, mtime: i64) -> WalkEntry {
        WalkEntry {
            rel: rel.as_bytes().to_vec(),
            kind: EntryKind::File,
            size,
            mtime: Some(mtime),
        }
    }

    fn d(rel: &str) -> WalkEntry {
        WalkEntry {
            rel: rel.as_bytes().to_vec(),
            kind: EntryKind::Dir,
            size: 4096,
            mtime: Some(1),
        }
    }

    fn summary(plan: &SyncPlan) -> Vec<(String, SyncState, SyncAction)> {
        plan.items.iter().map(|i| (i.rel_lossy(), i.state, i.action)).collect()
    }

    #[test]
    fn plans_both_ways_and_collapses_one_sided_folders() {
        let left = vec![
            d("docs"),
            f("docs/a.txt", 1, 100),
            f("docs/b.txt", 2, 500),
            d("only-left"),
            d("only-left/deep"),
            f("only-left/deep/x", 10, 1),
            f("only-left/y", 5, 1),
            f("same.txt", 3, 100),
            f("clash", 3, 100),
            f("kind", 3, 100),
        ];
        let right = vec![
            d("docs"),
            f("docs/a.txt", 1, 900),
            f("docs/b.txt", 2, 100),
            f("docs/c.txt", 7, 100),
            f("same.txt", 3, 101),
            f("clash", 4, 100),
            d("kind"),
            f("kind/inner", 1, 1),
        ];
        let plan = plan(&left, &right, SyncOptions { recursive: true, ..Default::default() });
        assert_eq!(
            summary(&plan),
            [
                ("clash".into(), SyncState::Conflict, SyncAction::Skip),
                ("docs/a.txt".into(), SyncState::RightNewer, SyncAction::ToLeft),
                ("docs/b.txt".into(), SyncState::LeftNewer, SyncAction::ToRight),
                ("docs/c.txt".into(), SyncState::OnlyRight, SyncAction::ToLeft),
                ("kind".into(), SyncState::Conflict, SyncAction::Skip),
                ("only-left".into(), SyncState::OnlyLeft, SyncAction::ToRight),
                ("same.txt".into(), SyncState::Same, SyncAction::Skip),
            ]
        );
        let folder = plan.items.iter().find(|i| i.rel == b"only-left").unwrap();
        assert_eq!(folder.left.as_ref().map(|s| (s.size, s.files)), Some((15, 2)));
        let t = plan.totals();
        assert_eq!((t.to_right, t.to_right_bytes, t.to_left, t.to_left_bytes), (2, 17, 2, 8));
        assert_eq!((t.conflicts, t.same), (2, 1));

        let dirs = planned_dirs(&plan, SyncAction::ToLeft);
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].rel, b"docs");
        assert!(dirs[0].items.iter().any(|i| i.source.name == b"a.txt" && i.existing.is_some()));
        assert!(dirs[0].items.iter().any(|i| i.source.name == b"c.txt" && i.existing.is_none()));
        let dirs = planned_dirs(&plan, SyncAction::ToRight);
        let rels: Vec<&[u8]> = dirs.iter().map(|d| d.rel.as_slice()).collect();
        assert_eq!(rels, [&b""[..], &b"docs"[..]]);
    }

    #[test]
    fn one_way_and_manual_actions() {
        let left = vec![f("new-left", 1, 1), f("older-here", 1, 100)];
        let right = vec![f("new-right", 1, 1), f("older-here", 1, 900)];
        let mut plan = plan(&left, &right, SyncOptions { direction: SyncDirection::LeftToRight, ..Default::default() });
        assert_eq!(
            summary(&plan),
            [
                ("new-left".into(), SyncState::OnlyLeft, SyncAction::ToRight),
                ("new-right".into(), SyncState::OnlyRight, SyncAction::Skip),
                ("older-here".into(), SyncState::RightNewer, SyncAction::Skip),
            ]
        );
        // Clicking cycles through the possible directions only.
        let only_right = &plan.items[1];
        assert_eq!(only_right.next_action(), SyncAction::ToLeft);
        assert!(!only_right.allows(SyncAction::ToRight));
        let both = &mut plan.items[2];
        assert_eq!(both.next_action(), SyncAction::ToRight);
        both.action = SyncAction::ToRight;
        assert_eq!(both.next_action(), SyncAction::ToLeft);
        plan.set_direction(SyncDirection::Both);
        assert_eq!(plan.items[2].action, SyncAction::ToLeft);
    }

    #[test]
    fn case_rules() {
        let left = vec![f("README", 1, 1)];
        let right = vec![f("readme", 1, 1)];
        let both_ci = SyncOptions {
            left_case_insensitive: true,
            right_case_insensitive: true,
            ..Default::default()
        };
        assert_eq!(summary(&plan(&left, &right, both_ci))[0].1, SyncState::Same);
        // A Mac folder against a Linux one: "readme" would land on "README".
        let mac_left = SyncOptions {
            left_case_insensitive: true,
            ..Default::default()
        };
        let s = summary(&plan(&left, &right, mac_left));
        assert_eq!(s[0], ("README".into(), SyncState::OnlyLeft, SyncAction::ToRight));
        assert_eq!(s[1], ("readme".into(), SyncState::Conflict, SyncAction::Skip));
    }

    #[test]
    fn stale_plans_are_refused() {
        let left = vec![f("a", 1, 100)];
        let right = vec![];
        let plan1 = plan(&left, &right, SyncOptions::default());
        assert!(check_unchanged(&plan1, &plan(&left, &right, SyncOptions::default())).is_ok());
        let changed = vec![f("a", 2, 100)];
        assert!(check_unchanged(&plan1, &plan(&changed, &right, SyncOptions::default())).is_err());
        let appeared = vec![f("a", 9, 999)];
        assert!(check_unchanged(&plan1, &plan(&left, &appeared, SyncOptions::default())).is_err());
    }
}
