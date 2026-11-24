//! The column view, the way Finder shows one: the folders above the
//! pane's folder as columns to its left, and the folder under the cursor
//! as a column to its right.
//!
//! The pane's own folder stays the one every command acts on, with its
//! listing, cursor, marks and filter; it is the column with the keyboard.
//! The others are read-only listings, kept per tab and by path in
//! [`Columns`]. They come from the listings the pane already had as it
//! walked (leaving a folder keeps its listing as a column), so walking
//! left and right lists nothing twice, and walking into a column lands on
//! its rows at once while the folder is listed again behind them.
//!
//! The leftmost column is the root: the home folder when the path is
//! inside it, the archive when the pane is in one, else `/`. A path that
//! leaves the root (going up from it, or jumping elsewhere) finds a new
//! one.
//!
//! The cursor moves faster than a folder can be listed, above all over a
//! connection, so the column to the right is listed only once the cursor
//! has sat still for [`SETTLE_MS`], unless it is already known.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crux_core::Command;
use tl_model::{VPath, VfsError};

use crate::capabilities::*;
use crate::ids::{ConnId, TabId};
use crate::model::{Model, Reply, shell};
use crate::pane::{self, Display, Listing, Side, Tab};
use crate::settings::ViewMode;
use crate::{Cmd, Out};

/// How long the cursor has to sit on a folder before it is listed.
pub const SETTLE_MS: u64 = 150;

/// What a tab knows of the folders around its own, in the column view.
#[derive(Clone, Debug, Default)]
pub struct Columns {
    /// The leftmost column's folder; `None` until the view is first shown.
    pub root: Option<VPath>,
    /// Listings of the folders shown as columns, by path.
    pub cache: HashMap<VPath, Column>,
    /// The folder under the cursor that is waiting to be listed.
    waiting: Option<VPath>,
    /// Bumped on every wait; a wait that comes back older is dropped.
    generation: u64,
    /// The folder the columns' `selected` rows were found for.
    synced: Option<VPath>,
}

/// One column's folder.
#[derive(Clone, Debug, Default)]
pub struct Column {
    pub listing: Listing,
    /// Indices into `listing`, as the pane would show them unfiltered.
    pub order: Arc<Vec<u32>>,
    pub loading: bool,
    pub error: Option<String>,
    /// The row of the folder the path goes on into.
    pub selected: Option<usize>,
    /// Unique among every column ever made, and new whenever the rows
    /// change: what the shell caches rows by.
    pub revision: u64,
}

static REVISION: AtomicU64 = AtomicU64::new(1);

fn revision() -> u64 {
    REVISION.fetch_add(1, Ordering::Relaxed)
}

impl Column {
    fn loading() -> Column {
        Column {
            loading: true,
            revision: revision(),
            ..Column::default()
        }
    }

    fn of(listing: Listing, display: Display) -> Column {
        let mut column = Column {
            listing,
            revision: revision(),
            ..Column::default()
        };
        column.sort(display);
        column
    }

    fn sort(&mut self, display: Display) {
        let (order, _) = pane::build_order(&self.listing, display.sort, display.folders_first, display.show_hidden, true, "");
        self.order = Arc::new(order);
        self.revision = revision();
    }

    /// The row of the entry called `name`.
    fn row_of(&self, name: &[u8]) -> Option<usize> {
        self.order
            .iter()
            .position(|ix| self.listing.entries.get(*ix as usize).is_some_and(|e| e.name == name))
    }

    /// The entry name behind `row`.
    pub fn name_at(&self, row: usize) -> Option<&[u8]> {
        let ix = *self.order.get(row)?;
        self.listing.entries.get(ix as usize).map(|e| e.name.as_slice())
    }
}

#[derive(Debug)]
pub enum ColumnsReply {
    Listed { tab: TabId, path: VPath, out: Box<FsOutput> },
    /// The cursor has sat still since the wait `generation` began.
    Settled { tab: TabId, generation: u64 },
}

impl Columns {
    /// Forgets everything: the pane moved to another file system, or the
    /// view was switched on.
    pub fn reset(&mut self) {
        *self = Columns {
            generation: self.generation + 1,
            ..Columns::default()
        };
    }

    /// Keeps the listing of the folder a tab is leaving, so it can be shown
    /// as a column (or walked back into) without listing it again.
    pub fn stash(tab: &mut Tab, display: Display) {
        if tab.error.is_some() || (tab.loading && tab.listing.entries.is_empty()) {
            return;
        }
        let column = Column::of(tab.listing.clone(), display);
        tab.columns.cache.insert(tab.loc.path.clone(), column);
        tab.columns.synced = None;
    }

    /// The listing a tab has just arrived in, if a column holds it.
    pub fn known(&self, path: &VPath) -> Option<&Listing> {
        self.cache
            .get(path)
            .filter(|c| !c.loading && c.error.is_none())
            .map(|c| &c.listing)
    }

    /// The sort or the hidden files changed.
    pub fn resort(&mut self, display: Display) {
        for column in self.cache.values_mut().filter(|c| !c.loading) {
            column.sort(display);
        }
        self.synced = None;
    }
}

/// The folders shown left of `path`, leftmost first: `root` and those
/// between it and `path`.
pub fn ancestors(root: &VPath, path: &VPath) -> Vec<VPath> {
    let mut out = Vec::new();
    let mut at = path.parent();
    while let Some(p) = at {
        if !p.starts_with(root) {
            break;
        }
        at = p.parent();
        out.push(p);
    }
    out.reverse();
    out
}

/// The folder the cursor is on, for the column to the right.
pub fn preview_path(tab: &Tab) -> Option<VPath> {
    let entry = tab.cursor_entry()?;
    entry.is_dir().then(|| tab.loc.path.child(&entry.name))
}

/// Where the leftmost column starts for `path` on `conn`.
fn natural_root(model: &Model, conn: ConnId, path: &VPath) -> VPath {
    if let Some((_, file)) = model.conns.get(conn).and_then(|c| c.host.clone()) {
        return file;
    }
    match model.conns.get(conn).map(|c| &c.home) {
        Some(home) if path.starts_with(home) => home.clone(),
        _ => VPath::root(),
    }
}

/// Runs after every update: lists the columns the panes in the column view
/// now show and do not have, and drops those they no longer show.
pub fn follow(model: &mut Model) -> Cmd {
    let mut cmds = Vec::new();
    for side in Side::BOTH {
        if model.settings.session.view(side) != ViewMode::Columns {
            continue;
        }
        let pane = model.pane(side);
        let (conn, path) = (pane.loc().conn, pane.loc().path.clone());
        let root = match &pane.tab().columns.root {
            Some(root) if path.starts_with(root) => None,
            _ => Some(natural_root(model, conn, &path)),
        };
        let tab = model.pane_mut(side).tab_mut();
        if let Some(root) = root {
            tab.columns.root = Some(root);
            tab.columns.synced = None;
        }
        let Some(root) = tab.columns.root.clone() else { continue };
        let ancestors = ancestors(&root, &path);
        let preview = preview_path(tab);
        let (id, elevated) = (tab.id, tab.elevated);
        let columns = &mut tab.columns;
        columns
            .cache
            .retain(|p, _| ancestors.contains(p) || preview.as_ref() == Some(p));
        for p in &ancestors {
            if !columns.cache.contains_key(p) {
                columns.cache.insert(p.clone(), Column::loading());
                columns.synced = None;
                cmds.push(list(id, conn, p.clone(), elevated));
            }
        }
        match preview {
            Some(p) if !columns.cache.contains_key(&p) => {
                if columns.waiting.as_ref() != Some(&p) {
                    columns.waiting = Some(p);
                    columns.generation += 1;
                    let generation = columns.generation;
                    cmds.push(
                        Command::request_from_shell(SleepOp(SETTLE_MS))
                            .then_send(move |()| shell(Reply::Columns(ColumnsReply::Settled { tab: id, generation }))),
                    );
                }
            }
            _ => columns.waiting = None,
        }
        if columns.synced.as_ref() != Some(&path) {
            for p in &ancestors {
                let next = path.relative_to(p).and_then(|rel| rel.split(|b| *b == b'/').next());
                if let (Some(column), Some(next)) = (columns.cache.get_mut(p), next) {
                    column.selected = column.row_of(next);
                }
            }
            columns.synced = Some(path);
        }
    }
    Command::all(cmds)
}

fn list(tab: TabId, conn: ConnId, path: VPath, elevated: bool) -> Cmd {
    let op = FsOp::List {
        conn,
        path: path.clone(),
        elevated,
    };
    Command::request_from_shell(op)
        .then_send(move |out| shell(Reply::Columns(ColumnsReply::Listed { tab, path, out: Box::new(out) })))
}

pub fn reply(model: &mut Model, reply: ColumnsReply, out: &mut Out) {
    match reply {
        ColumnsReply::Listed { tab, path, out } => {
            let Some((side, ix)) = pane::find_tab(model, tab) else { return };
            let display = pane::display(model, side);
            let columns = &mut model.pane_mut(side).tabs[ix].columns;
            // Not if the column went away meanwhile, or was filled from a
            // listing the pane made itself.
            let Some(column) = columns.cache.get_mut(&path).filter(|c| c.loading) else { return };
            let FsOutput::Listing { entries, .. } = *out else { return };
            *column = match entries {
                Ok(entries) => Column::of(Listing::new(entries), display),
                Err(VfsError::Cancelled) => Column {
                    revision: revision(),
                    ..Column::default()
                },
                Err(err) => Column {
                    error: Some(err.to_string()),
                    revision: revision(),
                    ..Column::default()
                },
            };
            columns.synced = None;
        }
        ColumnsReply::Settled { tab, generation } => {
            let Some((side, ix)) = pane::find_tab(model, tab) else { return };
            let tab = &mut model.pane_mut(side).tabs[ix];
            if tab.columns.generation != generation {
                return;
            }
            let Some(path) = tab.columns.waiting.take() else { return };
            if preview_path(tab).as_ref() != Some(&path) || tab.columns.cache.contains_key(&path) {
                return;
            }
            tab.columns.cache.insert(path.clone(), Column::loading());
            let (id, conn, elevated) = (tab.id, tab.loc.conn, tab.elevated);
            out.push(list(id, conn, path, elevated));
        }
    }
}

/// A press on a row of a column other than the pane's own: the pane moves
/// to that column's folder, with the cursor on the row.
pub fn click(model: &mut Model, side: Side, column: usize, row: usize, out: &mut Out) {
    model.active = side;
    let tab = model.pane(side).tab();
    let Some(root) = tab.columns.root.clone() else { return };
    let mut shown = ancestors(&root, &tab.loc.path);
    shown.extend(preview_path(tab));
    let Some(path) = shown.get(column).cloned() else { return };
    let Some(name) = tab.columns.cache.get(&path).and_then(|c| c.name_at(row)).map(<[u8]>::to_vec) else {
        return;
    };
    pane::navigate(model, side, path, Some(name), out);
}
