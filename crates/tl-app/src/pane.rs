//! One file pane: tabs, a listing with a cursor and marks, sorting, the
//! type-ahead filter, history, and the row of saved folders.
//!
//! A listing is kept once per tab as it came from the file system
//! ([`Listing`]); what the list shows is an order of indices into it, so
//! sorting, filtering and marking never copy entries, and the view hands
//! the shell the same `Arc`s until something changes.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crux_core::Command;
use tl_model::archive::ArchiveFormat;
use tl_model::util::wildcard_match;
use tl_model::{Entry, Place, SpaceInfo, VPath, VfsError};

use crate::capabilities::*;
use crate::dialog::{self, InputPurpose};
use crate::ids::{ConnId, Ids, LOCAL, TabId};
use crate::model::{Command as Cmd, Model, Reply, shell};
use crate::settings::SortKey;
use crate::{Out, ops};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Side {
    #[default]
    Left,
    Right,
}

impl Side {
    pub fn other(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }

    pub fn index(self) -> usize {
        match self {
            Side::Left => 0,
            Side::Right => 1,
        }
    }

    pub const BOTH: [Side; 2] = [Side::Left, Side::Right];
}

/// A folder on a file system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loc {
    pub conn: ConnId,
    pub path: VPath,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub desc: bool,
}

/// A folder's entries as the file system listed them, and their names as
/// text (decoded once, lossily).
#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub entries: Arc<Vec<Entry>>,
    pub names: Arc<Vec<String>>,
}

impl Listing {
    pub fn new(entries: Vec<Entry>) -> Self {
        let names = entries.iter().map(Entry::name_lossy).collect();
        Listing {
            entries: Arc::new(entries),
            names: Arc::new(names),
        }
    }
}

/// The ".." row, in a list order.
pub const PARENT: u32 = u32::MAX;

/// Totals of a set of rows, for the status line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub dirs: u64,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug)]
pub struct Tab {
    pub id: TabId,
    pub loc: Loc,
    pub elevated: bool,
    pub listing: Listing,
    /// What the list shows, in order: indices into `listing`, [`PARENT`]
    /// for "..".
    pub order: Arc<Vec<u32>>,
    pub cursor: usize,
    /// Where a shift-click range starts.
    pub anchor: usize,
    pub marked: Arc<HashSet<Vec<u8>>>,
    /// Only names matching this are listed; empty lists everything.
    pub filter: String,
    /// The entry a caller asked to put the cursor on, until a listing has
    /// managed to: a folder that changes while it is listed is listed again,
    /// and the request has to outlive the listing it was made with.
    pending_select: Option<Vec<u8>>,
    pub sort: Sort,
    pub loading: bool,
    /// Which listing is the current one; older answers are dropped.
    generation: u64,
    pub error: Option<String>,
    pub back: Vec<(Loc, bool)>,
    pub forward: Vec<(Loc, bool)>,
    pub space: Option<SpaceInfo>,
    pub dir_sizes: Arc<HashMap<Vec<u8>, u64>>,
    /// The local folder is being watched for changes.
    watching: bool,
    /// Totals of the listed rows, and of the marked ones.
    pub totals: Totals,
    pub marked_totals: Totals,
    /// How many rows the listing has before the filter (hidden files count
    /// only when shown).
    pub unfiltered: usize,
}

impl Tab {
    pub fn new(ids: &mut Ids, loc: Loc, elevated: bool, sort: Sort) -> Tab {
        Tab {
            id: ids.tab(),
            loc,
            elevated,
            listing: Listing::default(),
            order: Arc::new(Vec::new()),
            cursor: 0,
            anchor: 0,
            marked: Arc::new(HashSet::new()),
            filter: String::new(),
            pending_select: None,
            sort,
            loading: false,
            generation: 0,
            error: None,
            back: Vec::new(),
            forward: Vec::new(),
            space: None,
            dir_sizes: Arc::new(HashMap::new()),
            watching: false,
            totals: Totals::default(),
            marked_totals: Totals::default(),
            unfiltered: 0,
        }
    }

    /// The entry behind a row, `None` for "..".
    pub fn entry(&self, row: usize) -> Option<&Entry> {
        match self.order.get(row) {
            Some(&PARENT) | None => None,
            Some(&ix) => self.listing.entries.get(ix as usize),
        }
    }

    pub fn is_parent(&self, row: usize) -> bool {
        self.order.get(row) == Some(&PARENT)
    }

    pub fn name(&self, row: usize) -> Option<&str> {
        match self.order.get(row) {
            Some(&PARENT) => Some(".."),
            Some(&ix) => self.listing.names.get(ix as usize).map(String::as_str),
            None => None,
        }
    }

    pub fn cursor_entry(&self) -> Option<&Entry> {
        self.entry(self.cursor)
    }

    /// The listed entries, as shown (filtered, hidden files only when
    /// visible), in display order.
    pub fn listed(&self) -> impl Iterator<Item = &Entry> {
        self.order
            .iter()
            .filter(|ix| **ix != PARENT)
            .map(|ix| &self.listing.entries[*ix as usize])
    }

    /// Marked entries in display order (only the visible ones).
    pub fn marked_entries(&self) -> Vec<Entry> {
        self.listed().filter(|e| self.marked.contains(&e.name)).cloned().collect()
    }

    fn reset_view(&mut self) {
        self.pending_select = None;
        self.marked = Arc::new(HashSet::new());
        self.dir_sizes = Arc::new(HashMap::new());
        self.error = None;
        self.cursor = 0;
        self.anchor = 0;
        self.order = Arc::new(Vec::new());
        self.listing = Listing::default();
        self.space = None;
        self.totals = Totals::default();
        self.marked_totals = Totals::default();
        self.unfiltered = 0;
    }

    fn update_marked_totals(&mut self) {
        let mut t = Totals::default();
        if !self.marked.is_empty() {
            for entry in self.listed().filter(|e| self.marked.contains(&e.name)) {
                if entry.is_dir() {
                    t.dirs += 1;
                    t.bytes += self.dir_sizes.get(&entry.name).copied().unwrap_or(0);
                } else {
                    t.files += 1;
                    t.bytes += entry.size;
                }
            }
        }
        self.marked_totals = t;
    }
}

#[derive(Clone, Debug)]
pub struct Pane {
    pub tabs: Vec<Tab>,
    pub active: usize,
}

impl Default for Pane {
    fn default() -> Self {
        let sort = Sort {
            key: SortKey::Name,
            desc: false,
        };
        Pane {
            tabs: vec![Tab::new(
                &mut Ids::default(),
                Loc {
                    conn: LOCAL,
                    path: VPath::root(),
                },
                false,
                sort,
            )],
            active: 0,
        }
    }
}

impl Pane {
    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    pub fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    pub fn loc(&self) -> &Loc {
        &self.tab().loc
    }

    pub fn elevated(&self) -> bool {
        self.tab().elevated
    }

    pub fn is_loading(&self) -> bool {
        self.tab().loading
    }

    /// The items an operation acts on: the marked rows, or the cursor row.
    pub fn selection(&self) -> Option<Selection> {
        let tab = self.tab();
        let entries = if tab.marked.is_empty() {
            vec![tab.cursor_entry()?.clone()]
        } else {
            tab.marked_entries()
        };
        if entries.is_empty() {
            return None;
        }
        Some(Selection {
            conn: tab.loc.conn,
            dir: tab.loc.path.clone(),
            entries,
            elevated: tab.elevated,
        })
    }
}

/// The items an operation acts on.
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub conn: ConnId,
    pub dir: VPath,
    pub entries: Vec<Entry>,
    pub elevated: bool,
}

impl Selection {
    pub fn names(&self) -> Vec<Vec<u8>> {
        self.entries.iter().map(|e| e.name.clone()).collect()
    }

    pub fn paths(&self) -> Vec<VPath> {
        self.entries.iter().map(|e| self.dir.child(&e.name)).collect()
    }

    /// "“notes.txt”" or "3 items".
    pub fn describe(&self) -> String {
        describe_names(&self.names())
    }
}

/// "“notes.txt”" or "3 items".
pub fn describe_names(names: &[Vec<u8>]) -> String {
    match names {
        [one] => format!("“{}”", String::from_utf8_lossy(one)),
        many => format!("{} items", many.len()),
    }
}

/// What a drag from a pane carries.
#[derive(Clone, Debug, PartialEq)]
pub struct Dragged {
    pub side: Side,
    pub conn: ConnId,
    pub dir: VPath,
    pub names: Vec<Vec<u8>>,
    pub elevated: bool,
}

/// Mouse and pane-local input.
#[derive(Clone, Debug, PartialEq)]
pub enum PaneInput {
    /// A press on a row: ⌘ toggles its mark, ⇧ marks the range from the
    /// anchor, neither moves the cursor.
    Click { row: usize, toggle: bool, range: bool },
    DoubleClick { row: usize },
    /// A right press: an unmarked row gets the cursor before the menu opens.
    ContextClick { row: usize },
    SortBy(SortKey),
    SelectTab(usize),
    CloseTab(usize),
    /// A breadcrumb or a place chip.
    Navigate(VPath),
    /// "This Mac" entries of the locations menu.
    OpenLocal(VPath),
    OpenBookmark(String),
    RenamePlace(String),
    MovePlace { id: String, delta: isize },
    RemovePlace(String),
    Retry,
    Reconnect,
    Disconnect,
    /// The locations menu opened: refresh what it lists.
    LocationsShown,
    /// Files from the other pane dropped here.
    Drop { drag: Dragged, mode: tl_model::transfer::TransferMode },
}

#[derive(Debug)]
pub enum PaneReply {
    Listed { tab: TabId, generation: u64, out: FsOutput },
    ArchiveOpened { tab: TabId, from: Loc, file: VPath, name: Vec<u8>, out: FsOutput },
    Elevated { tab: TabId, out: FsOutput },
}

// ----- rows -----------------------------------------------------------------

/// Whether a name passes the filter. The whole name is matched, extension
/// included, so `log` finds both `logrotate.conf` and `app.log`. A query
/// holding `*` or `?` is a wildcard instead, matching the name end to end,
/// so `*.log` finds only the second; `;` separates several.
pub fn matches_filter(name: &str, query: &str) -> bool {
    if query.contains('*') || query.contains('?') {
        return wildcard_match(query, name);
    }
    name.to_lowercase().contains(&query.to_lowercase())
}

/// Case-insensitive natural order: `file2` before `file10`. Case only
/// breaks ties between otherwise equal names.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    natural_cmp_ignore_case(a, b).then_with(|| a.cmp(b))
}

fn natural_cmp_ignore_case(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    // ASCII fast path; anything else compares by lower-cased chars.
    while i < a.len() && j < b.len() {
        let (x, y) = (a[i], b[j]);
        if x.is_ascii_digit() && y.is_ascii_digit() {
            let si = i;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let na = trim_zeros(&a[si..i]);
            let nb = trim_zeros(&b[sj..j]);
            let ord = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
            if ord != Ordering::Equal {
                return ord;
            }
        } else if x.is_ascii() && y.is_ascii() {
            let ord = x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase());
            if ord != Ordering::Equal {
                return ord;
            }
            i += 1;
            j += 1;
        } else {
            let ca = std::str::from_utf8(&a[i..]).ok().and_then(|s| s.chars().next());
            let cb = std::str::from_utf8(&b[j..]).ok().and_then(|s| s.chars().next());
            let (Some(ca), Some(cb)) = (ca, cb) else {
                return a[i..].cmp(&b[j..]);
            };
            let ord = ca.to_lowercase().cmp(cb.to_lowercase());
            if ord != Ordering::Equal {
                return ord;
            }
            i += ca.len_utf8();
            j += cb.len_utf8();
        }
    }
    if i < a.len() {
        Ordering::Greater
    } else if j < b.len() {
        Ordering::Less
    } else {
        Ordering::Equal
    }
}

fn trim_zeros(digits: &[u8]) -> &[u8] {
    let start = digits.iter().position(|d| *d != b'0').unwrap_or(digits.len());
    &digits[start..]
}

/// The order a listing is shown in, and how many rows it has before the
/// filter.
pub fn build_order(
    listing: &Listing,
    sort: Sort,
    folders_first: bool,
    show_hidden: bool,
    at_root: bool,
    filter: &str,
) -> (Vec<u32>, usize) {
    let query = filter.trim();
    let entries = &listing.entries;
    let names = &listing.names;
    let visible: Vec<u32> = (0..entries.len() as u32)
        .filter(|ix| show_hidden || !entries[*ix as usize].is_hidden())
        .collect();
    let unfiltered = visible.len();
    let mut order: Vec<u32> = if query.is_empty() {
        visible
    } else {
        visible.into_iter().filter(|ix| matches_filter(&names[*ix as usize], query)).collect()
    };
    order.sort_by(|&a, &b| {
        let (ea, eb) = (&entries[a as usize], &entries[b as usize]);
        let (ad, bd) = (ea.is_dir(), eb.is_dir());
        if folders_first && ad != bd {
            return bd.cmp(&ad);
        }
        let by_name = || natural_cmp(&names[a as usize], &names[b as usize]).then_with(|| ea.name.cmp(&eb.name));
        let ord = match sort.key {
            SortKey::Name => by_name(),
            SortKey::Ext => ea
                .extension()
                .map(|e| e.to_ascii_lowercase())
                .cmp(&eb.extension().map(|e| e.to_ascii_lowercase()))
                .then_with(by_name),
            SortKey::Size if ad => by_name(),
            SortKey::Size => ea.size.cmp(&eb.size).then_with(by_name),
            SortKey::Modified => ea.mtime.cmp(&eb.mtime).then_with(by_name),
        };
        if sort.desc { ord.reverse() } else { ord }
    });
    if !at_root {
        order.insert(0, PARENT);
    }
    (order, unfiltered)
}

/// Rebuilds what tab `tab` shows, keeping the cursor on `select`, or else
/// on the entry it was on.
fn rebuild(tab: &mut Tab, select: Option<&[u8]>, show_hidden: bool, folders_first: bool) {
    let keep: Option<Vec<u8>> = select
        .map(<[u8]>::to_vec)
        .or_else(|| tab.cursor_entry().map(|e| e.name.clone()));
    let (order, unfiltered) = build_order(
        &tab.listing,
        tab.sort,
        folders_first,
        show_hidden,
        tab.loc.path.is_root(),
        &tab.filter,
    );
    tab.order = Arc::new(order);
    tab.unfiltered = unfiltered;
    // Against the whole listing rather than the visible rows: a mark has to
    // survive the filter being narrowed and widened again, and only a file
    // that has actually gone should lose it.
    if !tab.marked.is_empty() {
        let names: HashSet<&[u8]> = tab.listing.entries.iter().map(|e| e.name.as_slice()).collect();
        if tab.marked.iter().any(|n| !names.contains(n.as_slice())) {
            Arc::make_mut(&mut tab.marked).retain(|n| names.contains(n.as_slice()));
        }
    }
    let found = keep.and_then(|name| (0..tab.order.len()).find(|row| tab.entry(*row).is_some_and(|e| e.name == name)));
    tab.cursor = found.unwrap_or_else(|| tab.cursor.min(tab.order.len().saturating_sub(1)));
    let mut totals = Totals::default();
    for entry in tab.listed() {
        if entry.is_dir() {
            totals.dirs += 1;
        } else {
            totals.files += 1;
            totals.bytes += entry.size;
        }
    }
    tab.totals = totals;
    tab.update_marked_totals();
}

fn display_flags(model: &Model) -> (bool, bool) {
    (model.settings.general.show_hidden, model.settings.general.folders_first)
}

/// A setting the rows are built from changed: rebuild every tab.
pub fn settings_changed(model: &mut Model) {
    let (show_hidden, folders_first) = display_flags(model);
    for pane in &mut model.panes {
        for tab in &mut pane.tabs {
            rebuild(tab, None, show_hidden, folders_first);
        }
    }
}

/// Where a tab is, by id.
pub fn find_tab(model: &Model, id: TabId) -> Option<(Side, usize)> {
    Side::BOTH.into_iter().find_map(|side| {
        model.pane(side).tabs.iter().position(|t| t.id == id).map(|ix| (side, ix))
    })
}

// ----- loading ---------------------------------------------------------------

/// (Re)lists tab `ix` of `side`, putting the cursor on `select` if given.
pub fn load(model: &mut Model, side: Side, ix: usize, select: Option<Vec<u8>>, out: &mut Out) {
    let tab = &mut model.pane_mut(side).tabs[ix];
    if select.is_some() {
        tab.pending_select = select;
    }
    tab.loading = true;
    tab.generation += 1;
    let (id, generation) = (tab.id, tab.generation);
    let op = FsOp::List {
        conn: tab.loc.conn,
        path: tab.loc.path.clone(),
        elevated: tab.elevated,
    };
    out.fs(op, move |out| Reply::Pane(PaneReply::Listed { tab: id, generation, out }));
}

pub fn reload_active(model: &mut Model, side: Side, out: &mut Out) {
    let ix = model.pane(side).active;
    load(model, side, ix, None, out);
}

/// Reloads every tab showing `dir` on a file system with `key`.
pub fn refresh_dir(model: &mut Model, key: &str, dir: &VPath, out: &mut Out) {
    for side in Side::BOTH {
        for ix in 0..model.pane(side).tabs.len() {
            let loc = &model.pane(side).tabs[ix].loc;
            if &loc.path == dir && model.conns.location_key(loc.conn) == key {
                load(model, side, ix, None, out);
            }
        }
    }
}

/// Reloads every tab showing `dir` on `key` with the cursor on `name`,
/// marks cleared (after a create, rename or copy).
pub fn select_in_panes(model: &mut Model, key: &str, dir: &VPath, name: Vec<u8>, out: &mut Out) {
    for side in Side::BOTH {
        let ix = model.pane(side).active;
        let loc = model.pane(side).tabs[ix].loc.clone();
        if &loc.path == dir && model.conns.location_key(loc.conn) == key {
            model.pane_mut(side).tabs[ix].marked = Arc::new(HashSet::new());
            load(model, side, ix, Some(name.clone()), out);
        }
    }
}

fn listed(model: &mut Model, id: TabId, generation: u64, result: FsOutput, out: &mut Out) {
    let (show_hidden, folders_first) = display_flags(model);
    let Some((side, ix)) = find_tab(model, id) else { return };
    let can_elevate;
    {
        let conn = model.pane(side).tabs[ix].loc.conn;
        can_elevate = model.conns.caps(conn).elevation;
    }
    let tab = &mut model.pane_mut(side).tabs[ix];
    if tab.generation != generation {
        return;
    }
    tab.loading = false;
    let FsOutput::Listing { entries, space } = result else { return };
    tab.space = space;
    match entries {
        Ok(entries) => {
            tab.listing = Listing::new(entries);
            tab.error = None;
            // This listing supersedes any earlier one, so it owes the cursor
            // to whoever asked last.
            let select = tab.pending_select.take();
            rebuild(tab, select.as_deref(), show_hidden, folders_first);
            watch(model, side, ix, out);
        }
        Err(err) => {
            tab.error = Some(err.to_string());
            tab.listing = Listing::default();
            tab.order = Arc::new(if tab.loc.path.is_root() { vec![] } else { vec![PARENT] });
            tab.cursor = 0;
            tab.totals = Totals::default();
            tab.marked_totals = Totals::default();
            let (elevated, path) = (tab.elevated, tab.loc.path.clone());
            match err {
                VfsError::PermissionDenied(_) if can_elevate && !elevated => {
                    ops::offer_sudo_listing(model, id, path, out);
                }
                VfsError::Cancelled => {}
                other => out.error(other.to_string()),
            }
        }
    }
}

/// Watches a local tab's folder so the listing follows outside changes.
fn watch(model: &mut Model, side: Side, ix: usize, out: &mut Out) {
    let is_local = model.conns.is_local(model.pane(side).tabs[ix].loc.conn);
    let tab = &mut model.pane_mut(side).tabs[ix];
    if !is_local || tab.watching {
        return;
    }
    tab.watching = true;
    let id = tab.id;
    let op = WatchOp::Start {
        tab: id,
        path: tab.loc.path.clone(),
    };
    out.push(Command::stream_from_shell(op).then_send(move |change| shell(Reply::Watch { tab: id, out: change })));
}

fn unwatch(tab: &mut Tab, out: &mut Out) {
    if tab.watching {
        tab.watching = false;
        out.push(Command::notify_shell(WatchOp::Stop { tab: tab.id }).into());
    }
}

pub fn watched(model: &mut Model, id: TabId, change: WatchOutput, out: &mut Out) {
    if change != WatchOutput::Changed {
        return;
    }
    let Some((side, ix)) = find_tab(model, id) else { return };
    let tab = &model.pane(side).tabs[ix];
    if tab.watching && !tab.loading {
        load(model, side, ix, None, out);
    }
}

// ----- navigation --------------------------------------------------------------

/// Moves the active tab of `side` to `path` on its file system; leaving an
/// archive's root goes back to the folder holding it.
pub fn navigate(model: &mut Model, side: Side, path: VPath, select: Option<Vec<u8>>, out: &mut Out) {
    let conn = model.pane(side).loc().conn;
    if let Some((host, file)) = model.conns.get(conn).and_then(|c| c.host.clone())
        && !path.starts_with(&file)
    {
        enter(model, side, Loc { conn: host, path }, select, out);
        return;
    }
    let tab = model.pane_mut(side).tab_mut();
    tab.filter.clear();
    if tab.loc.path != path {
        let old = tab.loc.clone();
        tab.back.push((old, tab.elevated));
        tab.forward.clear();
        tab.loc.path = path;
        tab.reset_view();
        unwatch(tab, out);
    }
    reload_active_select(model, side, select, out);
    ops::location_changed(model, out);
}

fn reload_active_select(model: &mut Model, side: Side, select: Option<Vec<u8>>, out: &mut Out) {
    let ix = model.pane(side).active;
    load(model, side, ix, select, out);
}

/// Moves the active tab into or out of an archive (or onto another file
/// system), keeping its history and sudo state.
pub fn enter(model: &mut Model, side: Side, loc: Loc, select: Option<Vec<u8>>, out: &mut Out) {
    let tab = model.pane_mut(side).tab_mut();
    tab.filter.clear();
    let old = std::mem::replace(&mut tab.loc, loc);
    tab.back.push((old, tab.elevated));
    tab.forward.clear();
    tab.reset_view();
    unwatch(tab, out);
    reload_active_select(model, side, select, out);
    ops::location_changed(model, out);
}

/// Switches the active tab of `side` to another file system (a fresh tab
/// with the same sort), or navigates when it is the same one.
pub fn open_location(model: &mut Model, side: Side, loc: Loc, elevated: bool, out: &mut Out) {
    if model.pane(side).loc().conn == loc.conn {
        if model.pane(side).elevated() != elevated {
            model.pane_mut(side).tab_mut().elevated = elevated;
        }
        navigate(model, side, loc.path, None, out);
        return;
    }
    let sort = model.pane(side).tab().sort;
    let fresh = Tab::new(&mut model.ids, loc, elevated, sort);
    let pane = model.pane_mut(side);
    let ix = pane.active;
    unwatch(&mut pane.tabs[ix], out);
    pane.tabs[ix] = fresh;
    load(model, side, ix, None, out);
    ops::location_changed(model, out);
}

pub fn set_elevated(model: &mut Model, side: Side, elevated: bool, out: &mut Out) {
    model.pane_mut(side).tab_mut().elevated = elevated;
    reload_active(model, side, out);
    ops::location_changed(model, out);
}

pub fn set_dir_sizes(model: &mut Model, side: Side, tab: TabId, sizes: Vec<(Vec<u8>, u64)>) {
    let Some(ix) = model.pane(side).tabs.iter().position(|t| t.id == tab) else { return };
    let tab = &mut model.pane_mut(side).tabs[ix];
    let map = Arc::make_mut(&mut tab.dir_sizes);
    map.extend(sizes);
    tab.update_marked_totals();
}

/// Marks exactly `names` in the active tab, cursor on the first of them.
pub fn set_marks(model: &mut Model, side: Side, names: Vec<Vec<u8>>) {
    let tab = model.pane_mut(side).tab_mut();
    tab.marked = Arc::new(names.into_iter().collect());
    if let Some(row) = (0..tab.order.len()).find(|r| tab.entry(*r).is_some_and(|e| tab.marked.contains(&e.name))) {
        tab.cursor = row;
        tab.anchor = row;
    }
    tab.update_marked_totals();
}

/// Every tab on `conn` (or inside an archive on it) goes back to the local
/// home folder.
pub fn drop_connection(model: &mut Model, conn: ConnId, out: &mut Out) {
    let home = model.conns.local().map(|c| c.home.clone()).unwrap_or_else(VPath::root);
    for side in Side::BOTH {
        for ix in 0..model.pane(side).tabs.len() {
            let tab_conn = model.pane(side).tabs[ix].loc.conn;
            if model.conns.host_of(tab_conn) == conn || tab_conn == conn {
                let sort = model.pane(side).tabs[ix].sort;
                let fresh = Tab::new(
                    &mut model.ids,
                    Loc {
                        conn: LOCAL,
                        path: home.clone(),
                    },
                    false,
                    sort,
                );
                unwatch(&mut model.pane_mut(side).tabs[ix], out);
                model.pane_mut(side).tabs[ix] = fresh;
                load(model, side, ix, None, out);
            }
        }
    }
    ops::location_changed(model, out);
}

fn go_up(model: &mut Model, side: Side, out: &mut Out) {
    let path = model.pane(side).loc().path.clone();
    if let Some(parent) = path.parent() {
        let from = path.file_name().map(<[u8]>::to_vec);
        navigate(model, side, parent, from, out);
    }
}

fn go_back(model: &mut Model, side: Side, out: &mut Out) {
    let tab = model.pane_mut(side).tab_mut();
    let Some((prev, elevated)) = tab.back.pop() else { return };
    let cur = std::mem::replace(&mut tab.loc, prev);
    let cur_elevated = std::mem::replace(&mut tab.elevated, elevated);
    // Land on the folder (or archive) we came out of.
    let select = cur
        .path
        .relative_to(&tab.loc.path)
        .and_then(|rel| rel.split(|b| *b == b'/').next())
        .filter(|name| !name.is_empty())
        .map(<[u8]>::to_vec);
    tab.forward.push((cur, cur_elevated));
    tab.filter.clear();
    tab.reset_view();
    unwatch(tab, out);
    reload_active_select(model, side, select, out);
    ops::location_changed(model, out);
}

fn go_forward(model: &mut Model, side: Side, out: &mut Out) {
    let tab = model.pane_mut(side).tab_mut();
    let Some((next, elevated)) = tab.forward.pop() else { return };
    let cur = std::mem::replace(&mut tab.loc, next);
    let cur_elevated = std::mem::replace(&mut tab.elevated, elevated);
    tab.back.push((cur, cur_elevated));
    tab.filter.clear();
    tab.reset_view();
    unwatch(tab, out);
    reload_active_select(model, side, None, out);
    ops::location_changed(model, out);
}

/// A file the pane can open as a folder.
pub fn is_archive(entry: &Entry) -> bool {
    !entry.is_dir() && ArchiveFormat::detect(&entry.name).is_some()
}

fn open_row(model: &mut Model, side: Side, out: &mut Out) {
    let tab = model.pane(side).tab();
    if tab.is_parent(tab.cursor) {
        go_up(model, side, out);
        return;
    }
    let Some(entry) = tab.cursor_entry().cloned() else { return };
    let in_archive = model.conns.get(tab.loc.conn).is_some_and(|c| c.is_archive());
    if entry.is_dir() {
        let path = tab.loc.path.child(&entry.name);
        navigate(model, side, path, None, out);
    } else if is_archive(&entry) && !in_archive {
        open_archive(model, side, entry.name, out);
    } else {
        ops::open_file(model, side, entry, out);
    }
}

/// Opens the archive `name` in the current folder as a folder.
pub fn open_archive(model: &mut Model, side: Side, name: Vec<u8>, out: &mut Out) {
    let new = model.ids.conn();
    let tab = model.pane_mut(side).tab_mut();
    tab.loading = true;
    let from = tab.loc.clone();
    let file = from.path.child(&name);
    let id = tab.id;
    let op = FsOp::OpenArchive {
        conn: from.conn,
        file: file.clone(),
        elevated: tab.elevated,
        new,
    };
    out.fs(op, move |out| Reply::Pane(PaneReply::ArchiveOpened { tab: id, from, file, name, out }));
}

fn archive_opened(model: &mut Model, id: TabId, from: Loc, file: VPath, name: Vec<u8>, result: FsOutput, out: &mut Out) {
    let Some((side, ix)) = find_tab(model, id) else { return };
    model.pane_mut(side).tabs[ix].loading = false;
    let FsOutput::Opened(result) = result else { return };
    match result {
        Ok(info) => {
            let conn = info.id;
            model.conns.insert(info);
            // Not if the pane moved on meanwhile.
            let pane = model.pane(side);
            if pane.active != ix || pane.tabs[ix].loc != from {
                ops::release_unused(model, out);
                return;
            }
            enter(model, side, Loc { conn, path: file }, None, out);
        }
        Err(err) => out.error(format!("Cannot open {}: {err}", String::from_utf8_lossy(&name))),
    }
}

// ----- tabs --------------------------------------------------------------------

fn new_tab(model: &mut Model, side: Side, out: &mut Out) {
    let tab = model.pane(side).tab();
    let (loc, elevated, sort) = (tab.loc.clone(), tab.elevated, tab.sort);
    let fresh = Tab::new(&mut model.ids, loc, elevated, sort);
    let pane = model.pane_mut(side);
    pane.tabs.insert(pane.active + 1, fresh);
    pane.active += 1;
    reload_active(model, side, out);
    ops::location_changed(model, out);
}

fn close_tab(model: &mut Model, side: Side, ix: usize, out: &mut Out) {
    let pane = model.pane_mut(side);
    if pane.tabs.len() <= 1 || ix >= pane.tabs.len() {
        return;
    }
    let mut gone = pane.tabs.remove(ix);
    unwatch(&mut gone, out);
    if pane.active >= pane.tabs.len() || pane.active > ix {
        pane.active = pane.active.saturating_sub(1);
    }
    ops::release_unused(model, out);
    ops::location_changed(model, out);
}

fn select_tab(model: &mut Model, side: Side, ix: usize, out: &mut Out) {
    if ix < model.pane(side).tabs.len() {
        model.pane_mut(side).active = ix;
        ops::location_changed(model, out);
    }
}

// ----- cursor, marks and filter -------------------------------------------------------

fn move_cursor(tab: &mut Tab, to: usize) {
    if tab.order.is_empty() {
        return;
    }
    tab.cursor = to.min(tab.order.len() - 1);
    tab.anchor = tab.cursor;
}

fn toggle_mark_at(tab: &mut Tab, row: usize) {
    let Some(name) = tab.entry(row).map(|e| e.name.clone()) else { return };
    let marked = Arc::make_mut(&mut tab.marked);
    if !marked.remove(&name) {
        marked.insert(name);
    }
}

/// Narrows or widens the list, keeping the cursor on its file for as long as
/// that file still matches.
fn set_filter(model: &mut Model, side: Side, query: String) {
    let (show_hidden, folders_first) = display_flags(model);
    let tab = model.pane_mut(side).tab_mut();
    let keep = tab.cursor_entry().map(|e| e.name.clone());
    tab.filter = query;
    rebuild(tab, keep.as_deref(), show_hidden, folders_first);
    let kept = keep.is_some_and(|name| tab.cursor_entry().is_some_and(|e| e.name == name));
    if !kept {
        // The file under the cursor no longer matches, and the old index now
        // points at whatever happens to sit there.
        tab.cursor = (0..tab.order.len()).find(|r| !tab.is_parent(*r)).unwrap_or(0);
        tab.anchor = tab.cursor;
    }
}

fn ask_mask(model: &mut Model, side: Side, mark: bool) {
    let (title, ok) = if mark { ("Mark Files", "Mark") } else { ("Unmark Files", "Unmark") };
    dialog::ask_input(
        model,
        title,
        "Wildcards like *.log or img_??.png; separate several with ;",
        "*",
        None,
        ok,
        InputPurpose::Mask { side, mark },
    );
}

pub fn apply_mask(model: &mut Model, side: Side, mark: bool, mask: &str) {
    let tab = model.pane_mut(side).tab_mut();
    let hits: Vec<Vec<u8>> = (0..tab.order.len())
        .filter(|r| !tab.is_parent(*r) && tab.name(*r).is_some_and(|n| wildcard_match(mask, n)))
        .filter_map(|r| tab.entry(r).map(|e| e.name.clone()))
        .collect();
    let marked = Arc::make_mut(&mut tab.marked);
    for name in hits {
        if mark {
            marked.insert(name);
        } else {
            marked.remove(&name);
        }
    }
    tab.update_marked_totals();
}

// ----- places ------------------------------------------------------------------------

/// Which connection the row of saved folders belongs to: inside an archive,
/// the file system holding it.
pub fn host_key(model: &Model, side: Side) -> String {
    let host = model.conns.host_of(model.pane(side).loc().conn);
    model.conns.location_key(host)
}

/// The folder on that host, or `None` inside an archive.
fn host_path(model: &Model, side: Side) -> Option<VPath> {
    let loc = model.pane(side).loc();
    (!model.conns.get(loc.conn).is_some_and(|c| c.is_archive())).then(|| loc.path.clone())
}

fn toggle_place(model: &mut Model, side: Side, out: &mut Out) {
    let Some(path) = host_path(model, side) else {
        out.error("A folder inside an archive cannot be saved.");
        return;
    };
    let host = host_key(model, side);
    let saved = model.bookmarks.place_of(&host, &path).map(|p| p.id.clone());
    match saved {
        Some(id) => {
            Arc::make_mut(&mut model.bookmarks).remove_place(&id);
        }
        None => {
            let name = if path.is_root() { "/".to_string() } else { path.file_name_lossy() };
            let id = model.ids.uuid();
            Arc::make_mut(&mut model.bookmarks).add_place(Place::with_id(id, host, name, path));
        }
    }
    model.save_bookmarks(out, "places");
}

fn go_to_place(model: &mut Model, side: Side, index: usize, out: &mut Out) {
    let host = host_key(model, side);
    if let Some(path) = model.bookmarks.place_at(&host, index).map(|p| p.path.clone()) {
        navigate(model, side, path, None, out);
    }
}

pub fn rename_place(model: &mut Model, id: &str, name: String, out: &mut Out) {
    let name = name.trim().to_string();
    if name.is_empty() {
        return;
    }
    if Arc::make_mut(&mut model.bookmarks).rename_place(id, name) {
        model.save_bookmarks(out, "places");
    }
}

// ----- dispatch ------------------------------------------------------------------------

/// Commands the active pane carries out itself.
pub fn is_pane_command(cmd: &Cmd) -> bool {
    use Cmd::*;
    matches!(
        cmd,
        CursorUp
            | CursorDown
            | PageUp { .. }
            | PageDown { .. }
            | CursorHome
            | CursorEnd
            | MarkUp
            | MarkDown
            | ToggleMark
            | MarkAll
            | UnmarkAll
            | InvertMarks
            | MarkByMask
            | UnmarkByMask
            | Open
            | OpenArchive
            | GoParent
            | GoBack
            | GoForward
            | GoHome
            | GoToPath
            | Type(_)
            | FilterBackspace
            | ClearFilter
            | Refresh
            | NewTab
            | CloseTab
            | NextTab
            | PrevTab
            | TogglePlace
            | GoToPlace(_)
    )
}

pub fn command(model: &mut Model, side: Side, cmd: Cmd, out: &mut Out) {
    let tab = model.pane_mut(side).tab_mut();
    match cmd {
        Cmd::CursorUp => {
            let to = tab.cursor.saturating_sub(1);
            move_cursor(tab, to);
        }
        Cmd::CursorDown => {
            let to = tab.cursor + 1;
            move_cursor(tab, to);
        }
        Cmd::PageUp { rows } => {
            let to = tab.cursor.saturating_sub(rows.max(1));
            move_cursor(tab, to);
        }
        Cmd::PageDown { rows } => {
            let to = tab.cursor + rows.max(1);
            move_cursor(tab, to);
        }
        Cmd::CursorHome => move_cursor(tab, 0),
        Cmd::CursorEnd => move_cursor(tab, usize::MAX),
        Cmd::MarkUp => {
            let cur = tab.cursor;
            toggle_mark_at(tab, cur);
            move_cursor(tab, cur.saturating_sub(1));
            tab.update_marked_totals();
        }
        Cmd::MarkDown | Cmd::ToggleMark => {
            let cur = tab.cursor;
            toggle_mark_at(tab, cur);
            move_cursor(tab, cur + 1);
            tab.update_marked_totals();
        }
        Cmd::MarkAll => {
            tab.marked = Arc::new(tab.listed().map(|e| e.name.clone()).collect());
            tab.update_marked_totals();
        }
        Cmd::UnmarkAll => {
            tab.marked = Arc::new(HashSet::new());
            tab.update_marked_totals();
        }
        Cmd::InvertMarks => {
            // Files only: marked folders stay marked, unmarked ones stay
            // unmarked.
            let files: HashSet<Vec<u8>> = tab.listed().filter(|e| !e.is_dir()).map(|e| e.name.clone()).collect();
            let dirs: HashSet<Vec<u8>> = tab.marked.iter().filter(|n| !files.contains(*n)).cloned().collect();
            let mut marked: HashSet<Vec<u8>> = files.difference(&tab.marked).cloned().collect();
            marked.extend(dirs);
            tab.marked = Arc::new(marked);
            tab.update_marked_totals();
        }
        Cmd::MarkByMask => ask_mask(model, side, true),
        Cmd::UnmarkByMask => ask_mask(model, side, false),
        Cmd::Open => open_row(model, side, out),
        Cmd::OpenArchive => {
            let tab = model.pane(side).tab();
            let in_archive = model.conns.get(tab.loc.conn).is_some_and(|c| c.is_archive());
            match tab.cursor_entry().cloned() {
                Some(entry) if is_archive(&entry) => {
                    if in_archive {
                        out.error("Archives inside archives cannot be opened as folders.");
                    } else {
                        open_archive(model, side, entry.name, out);
                    }
                }
                _ => open_row(model, side, out),
            }
        }
        Cmd::GoParent => go_up(model, side, out),
        Cmd::GoBack => go_back(model, side, out),
        Cmd::GoForward => go_forward(model, side, out),
        Cmd::GoHome => {
            let conn = tab.loc.conn;
            if let Some(home) = model.conns.get(conn).map(|c| c.home.clone()) {
                navigate(model, side, home, None, out);
            }
        }
        Cmd::GoToPath => {
            let loc = tab.loc.clone();
            let label = model.conns.label(loc.conn);
            dialog::ask_input(
                model,
                "Go to Folder",
                format!("Path on {label}. Relative paths and ~ work."),
                loc.path.display(),
                None,
                "Go",
                InputPurpose::GoTo { side },
            );
        }
        Cmd::Type(text) => {
            if tab.filter.is_empty() {
                // The classic mask keys, which a filter being typed turns back
                // into ordinary characters. `*` is not among them: `*.log` is
                // how anyone filters by extension.
                match text.as_str() {
                    "+" => return ask_mask(model, side, true),
                    "-" => return ask_mask(model, side, false),
                    _ => {}
                }
            }
            if text.is_empty() || text.chars().any(char::is_control) {
                return;
            }
            let query = format!("{}{text}", tab.filter);
            set_filter(model, side, query);
        }
        Cmd::FilterBackspace => {
            let mut query = tab.filter.clone();
            if query.pop().is_none() {
                go_up(model, side, out);
            } else {
                set_filter(model, side, query);
            }
        }
        Cmd::ClearFilter => {
            if !tab.filter.is_empty() {
                set_filter(model, side, String::new());
            }
        }
        Cmd::Refresh => reload_active(model, side, out),
        Cmd::NewTab => new_tab(model, side, out),
        Cmd::CloseTab => {
            let ix = model.pane(side).active;
            close_tab(model, side, ix, out);
        }
        Cmd::NextTab => {
            let pane = model.pane(side);
            let ix = (pane.active + 1) % pane.tabs.len();
            select_tab(model, side, ix, out);
        }
        Cmd::PrevTab => {
            let pane = model.pane(side);
            let ix = (pane.active + pane.tabs.len() - 1) % pane.tabs.len();
            select_tab(model, side, ix, out);
        }
        Cmd::TogglePlace => toggle_place(model, side, out),
        Cmd::GoToPlace(index) => go_to_place(model, side, index, out),
        _ => {}
    }
}

pub fn input(model: &mut Model, side: Side, input: PaneInput, out: &mut Out) {
    if !model.ready {
        return;
    }
    match input {
        PaneInput::Click { row, toggle, range } => {
            model.active = side;
            let tab = model.pane_mut(side).tab_mut();
            if row >= tab.order.len() {
                return;
            }
            if toggle {
                tab.cursor = row;
                toggle_mark_at(tab, row);
            } else if range {
                let (a, b) = if tab.anchor <= row { (tab.anchor, row) } else { (row, tab.anchor) };
                let names: Vec<Vec<u8>> = (a..=b.min(tab.order.len() - 1))
                    .filter_map(|r| tab.entry(r).map(|e| e.name.clone()))
                    .collect();
                Arc::make_mut(&mut tab.marked).extend(names);
                tab.cursor = row;
            } else {
                tab.cursor = row;
                tab.anchor = row;
            }
            tab.update_marked_totals();
        }
        PaneInput::DoubleClick { row } => {
            model.active = side;
            let tab = model.pane_mut(side).tab_mut();
            if row < tab.order.len() {
                tab.cursor = row;
                open_row(model, side, out);
            }
        }
        PaneInput::ContextClick { row } => {
            model.active = side;
            let tab = model.pane_mut(side).tab_mut();
            let marked = tab.entry(row).is_some_and(|e| tab.marked.contains(&e.name));
            if !marked && row < tab.order.len() {
                tab.cursor = row;
                tab.anchor = row;
            }
        }
        PaneInput::SortBy(key) => {
            let (show_hidden, folders_first) = display_flags(model);
            let tab = model.pane_mut(side).tab_mut();
            if tab.sort.key == key {
                tab.sort.desc = !tab.sort.desc;
            } else {
                tab.sort = Sort { key, desc: false };
            }
            rebuild(tab, None, show_hidden, folders_first);
        }
        PaneInput::SelectTab(ix) => select_tab(model, side, ix, out),
        PaneInput::CloseTab(ix) => close_tab(model, side, ix, out),
        PaneInput::Navigate(path) => {
            model.active = side;
            navigate(model, side, path, None, out);
        }
        PaneInput::OpenLocal(path) => {
            model.active = side;
            open_location(model, side, Loc { conn: LOCAL, path }, false, out);
        }
        PaneInput::OpenBookmark(id) => ops::open_bookmark(model, id, side, out),
        PaneInput::RenamePlace(id) => {
            let Some(name) = model.bookmarks.places.iter().find(|p| p.id == id).map(|p| p.name.clone()) else {
                return;
            };
            dialog::ask_input(model, "Rename Place", "", name, None, "Rename", InputPurpose::RenamePlace { id });
        }
        PaneInput::MovePlace { id, delta } => {
            if Arc::make_mut(&mut model.bookmarks).move_place(&id, delta) {
                model.save_bookmarks(out, "places");
            }
        }
        PaneInput::RemovePlace(id) => {
            if Arc::make_mut(&mut model.bookmarks).remove_place(&id) {
                model.save_bookmarks(out, "places");
            }
        }
        PaneInput::Retry => reload_active(model, side, out),
        PaneInput::Reconnect => {
            let conn = model.conns.host_of(model.pane(side).loc().conn);
            if let Some(bookmark) = model.conns.get(conn).and_then(|c| c.bookmark.clone()) {
                ops::open_bookmark(model, bookmark, side, out);
            }
        }
        PaneInput::Disconnect => {
            let conn = model.conns.host_of(model.pane(side).loc().conn);
            ops::disconnect(model, conn, out);
        }
        PaneInput::LocationsShown => ops::refresh_local_folders(out),
        PaneInput::Drop { drag, mode } => {
            if drag.side != side {
                ops::transfer_drop(model, drag, side, mode, out);
            }
        }
    }
}

pub fn reply(model: &mut Model, reply: PaneReply, out: &mut Out) {
    match reply {
        PaneReply::Listed { tab, generation, out: result } => listed(model, tab, generation, result, out),
        PaneReply::ArchiveOpened { tab, from, file, name, out: result } => {
            archive_opened(model, tab, from, file, name, result, out)
        }
        PaneReply::Elevated { tab, out: result } => {
            let FsOutput::Done(result) = result else { return };
            match result {
                Ok(()) => {
                    if let Some((side, ix)) = find_tab(model, tab) {
                        model.pane_mut(side).tabs[ix].elevated = true;
                        load(model, side, ix, None, out);
                        ops::location_changed(model, out);
                    }
                }
                Err(VfsError::Cancelled) => {}
                Err(err) => out.error(err.to_string()),
            }
        }
    }
}

/// Prepares sudo on a tab's file system, then lists it elevated.
pub fn elevate(model: &mut Model, tab: TabId, out: &mut Out) {
    let Some((side, ix)) = find_tab(model, tab) else { return };
    let conn = model.pane(side).tabs[ix].loc.conn;
    out.fs(FsOp::PrepareElevation { conn }, move |out| Reply::Pane(PaneReply::Elevated { tab, out }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut names = vec!["file10", "File2", "file1", "a", "B", "file02"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["a", "B", "file1", "File2", "file02", "file10"]);
        assert_eq!(natural_cmp("abc", "ABC"), "abc".cmp("ABC"));
        assert_eq!(natural_cmp("x9", "x10"), Ordering::Less);
        assert_eq!(natural_cmp("x", "x1"), Ordering::Less);
        assert_eq!(natural_cmp("Äb", "äa"), Ordering::Greater);
    }

    #[test]
    fn filters_match_names_or_wildcards() {
        assert!(matches_filter("logrotate.conf", "log"));
        assert!(matches_filter("app.log", "LOG"));
        assert!(matches_filter("app.log", "*.log"));
        assert!(!matches_filter("logrotate.conf", "*.log"));
    }
}
