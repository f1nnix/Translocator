//! Copy/move between panes.
//!
//! The engine first resolves name conflicts with the user, then picks a
//! strategy:
//!
//! * **Same location** (equal [`Vfs::location_key`]): the provider does the
//!   work itself — on SSH that is a plain `cp -a` / `mv` on the server, so no
//!   data ever travels through this machine.
//! * **Different locations**: the source streams a tar archive, the engine
//!   rewrites it on the fly (renames, merge filters) and feeds it to the
//!   destination's unpacker. No temporary files; progress is exact.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

use crate::cancel::CancelToken;
use crate::entry::{Entry, EntryKind};
use crate::error::{VfsError, VfsResult};
use crate::path::VPath;
use crate::progress::Progress;
use crate::util::{name_key, unique_name};
use crate::vfs::{
    MergePolicy, OpCtx, OwnerHint, Resolution, TarSink, TarSource, TransferItem, Vfs, WalkEntry,
};

/// Existing regular files up to this size are overwritten in place, which
/// keeps their owner, mode and hard links (think `/etc/nginx/nginx.conf`
/// uploaded through sudo).
const IN_PLACE_OVERWRITE_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferMode {
    Copy,
    Move,
}

pub struct TransferSpec {
    pub mode: TransferMode,
    pub src: Arc<dyn Vfs>,
    pub src_dir: VPath,
    pub names: Vec<Vec<u8>>,
    pub src_elevated: bool,
    pub dst: Arc<dyn Vfs>,
    pub dst_dir: VPath,
    pub dst_elevated: bool,
    /// New name for a single-item transfer ("copy as"). Ignored otherwise.
    pub target_name: Option<Vec<u8>>,
}

impl TransferSpec {
    pub fn is_server_side(&self) -> bool {
        self.src.location_key() == self.dst.location_key()
    }
}

/// An item about to land on something that already exists.
#[derive(Clone, Debug)]
pub struct Conflict {
    pub source: Entry,
    pub existing: Entry,
    pub dst_dir: VPath,
    /// Both sides are real directories, so the overwrite choices merge.
    pub both_dirs: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictChoice {
    /// File: replace it. Folder: merge, overwriting files inside.
    Overwrite,
    /// File: replace it if the source is newer. Folder: merge, overwriting
    /// only older files.
    OverwriteOlder,
    /// Folder: merge, keeping existing files. File: same as `Skip`.
    SkipExisting,
    Skip,
    /// Transfer under a new, unused name.
    KeepBoth,
    Cancel,
}

#[derive(Clone, Debug)]
pub struct ConflictAnswer {
    pub choice: ConflictChoice,
    pub apply_to_all: bool,
    /// Explicit name for `KeepBoth`; generated when `None`.
    pub rename_to: Option<Vec<u8>>,
}

impl ConflictAnswer {
    pub fn new(choice: ConflictChoice) -> Self {
        ConflictAnswer {
            choice,
            apply_to_all: false,
            rename_to: None,
        }
    }
}

/// Asks the user how to resolve a conflict. Called on the worker thread;
/// implementations block until the user answers.
pub trait ConflictResolver: Send + Sync {
    fn resolve(&self, conflict: &Conflict) -> ConflictAnswer;
}

/// Resolves every conflict the same way (tests, scripted use).
pub struct FixedResolver(pub ConflictChoice);

impl ConflictResolver for FixedResolver {
    fn resolve(&self, _conflict: &Conflict) -> ConflictAnswer {
        ConflictAnswer::new(self.0)
    }
}

#[derive(Clone, Debug, Default)]
pub struct TransferReport {
    pub transferred: usize,
    pub skipped: usize,
    pub server_side: bool,
    /// Final names of transferred items (to select them afterwards).
    pub names: Vec<Vec<u8>>,
}

/// A resolved item plus the bits the streaming path needs.
struct Planned {
    item: TransferItem,
    source: Entry,
    existing: Option<Entry>,
}

/// Items of one folder pair, ready to execute.
struct Group {
    src_dir: VPath,
    dst_dir: VPath,
    plan: Vec<Planned>,
}

fn contexts(spec: &TransferSpec, cancel: &CancelToken) -> (OpCtx, OpCtx) {
    let ctx = |elevated| OpCtx {
        elevated,
        owner_hint: OwnerHint::Natural,
        cancel: cancel.clone(),
    };
    (ctx(spec.src_elevated), ctx(spec.dst_elevated))
}

pub fn run(
    spec: &TransferSpec,
    resolver: &dyn ConflictResolver,
    cancel: &CancelToken,
    progress: &Progress,
) -> VfsResult<TransferReport> {
    progress.set_phase("Preparing");
    progress.set_indeterminate(true);
    let server_side = spec.is_server_side();
    let (src_ctx, dst_ctx) = contexts(spec, cancel);

    let sources = source_entries(spec, &src_ctx)?;
    if server_side {
        for source in &sources {
            let path = spec.src_dir.child(&source.name);
            if source.kind == EntryKind::Dir && spec.dst_dir.starts_with(&path) {
                return Err(VfsError::other(format!(
                    "Cannot {} “{}” into itself",
                    verb(spec.mode),
                    source.name_lossy()
                )));
            }
        }
    }

    let mut report = TransferReport {
        server_side,
        ..Default::default()
    };
    let plan = resolve_conflicts(spec, sources, resolver, &dst_ctx, &mut report)?;
    if plan.is_empty() {
        progress.set_indeterminate(false);
        return Ok(report);
    }
    cancel.check()?;
    report.transferred = plan.len();
    report.names = plan
        .iter()
        .map(|p| p.item.dst.file_name().unwrap_or_default().to_vec())
        .collect();

    let group = Group {
        src_dir: spec.src_dir.clone(),
        dst_dir: spec.dst_dir.clone(),
        plan,
    };
    execute(spec, vec![group], &src_ctx, &dst_ctx, progress)?;
    progress.set_indeterminate(false);
    Ok(report)
}

/// One folder of a transfer whose conflicts were settled beforehand (a
/// sync preview). `rel` is relative to the spec's `src_dir` and `dst_dir`.
#[derive(Clone, Debug)]
pub struct PlannedDir {
    pub rel: Vec<u8>,
    pub items: Vec<PlannedItem>,
}

#[derive(Clone, Debug)]
pub struct PlannedItem {
    /// The source entry; its name is kept at the destination.
    pub source: Entry,
    /// What the destination holds now, which gets replaced; `None` when
    /// the name is free.
    pub existing: Option<Entry>,
}

/// Runs a copy or move whose items and conflicts were decided ahead of
/// time: nothing is listed or asked, existing destinations are replaced.
/// `spec.names` and `spec.target_name` are ignored. Same-location
/// transfers still run server-side, as one provider call for all folders.
pub fn run_planned(
    spec: &TransferSpec,
    dirs: Vec<PlannedDir>,
    cancel: &CancelToken,
    progress: &Progress,
) -> VfsResult<TransferReport> {
    progress.set_phase("Preparing");
    progress.set_indeterminate(true);
    let (src_ctx, dst_ctx) = contexts(spec, cancel);
    let mut report = TransferReport {
        server_side: spec.is_server_side(),
        ..Default::default()
    };
    let mut groups = Vec::new();
    for dir in dirs.into_iter().filter(|d| !d.items.is_empty()) {
        let src_dir = spec.src_dir.join(&dir.rel);
        let dst_dir = spec.dst_dir.join(&dir.rel);
        let plan: Vec<Planned> = dir
            .items
            .into_iter()
            .map(|item| Planned {
                item: TransferItem {
                    src: src_dir.child(&item.source.name),
                    dst: dst_dir.child(&item.source.name),
                    kind: item.source.kind,
                    resolution: if item.existing.is_some() {
                        Resolution::Replace
                    } else {
                        Resolution::Create
                    },
                },
                source: item.source,
                existing: item.existing,
            })
            .collect();
        report.transferred += plan.len();
        groups.push(Group { src_dir, dst_dir, plan });
    }
    if !groups.is_empty() {
        cancel.check()?;
        execute(spec, groups, &src_ctx, &dst_ctx, progress)?;
    }
    progress.set_indeterminate(false);
    Ok(report)
}

/// Copies or moves resolved items: inside the provider when both ends
/// share a location, as tar streams otherwise.
fn execute(
    spec: &TransferSpec,
    groups: Vec<Group>,
    src_ctx: &OpCtx,
    dst_ctx: &OpCtx,
    progress: &Progress,
) -> VfsResult<()> {
    if !spec.is_server_side() {
        return stream_transfer(spec, groups, src_ctx, dst_ctx, progress);
    }
    let elevated = spec.src_elevated || spec.dst_elevated;
    let ctx = OpCtx {
        elevated,
        owner_hint: if elevated && !spec.dst_elevated && spec.mode == TransferMode::Copy {
            OwnerHint::LoginUser
        } else {
            OwnerHint::Natural
        },
        cancel: src_ctx.cancel.clone(),
    };
    let items: Vec<TransferItem> = groups
        .into_iter()
        .flat_map(|g| g.plan.into_iter().map(|p| p.item))
        .collect();
    progress.set_phase(match spec.mode {
        TransferMode::Copy => "Copying on server",
        TransferMode::Move => "Moving on server",
    });
    match spec.mode {
        TransferMode::Copy => spec.src.copy_within(&items, &ctx, progress),
        TransferMode::Move => spec.src.move_within(&items, &ctx, progress),
    }
}

fn verb(mode: TransferMode) -> &'static str {
    match mode {
        TransferMode::Copy => "copy",
        TransferMode::Move => "move",
    }
}

/// Entries of the selected names, in selection order.
fn source_entries(spec: &TransferSpec, ctx: &OpCtx) -> VfsResult<Vec<Entry>> {
    if spec.names.len() == 1 {
        let path = spec.src_dir.child(&spec.names[0]);
        return match spec.src.stat(&path, ctx)? {
            Some(entry) => Ok(vec![entry]),
            None => Err(VfsError::NotFound(path.display())),
        };
    }
    let mut listing: HashMap<Vec<u8>, Entry> = spec
        .src
        .list(&spec.src_dir, ctx)?
        .into_iter()
        .map(|e| (e.name.clone(), e))
        .collect();
    spec.names
        .iter()
        .map(|name| {
            listing
                .remove(name)
                .ok_or_else(|| VfsError::NotFound(spec.src_dir.child(name).display()))
        })
        .collect()
}

fn resolve_conflicts(
    spec: &TransferSpec,
    sources: Vec<Entry>,
    resolver: &dyn ConflictResolver,
    dst_ctx: &OpCtx,
    report: &mut TransferReport,
) -> VfsResult<Vec<Planned>> {
    let ci = spec.dst.capabilities().case_insensitive;
    let mut existing: HashMap<Vec<u8>, Entry> = spec
        .dst
        .list(&spec.dst_dir, dst_ctx)?
        .into_iter()
        .map(|e| (name_key(&e.name, ci), e))
        .collect();
    // Names this transfer will create, so "keep both" never collides with them.
    let mut claimed: HashSet<Vec<u8>> = HashSet::new();
    let mut sticky: Option<ConflictChoice> = None;
    let single = sources.len() == 1;
    let mut plan = Vec::with_capacity(sources.len());

    for source in sources {
        let src_path = spec.src_dir.child(&source.name);
        let target_name = match (&spec.target_name, single) {
            (Some(name), true) => name.clone(),
            _ => source.name.clone(),
        };
        let mut dst_path = spec.dst_dir.child(&target_name);
        let mut resolution = Resolution::Create;
        let found = existing.get(&name_key(&target_name, ci)).cloned();

        if let Some(found) = &found {
            let same_item = spec.is_server_side() && dst_path == src_path;
            if same_item && spec.mode == TransferMode::Move {
                report.skipped += 1;
                continue;
            }
            let both_dirs = source.kind == EntryKind::Dir && found.kind == EntryKind::Dir;
            let answer = if same_item {
                // Copying something onto itself: make a sibling copy.
                ConflictAnswer::new(ConflictChoice::KeepBoth)
            } else if let Some(choice) = sticky {
                ConflictAnswer::new(choice)
            } else {
                let answer = resolver.resolve(&Conflict {
                    source: source.clone(),
                    existing: found.clone(),
                    dst_dir: spec.dst_dir.clone(),
                    both_dirs,
                });
                if answer.apply_to_all {
                    sticky = Some(answer.choice);
                }
                answer
            };
            match answer.choice {
                ConflictChoice::Cancel => return Err(VfsError::Cancelled),
                ConflictChoice::Skip => {
                    report.skipped += 1;
                    continue;
                }
                ConflictChoice::SkipExisting if !both_dirs => {
                    report.skipped += 1;
                    continue;
                }
                ConflictChoice::SkipExisting => {
                    resolution = Resolution::Merge(MergePolicy::SkipExisting)
                }
                ConflictChoice::Overwrite if both_dirs => {
                    resolution = Resolution::Merge(MergePolicy::Overwrite)
                }
                ConflictChoice::Overwrite => resolution = Resolution::Replace,
                ConflictChoice::OverwriteOlder if both_dirs => {
                    resolution = Resolution::Merge(MergePolicy::OverwriteOlder)
                }
                ConflictChoice::OverwriteOlder => {
                    if is_newer(source.mtime, found.mtime) {
                        resolution = Resolution::Replace;
                    } else {
                        report.skipped += 1;
                        continue;
                    }
                }
                ConflictChoice::KeepBoth => {
                    let new_name = match answer.rename_to.filter(|n| !n.is_empty()) {
                        Some(name) => name,
                        None => unique_name(&target_name, source.is_dir(), |candidate| {
                            let key = name_key(candidate, ci);
                            existing.contains_key(&key) || claimed.contains(&key)
                        }),
                    };
                    if existing.contains_key(&name_key(&new_name, ci)) {
                        return Err(VfsError::AlreadyExists(
                            spec.dst_dir.child(&new_name).display(),
                        ));
                    }
                    dst_path = spec.dst_dir.child(&new_name);
                }
            }
        }

        let final_name = dst_path.file_name().unwrap_or_default().to_vec();
        claimed.insert(name_key(&final_name, ci));
        if resolution == Resolution::Create {
            // A later item with the same target name must see this one.
            existing.remove(&name_key(&final_name, ci));
        }
        plan.push(Planned {
            item: TransferItem {
                src: src_path,
                dst: dst_path,
                kind: source.kind,
                resolution,
            },
            source,
            existing: found.filter(|_| resolution != Resolution::Create),
        });
    }
    Ok(plan)
}

fn is_newer(src: Option<i64>, dst: Option<i64>) -> bool {
    match (src, dst) {
        (Some(s), Some(d)) => s > d,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Per-item state while streaming.
struct StreamItem {
    planned: Planned,
    /// Destination tree for filtered merges (relative path → node).
    dst_tree: Option<HashMap<Vec<u8>, WalkEntry>>,
    policy: Option<MergePolicy>,
    /// Something inside was skipped by the merge policy, so a move must not
    /// delete the whole source item.
    filtered: bool,
    /// Relative paths of files written (for filtered moves).
    files_done: Vec<Vec<u8>>,
    dirs_seen: Vec<Vec<u8>>,
}

fn stream_transfer(
    spec: &TransferSpec,
    groups: Vec<Group>,
    src_ctx: &OpCtx,
    dst_ctx: &OpCtx,
    progress: &Progress,
) -> VfsResult<()> {
    progress.set_phase("Scanning");
    let src_paths: Vec<VPath> = groups
        .iter()
        .flat_map(|g| g.plan.iter().map(|p| p.item.src.clone()))
        .collect();
    let totals = spec.src.scan(&src_paths, src_ctx)?;
    progress.set_totals(totals.bytes, totals.files.max(1));
    progress.set_indeterminate(false);
    for group in groups {
        stream_group(spec, group, src_ctx, dst_ctx, progress)?;
    }
    Ok(())
}

/// Streams one folder's items: one tar stream from `src_dir` into `dst_dir`.
fn stream_group(
    spec: &TransferSpec,
    group: Group,
    src_ctx: &OpCtx,
    dst_ctx: &OpCtx,
    progress: &Progress,
) -> VfsResult<()> {
    let cancel = &src_ctx.cancel;
    // Small regular files replacing regular files: rewrite in place.
    let mut streamed = Vec::new();
    for planned in group.plan {
        // Not into archives: they are rewritten anyway, and a stream keeps
        // the file's time.
        let in_place = planned.item.resolution == Resolution::Replace
            && spec.dst.host().is_none()
            && planned.source.kind == EntryKind::File
            && planned
                .existing
                .as_ref()
                .is_some_and(|e| e.kind == EntryKind::File)
            && planned.source.size <= IN_PLACE_OVERWRITE_LIMIT;
        if in_place {
            cancel.check()?;
            progress.set_phase("Uploading");
            progress.set_current(planned.source.name_lossy());
            let data = spec.src.read_file(&planned.item.src, u64::MAX, src_ctx)?;
            spec.dst.write_file(&planned.item.dst, &data, dst_ctx)?;
            progress.add_bytes(data.len() as u64);
            progress.add_items(1);
            if spec.mode == TransferMode::Move {
                spec.src
                    .delete(std::slice::from_ref(&planned.item.src), src_ctx, progress)?;
            }
        } else {
            streamed.push(planned);
        }
    }
    if streamed.is_empty() {
        return Ok(());
    }

    progress.set_phase("Preparing destination");
    let mut items = Vec::with_capacity(streamed.len());
    for planned in streamed {
        cancel.check()?;
        let mut dst_tree = None;
        let mut policy = None;
        match planned.item.resolution {
            Resolution::Replace => {
                spec.dst.delete(
                    std::slice::from_ref(&planned.item.dst),
                    dst_ctx,
                    &Progress::new(),
                )?;
            }
            Resolution::Merge(p) => {
                policy = Some(p);
                let tree = spec.dst.walk(&planned.item.dst, dst_ctx)?;
                dst_tree = Some(tree.into_iter().map(|w| (w.rel.clone(), w)).collect());
            }
            Resolution::Create => {}
        }
        items.push(StreamItem {
            planned,
            dst_tree,
            policy,
            filtered: false,
            files_done: Vec::new(),
            dirs_seen: Vec::new(),
        });
    }

    progress.set_phase(match spec.mode {
        TransferMode::Copy => "Copying",
        TransferMode::Move => "Moving",
    });
    let names: Vec<Vec<u8>> = items
        .iter()
        .map(|i| i.planned.item.src.file_name().unwrap_or_default().to_vec())
        .collect();
    let mut source = spec.src.export_tar(&group.src_dir, &names, src_ctx)?;
    let mut sink = match spec.dst.import_tar(&group.dst_dir, dst_ctx) {
        Ok(sink) => sink,
        Err(err) => {
            let _ = source.finish();
            return Err(err);
        }
    };

    let pumped = pump(&mut source, &mut sink, &mut items, cancel, progress);
    let sink_result = sink.finish();
    let source_result = source.finish();
    if cancel.is_cancelled() {
        return Err(VfsError::Cancelled);
    }
    // The unpacker's own error explains a broken pipe better than the pipe.
    sink_result?;
    source_result?;
    pumped?;

    if spec.mode == TransferMode::Move {
        progress.set_phase("Removing originals");
        let mut whole = Vec::new();
        for item in &items {
            if !item.filtered {
                whole.push(item.planned.item.src.clone());
                continue;
            }
            let root = &item.planned.item.src;
            let files: Vec<VPath> = item.files_done.iter().map(|rel| root.join(rel)).collect();
            for chunk in files.chunks(500) {
                spec.src.delete(chunk, src_ctx, &Progress::new())?;
            }
            let mut dirs: Vec<VPath> = item.dirs_seen.iter().map(|rel| root.join(rel)).collect();
            dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
            dirs.push(root.clone());
            spec.src.remove_empty_dirs(&dirs, src_ctx)?;
        }
        if !whole.is_empty() {
            spec.src.delete(&whole, src_ctx, &Progress::new())?;
        }
    }
    Ok(())
}

/// Splits a member path into its top-level name and the rest.
fn member_parts(raw: &[u8]) -> Option<(&[u8], &[u8])> {
    let mut rel = raw;
    while let Some(rest) = rel.strip_prefix(b"./") {
        rel = rest;
    }
    let rel = rel.trim_ascii_start();
    let rel = rel.strip_prefix(b"/").unwrap_or(rel);
    let rel = rel.strip_suffix(b"/").unwrap_or(rel);
    if rel.is_empty() || rel == b"." {
        return None;
    }
    Some(match rel.iter().position(|b| *b == b'/') {
        Some(i) => (&rel[..i], &rel[i + 1..]),
        None => (rel, &rel[rel.len()..]),
    })
}

fn renamed(new_top: &[u8], rest: &[u8]) -> Vec<u8> {
    let mut path = new_top.to_vec();
    if !rest.is_empty() {
        path.push(b'/');
        path.extend_from_slice(rest);
    }
    path
}

fn bytes_path(bytes: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(bytes))
}

/// Copies archive members from `source` to `sink`, renaming top-level names
/// and applying merge filters.
fn pump(
    source: &mut Box<dyn TarSource>,
    sink: &mut Box<dyn TarSink>,
    items: &mut [StreamItem],
    cancel: &CancelToken,
    progress: &Progress,
) -> VfsResult<()> {
    let index: HashMap<Vec<u8>, usize> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            (
                item.planned.item.src.file_name().unwrap_or_default().to_vec(),
                i,
            )
        })
        .collect();
    let new_tops: Vec<Vec<u8>> = items
        .iter()
        .map(|i| i.planned.item.dst.file_name().unwrap_or_default().to_vec())
        .collect();

    let mut archive = tar::Archive::new(source);
    let mut builder = tar::Builder::new(sink);
    let io_err = |e: io::Error| {
        if cancel.is_cancelled() {
            VfsError::Cancelled
        } else {
            VfsError::other(format!("Transfer failed: {e}"))
        }
    };

    for entry in archive.entries().map_err(io_err)? {
        cancel.check()?;
        let mut entry = entry.map_err(io_err)?;
        let raw = entry.path_bytes().into_owned();
        let Some((top, rest)) = member_parts(&raw) else {
            continue;
        };
        let Some(&idx) = index.get(top) else {
            log::warn!("unexpected archive member {:?}", String::from_utf8_lossy(&raw));
            continue;
        };
        let header = entry.header();
        let entry_type = header.entry_type();
        let is_dir = entry_type.is_dir();
        let mtime = header.mtime().ok().map(|m| m as i64);
        let item = &mut items[idx];

        if let (Some(tree), Some(policy)) = (&item.dst_tree, item.policy) {
            if rest.is_empty() {
                // The merge target directory exists; keep its attributes.
                continue;
            }
            if let Some(existing) = tree.get(rest) {
                if is_dir && existing.kind == EntryKind::Dir {
                    item.dirs_seen.push(rest.to_vec());
                    continue;
                }
                let keep_existing = match policy {
                    MergePolicy::Overwrite => false,
                    MergePolicy::SkipExisting => true,
                    MergePolicy::OverwriteOlder => !is_newer(mtime, existing.mtime),
                };
                if keep_existing {
                    item.filtered = true;
                    if !is_dir {
                        progress.add_bytes(entry.size());
                        progress.add_items(1);
                    }
                    continue;
                }
            }
        }

        let new_path = renamed(&new_tops[idx], rest);
        let mut out = tar::Header::new_gnu();
        out.set_entry_type(entry_type);
        out.set_mode(header.mode().unwrap_or(0o644));
        out.set_mtime(header.mtime().unwrap_or(0));
        out.set_uid(header.uid().unwrap_or(0));
        out.set_gid(header.gid().unwrap_or(0));
        if let Ok(Some(name)) = header.username() {
            let _ = out.set_username(name);
        }
        if let Ok(Some(name)) = header.groupname() {
            let _ = out.set_groupname(name);
        }

        match entry_type {
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                progress.set_current(String::from_utf8_lossy(&new_path).into_owned());
                out.set_entry_type(tar::EntryType::Regular);
                out.set_size(entry.size());
                let counted = CountingReader {
                    inner: &mut entry,
                    progress,
                    cancel,
                };
                builder
                    .append_data(&mut out, bytes_path(&new_path), counted)
                    .map_err(io_err)?;
                progress.add_items(1);
                item.files_done.push(rest.to_vec());
            }
            tar::EntryType::Directory => {
                out.set_size(0);
                builder
                    .append_data(&mut out, bytes_path(&new_path), io::empty())
                    .map_err(io_err)?;
                if !rest.is_empty() {
                    item.dirs_seen.push(rest.to_vec());
                }
            }
            tar::EntryType::Symlink | tar::EntryType::Link => {
                let target = entry
                    .link_name_bytes()
                    .map(|t| t.into_owned())
                    .unwrap_or_default();
                let target = if entry_type == tar::EntryType::Link {
                    // Hard links name another archive member: rename it too.
                    match member_parts(&target) {
                        Some((link_top, link_rest)) => match index.get(link_top) {
                            Some(&j) => renamed(&new_tops[j], link_rest),
                            None => target,
                        },
                        None => target,
                    }
                } else {
                    target
                };
                out.set_size(0);
                builder
                    .append_link(&mut out, bytes_path(&new_path), bytes_path(&target))
                    .map_err(io_err)?;
                progress.add_items(1);
                item.files_done.push(rest.to_vec());
            }
            tar::EntryType::Fifo | tar::EntryType::Char | tar::EntryType::Block => {
                if let (Ok(Some(major)), Ok(Some(minor))) =
                    (header.device_major(), header.device_minor())
                {
                    let _ = out.set_device_major(major);
                    let _ = out.set_device_minor(minor);
                }
                out.set_size(0);
                builder
                    .append_data(&mut out, bytes_path(&new_path), io::empty())
                    .map_err(io_err)?;
                item.files_done.push(rest.to_vec());
            }
            _ => {}
        }
    }
    builder.finish().map_err(io_err)?;
    Ok(())
}

/// Counts file bytes into the progress and aborts promptly on cancel.
struct CountingReader<'a, R: Read> {
    inner: R,
    progress: &'a Progress,
    cancel: &'a CancelToken,
}

impl<R: Read> Read for CountingReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = self.inner.read(buf)?;
        self.progress.add_bytes(n as u64);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_paths() {
        assert_eq!(member_parts(b"./a/b/c"), Some((&b"a"[..], &b"b/c"[..])));
        assert_eq!(member_parts(b"a/"), Some((&b"a"[..], &b""[..])));
        assert_eq!(member_parts(b"./"), None);
        assert_eq!(member_parts(b"/abs/x"), Some((&b"abs"[..], &b"x"[..])));
        assert_eq!(renamed(b"new", b"x/y"), b"new/x/y");
        assert_eq!(renamed(b"new", b""), b"new");
    }
}
