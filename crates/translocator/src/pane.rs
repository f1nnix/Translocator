//! One file pane: tabs, a virtualized file list with a cursor and marks,
//! sorting, history and type-ahead search.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, Theme, WindowExt as _, h_flex, v_flex};
use gpui_kit::{prelude::*, *};
use serde::{Deserialize, Serialize};
use tl_core::archive::ArchiveFormat;
use tl_core::util::{human_size, wildcard_match};
use tl_core::{Entry, EntryKind, OpCtx, Place, SpaceInfo, VPath, Vfs, VfsError};

use crate::actions::*;
use crate::app_state::AppState;
use crate::dialogs;
use crate::util::{blocking, entry_icon, format_date, natural_cmp};

const EXT_W: f32 = 52.;
const SIZE_W: f32 = 74.;
const DATE_W: f32 = 116.;
const MODE_W: f32 = 84.;

/// Height of one row, which the settings make denser or roomier. Every row
/// is the same height: the list is virtualized and measures only the first.
fn row_height(cx: &App) -> Pixels {
    px(cx.global::<AppState>().settings.general.row_height())
}

/// Size of the text in the list, which [`crate::themes::tweaks`] keeps in
/// step with the setting.
fn font_size(cx: &App) -> Pixels {
    cx.theme().font_size
}

/// The sort a new tab starts with.
fn default_sort(cx: &App) -> Sort {
    let general = &cx.global::<AppState>().settings.general;
    Sort {
        key: general.sort,
        desc: general.sort_descending,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
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
}

/// Where a pane (tab) is: a file system and a directory in it.
#[derive(Clone)]
pub struct Location {
    pub vfs: Arc<dyn Vfs>,
    pub path: VPath,
}

impl Location {
    pub fn local(path: VPath) -> Self {
        Location {
            vfs: tl_local::local_vfs(),
            path,
        }
    }

    pub fn is_local(&self) -> bool {
        self.vfs.plugin_id() == tl_local::PLUGIN_ID
    }

    /// Inside an archive: the folder that holds the archive file.
    pub fn archive_folder(&self) -> Option<Location> {
        self.vfs.host().map(|(vfs, file)| Location {
            vfs,
            path: file.parent().unwrap_or_else(VPath::root),
        })
    }

    /// The location on a real file system: this one, or for archives the
    /// same path on the archive's host (`~/site.tar.gz/www`).
    pub fn on_host(&self) -> Location {
        match self.vfs.host() {
            Some((vfs, _)) => Location {
                vfs,
                path: self.path.clone(),
            },
            None => self.clone(),
        }
    }

    /// `~/Projects` locally, `deploy@web-1:/etc` remotely.
    pub fn display(&self) -> String {
        if self.vfs.host().is_some() {
            return self.on_host().display();
        }
        if self.is_local() {
            tilde(&self.path)
        } else {
            format!("{}:{}", self.vfs.label(), self.path)
        }
    }
}

/// Shortens the home directory to `~`.
pub fn tilde(path: &VPath) -> String {
    let text = path.display();
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if text == home {
            return "~".into();
        }
        if let Some(rest) = text.strip_prefix(&format!("{home}/")) {
            return format!("~/{rest}");
        }
    }
    text
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortKey {
    #[default]
    Name,
    Ext,
    Size,
    Modified,
}

impl SortKey {
    /// Name in the settings file and the settings window's dropdown.
    pub fn id(&self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Ext => "extension",
            SortKey::Size => "size",
            SortKey::Modified => "modified",
        }
    }

    pub fn from_id(id: &str) -> SortKey {
        match id {
            "extension" => SortKey::Ext,
            "size" => SortKey::Size,
            "modified" => SortKey::Modified,
            _ => SortKey::Name,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub desc: bool,
}

#[derive(Clone)]
pub struct Row {
    pub entry: Entry,
    pub name: SharedString,
    pub parent: bool,
}

/// The items an operation acts on: marked rows, or the cursor row.
#[derive(Clone)]
pub struct Selection {
    pub vfs: Arc<dyn Vfs>,
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
        match self.entries.as_slice() {
            [one] => format!("“{}”", one.name_lossy()),
            many => format!("{} items", many.len()),
        }
    }
}

/// Payload of a drag from a pane.
#[derive(Clone)]
pub struct DraggedFiles {
    pub side: Side,
    pub vfs: Arc<dyn Vfs>,
    pub dir: VPath,
    pub names: Rc<Vec<Vec<u8>>>,
    pub elevated: bool,
}

struct DragPreview {
    label: SharedString,
}

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .shadow_md()
            .text_size(font_size(cx))
            .child(Icon::new(Lucide::Files).small())
            .child(self.label.clone())
    }
}

pub enum PaneEvent {
    Focused,
    LocationChanged,
    Error(SharedString),
    /// Listing failed with "permission denied": offer sudo.
    PermissionDenied(VPath),
    OpenFile(Entry),
    OpenBookmark(String),
    /// A folder was saved, renamed, moved or dropped: both rows redraw.
    PlacesChanged,
    NewBookmark,
    ManageBookmarks,
    Disconnect,
    Dropped(DraggedFiles),
    DroppedExternal(Vec<PathBuf>),
}

pub struct TabState {
    pub loc: Location,
    rows: Vec<Row>,
    raw: Vec<Entry>,
    cursor: usize,
    /// Mark anchor for shift-click ranges.
    anchor: usize,
    marked: HashSet<Vec<u8>>,
    /// Only the entries whose name matches this are listed; empty lists
    /// everything. One per tab, so each tab keeps its own.
    filter: String,
    /// The entry a caller asked to put the cursor on, until a listing has
    /// managed to. A folder that changes while it is being listed is listed
    /// again, and the request has to outlive the listing it was made with.
    pending_select: Option<Vec<u8>>,
    sort: Sort,
    pub elevated: bool,
    loading: bool,
    error: Option<SharedString>,
    back: Vec<Location>,
    forward: Vec<Location>,
    space: Option<SpaceInfo>,
    scroll: UniformListScrollHandle,
    dir_sizes: HashMap<Vec<u8>, u64>,
    load_task: Option<Task<()>>,
    watch_task: Option<Task<()>>,
    watcher: Option<notify::RecommendedWatcher>,
}

impl TabState {
    fn new(loc: Location, elevated: bool, sort: Sort) -> Self {
        TabState {
            loc,
            rows: Vec::new(),
            raw: Vec::new(),
            cursor: 0,
            anchor: 0,
            marked: HashSet::new(),
            filter: String::new(),
            pending_select: None,
            sort,
            elevated,
            loading: false,
            error: None,
            back: Vec::new(),
            forward: Vec::new(),
            space: None,
            scroll: UniformListScrollHandle::new(),
            dir_sizes: HashMap::new(),
            load_task: None,
            watch_task: None,
            watcher: None,
        }
    }

    /// Forgets what was shown for the previous folder.
    fn reset_view(&mut self) {
        self.pending_select = None;
        self.marked.clear();
        self.dir_sizes.clear();
        self.error = None;
        self.cursor = 0;
        self.anchor = 0;
        self.rows.clear();
        self.raw.clear();
        self.space = None;
        self.scroll = UniformListScrollHandle::new();
        self.watcher = None;
        self.watch_task = None;
    }

    fn title(&self) -> SharedString {
        let loc = self.loc.on_host();
        let name = loc.path.file_name_lossy();
        if loc.is_local() {
            name.into()
        } else {
            format!("{} · {}", loc.vfs.label(), name).into()
        }
    }
}

pub struct FilePane {
    pub side: Side,
    pub focus: FocusHandle,
    tabs: Vec<TabState>,
    active: usize,
    /// The two settings the rows are built from, as this pane last built
    /// them: a change to either rebuilds, anything else in the settings
    /// does not.
    show_hidden: bool,
    folders_first: bool,
    narrow: u8,
    /// How many characters of path fit in the header.
    crumb_chars: usize,
    /// Top-left of the file list, where the keyboard locations menu opens.
    pub(crate) list_origin: Point<Pixels>,
    popup: Option<(Entity<PopupMenu>, Subscription)>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PaneEvent> for FilePane {}

impl Focusable for FilePane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Whether a name passes the pane's filter.
///
/// The whole name is matched, extension included, so `log` finds both
/// `logrotate.conf` and `app.log`. A query holding `*` or `?` is a
/// wildcard instead, matching the name end to end, so `*.log` finds only
/// the second — and `;` separates several, as the mark masks do.
pub(crate) fn matches_filter(name: &str, query: &str) -> bool {
    if query.contains('*') || query.contains('?') {
        return wildcard_match(query, name);
    }
    name.to_lowercase().contains(&query.to_lowercase())
}

fn build_rows(
    raw: &[Entry],
    sort: Sort,
    folders_first: bool,
    show_hidden: bool,
    at_root: bool,
    filter: &str,
) -> Vec<Row> {
    let query = filter.trim();
    let mut rows: Vec<Row> = raw
        .iter()
        .filter(|e| show_hidden || !e.is_hidden())
        .map(|e| Row {
            entry: e.clone(),
            name: e.name_lossy().into(),
            parent: false,
        })
        .filter(|r| query.is_empty() || matches_filter(&r.name, query))
        .collect();
    rows.sort_by(|a, b| {
        let (ad, bd) = (a.entry.is_dir(), b.entry.is_dir());
        if folders_first && ad != bd {
            return bd.cmp(&ad);
        }
        let by_name = || natural_cmp(&a.name, &b.name).then_with(|| a.entry.name.cmp(&b.entry.name));
        let ord = match sort.key {
            SortKey::Name => by_name(),
            SortKey::Ext => a
                .entry
                .extension()
                .map(|e| e.to_ascii_lowercase())
                .cmp(&b.entry.extension().map(|e| e.to_ascii_lowercase()))
                .then_with(by_name),
            SortKey::Size if ad => by_name(),
            SortKey::Size => a.entry.size.cmp(&b.entry.size).then_with(by_name),
            SortKey::Modified => a.entry.mtime.cmp(&b.entry.mtime).then_with(by_name),
        };
        if sort.desc { ord.reverse() } else { ord }
    });
    if !at_root {
        rows.insert(
            0,
            Row {
                entry: Entry::new(b"..".to_vec(), EntryKind::Dir),
                name: "..".into(),
                parent: true,
            },
        );
    }
    rows
}

impl FilePane {
    pub fn new(side: Side, loc: Location, elevated: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let subs = vec![
            cx.on_focus(&focus, window, |_this: &mut Self, _window, cx| {
                cx.emit(PaneEvent::Focused);
                cx.notify();
            }),
            cx.on_blur(&focus, window, |_this: &mut Self, _window, cx| cx.notify()),
            cx.observe_global::<AppState>(Self::settings_changed),
        ];
        let general = cx.global::<AppState>().settings.general.clone();
        let sort = default_sort(cx);
        let mut pane = FilePane {
            side,
            focus,
            tabs: vec![TabState::new(loc, elevated, sort)],
            active: 0,
            show_hidden: general.show_hidden,
            folders_first: general.folders_first,
            narrow: 0,
            crumb_chars: 60,
            list_origin: Point::default(),
            popup: None,
            _subs: subs,
        };
        pane.load(0, None, cx);
        pane
    }

    pub fn tab(&self) -> &TabState {
        &self.tabs[self.active]
    }

    fn tab_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.active]
    }

    pub fn location(&self) -> &Location {
        &self.tab().loc
    }

    pub fn elevated(&self) -> bool {
        self.tab().elevated
    }

    pub fn cursor_row(&self) -> Option<&Row> {
        self.tab().rows.get(self.tab().cursor)
    }

    /// Marked entries in display order, or the entry under the cursor.
    pub fn selection(&self) -> Option<Selection> {
        let tab = self.tab();
        let entries: Vec<Entry> = if tab.marked.is_empty() {
            match tab.rows.get(tab.cursor) {
                Some(row) if !row.parent => vec![row.entry.clone()],
                _ => return None,
            }
        } else {
            tab.rows
                .iter()
                .filter(|r| !r.parent && tab.marked.contains(&r.entry.name))
                .map(|r| r.entry.clone())
                .collect()
        };
        Some(Selection {
            vfs: tab.loc.vfs.clone(),
            dir: tab.loc.path.clone(),
            entries,
            elevated: tab.elevated,
        })
    }

    /// The entries listed in the active tab (as shown: hidden files only
    /// when they are visible).
    pub fn listed_entries(&self) -> Vec<Entry> {
        self.tab().rows.iter().filter(|r| !r.parent).map(|r| r.entry.clone()).collect()
    }

    pub fn is_loading(&self) -> bool {
        self.tab().loading
    }

    /// Names of the marked entries, in display order.
    #[cfg(test)]
    pub fn marked(&self) -> Vec<Vec<u8>> {
        let tab = self.tab();
        tab.rows
            .iter()
            .filter(|r| !r.parent && tab.marked.contains(&r.entry.name))
            .map(|r| r.entry.name.clone())
            .collect()
    }

    /// Marks exactly `names` in the active tab and puts the cursor on the
    /// first of them.
    pub fn set_marks(&mut self, names: Vec<Vec<u8>>, cx: &mut Context<Self>) {
        let tab = self.tab_mut();
        tab.marked = names.into_iter().collect();
        if let Some(ix) = tab.rows.iter().position(|r| tab.marked.contains(&r.entry.name)) {
            tab.cursor = ix;
            tab.anchor = ix;
            tab.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    /// Rebuilds the rows when a setting they are built from has changed.
    /// Every pane in every window hears this, so an ordinary change — a
    /// connection opening, a file copied to the clipboard — must cost
    /// nothing, and an uninteresting one returns here.
    fn settings_changed(&mut self, cx: &mut Context<Self>) {
        let general = &cx.global::<AppState>().settings.general;
        let wanted = (general.show_hidden, general.folders_first);
        if wanted == (self.show_hidden, self.folders_first) {
            return;
        }
        (self.show_hidden, self.folders_first) = wanted;
        for ix in 0..self.tabs.len() {
            self.rebuild_rows(ix, None);
        }
        cx.notify();
    }

    // ----- loading --------------------------------------------------------

    fn rebuild_rows(&mut self, ix: usize, select: Option<&[u8]>) {
        let (show_hidden, folders_first) = (self.show_hidden, self.folders_first);
        let tab = &mut self.tabs[ix];
        let keep = select
            .map(<[u8]>::to_vec)
            .or_else(|| tab.rows.get(tab.cursor).map(|r| r.entry.name.clone()));
        tab.rows = build_rows(&tab.raw, tab.sort, folders_first, show_hidden, tab.loc.path.is_root(), &tab.filter);
        // Against the whole listing rather than the visible rows: a mark
        // has to survive the filter being narrowed and widened again, and
        // only a file that has actually gone should lose it.
        let names: HashSet<&[u8]> = tab.raw.iter().map(|e| e.name.as_slice()).collect();
        tab.marked.retain(|n| names.contains(n.as_slice()));
        tab.cursor = keep
            .and_then(|name| tab.rows.iter().position(|r| r.entry.name == name))
            .unwrap_or_else(|| tab.cursor.min(tab.rows.len().saturating_sub(1)));
        tab.scroll.scroll_to_item(tab.cursor, ScrollStrategy::Nearest);
    }

    /// (Re)loads tab `ix`, putting the cursor on `select` if given.
    fn load(&mut self, ix: usize, select: Option<Vec<u8>>, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[ix];
        if select.is_some() {
            tab.pending_select = select;
        }
        tab.loading = true;
        let vfs = tab.loc.vfs.clone();
        let path = tab.loc.path.clone();
        let ctx = OpCtx::new().elevated(tab.elevated);
        let spin_path = path.clone();
        tab.load_task = Some(cx.spawn(async move |this, cx| {
            let list_path = path.clone();
            let list_vfs = vfs.clone();
            let list_ctx = ctx.clone();
            let result = blocking(move || list_vfs.list(&list_path, &list_ctx)).await;
            let space_vfs = vfs.clone();
            let space_path = path.clone();
            let space = blocking(move || space_vfs.free_space(&space_path, &OpCtx::new()))
                .await
                .ok()
                .flatten();
            let _ = this.update(cx, |this, cx| {
                let Some(ix) = this.tabs.iter().position(|t| t.loc.path == spin_path && Arc::ptr_eq(&t.loc.vfs, &vfs)) else {
                    return;
                };
                let tab = &mut this.tabs[ix];
                tab.loading = false;
                tab.space = space;
                match result {
                    Ok(entries) => {
                        tab.raw = entries;
                        tab.error = None;
                        // This listing supersedes any earlier one, so it
                        // owes the cursor to whoever asked last.
                        let select = tab.pending_select.take();
                        this.rebuild_rows(ix, select.as_deref());
                        this.watch(ix, cx);
                    }
                    Err(err) => {
                        tab.error = Some(err.to_string().into());
                        tab.raw.clear();
                        tab.rows = build_rows(&[], tab.sort, true, true, tab.loc.path.is_root(), "");
                        tab.cursor = 0;
                        match err {
                            VfsError::PermissionDenied(_) if tab.loc.vfs.capabilities().elevation && !tab.elevated => {
                                cx.emit(PaneEvent::PermissionDenied(tab.loc.path.clone()));
                            }
                            VfsError::Cancelled => {}
                            other => cx.emit(PaneEvent::Error(other.to_string().into())),
                        }
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Watches local directories so the listing follows outside changes.
    fn watch(&mut self, ix: usize, cx: &mut Context<Self>) {
        use notify::Watcher as _;
        let tab = &mut self.tabs[ix];
        tab.watcher = None;
        tab.watch_task = None;
        if !tab.loc.is_local() {
            return;
        }
        let (tx, rx) = smol::channel::unbounded::<()>();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if res.is_ok() {
                let _ = tx.try_send(());
            }
        });
        let Ok(mut watcher) = watcher else { return };
        if watcher
            .watch(&tab.loc.path.to_local(), notify::RecursiveMode::NonRecursive)
            .is_err()
        {
            return;
        }
        tab.watcher = Some(watcher);
        let path = tab.loc.path.clone();
        tab.watch_task = Some(cx.spawn(async move |this, cx| {
            while rx.recv().await.is_ok() {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                while rx.try_recv().is_ok() {}
                let alive = this.update(cx, |this, cx| {
                    if let Some(ix) = this.tabs.iter().position(|t| t.loc.is_local() && t.loc.path == path)
                        && !this.tabs[ix].loading {
                            this.load(ix, None, cx);
                        }
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let ix = self.active;
        self.load(ix, None, cx);
    }

    /// Reloads every tab that shows `dir` on the file system `key`.
    pub fn refresh_dir(&mut self, key: &str, dir: &VPath, cx: &mut Context<Self>) {
        for ix in 0..self.tabs.len() {
            let tab = &self.tabs[ix];
            if tab.loc.vfs.location_key() == key && &tab.loc.path == dir {
                self.load(ix, None, cx);
            }
        }
    }

    /// Reloads and moves the cursor to `name` (after create/rename/copy).
    pub fn reload_select(&mut self, name: Vec<u8>, cx: &mut Context<Self>) {
        let ix = self.active;
        self.tabs[ix].marked.clear();
        self.load(ix, Some(name), cx);
    }

    pub fn navigate(&mut self, path: VPath, select: Option<Vec<u8>>, cx: &mut Context<Self>) {
        // Above an archive's root: back on the file system that holds it.
        if let Some((host, file)) = self.location().vfs.host()
            && !path.starts_with(&file)
        {
            self.enter(Location { vfs: host, path }, select, cx);
            return;
        }
        self.tab_mut().filter.clear();
        let tab = self.tab_mut();
        if tab.loc.path != path {
            let old = tab.loc.clone();
            tab.loc.path = path;
            tab.back.push(old);
            tab.forward.clear();
            tab.pending_select = None;
            tab.marked.clear();
            tab.dir_sizes.clear();
            tab.error = None;
            tab.cursor = 0;
            tab.anchor = 0;
            tab.rows.clear();
            tab.scroll = UniformListScrollHandle::new();
        }
        let ix = self.active;
        self.load(ix, select, cx);
        cx.emit(PaneEvent::LocationChanged);
    }

    /// Moves the active tab into or out of an archive, keeping its history
    /// and sudo state.
    fn enter(&mut self, loc: Location, select: Option<Vec<u8>>, cx: &mut Context<Self>) {
        self.tab_mut().filter.clear();
        let tab = self.tab_mut();
        let old = std::mem::replace(&mut tab.loc, loc);
        tab.back.push(old);
        tab.forward.clear();
        tab.reset_view();
        let ix = self.active;
        self.load(ix, select, cx);
        cx.emit(PaneEvent::LocationChanged);
    }

    /// Opens the archive `name` in the current folder as a folder.
    pub fn open_archive(&mut self, name: Vec<u8>, cx: &mut Context<Self>) {
        let loc = self.location().clone();
        let ctx = OpCtx::new().elevated(self.elevated());
        let file = loc.path.child(&name);
        self.tab_mut().loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (host, target) = (loc.vfs.clone(), file.clone());
            let result = blocking(move || tl_core::archive::open(host, target, &ctx)).await;
            let _ = this.update(cx, |this, cx| {
                this.tab_mut().loading = false;
                // Not if the pane moved on meanwhile.
                let here = this.location();
                let moved = here.path != loc.path || !Arc::ptr_eq(&here.vfs, &loc.vfs);
                match result {
                    Ok(_) if moved => {}
                    Ok(vfs) => this.enter(Location { vfs, path: file }, None, cx),
                    Err(err) => cx.emit(PaneEvent::Error(
                        format!("Cannot open {}: {err}", String::from_utf8_lossy(&name)).into(),
                    )),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Switches the active tab to another file system.
    pub fn open_location(&mut self, loc: Location, elevated: bool, cx: &mut Context<Self>) {
        self.tab_mut().filter.clear();
        let tab = self.tab_mut();
        let same_fs = Arc::ptr_eq(&tab.loc.vfs, &loc.vfs);
        if same_fs {
            self.navigate(loc.path, None, cx);
            return;
        }
        let sort = tab.sort;
        *tab = TabState::new(loc, elevated, sort);
        let ix = self.active;
        self.load(ix, None, cx);
        cx.emit(PaneEvent::LocationChanged);
    }

    pub fn set_elevated(&mut self, elevated: bool, cx: &mut Context<Self>) {
        self.tab_mut().elevated = elevated;
        self.reload(cx);
        cx.emit(PaneEvent::LocationChanged);
    }

    pub fn set_dir_size(&mut self, name: Vec<u8>, size: u64, cx: &mut Context<Self>) {
        self.tab_mut().dir_sizes.insert(name, size);
        cx.notify();
    }

    /// Replaces every tab on a disconnected file system (or inside an
    /// archive on it) with the home folder.
    pub fn drop_connection(&mut self, vfs: &Arc<dyn Vfs>, cx: &mut Context<Self>) {
        for ix in 0..self.tabs.len() {
            let tab_vfs = &self.tabs[ix].loc.vfs;
            if Arc::ptr_eq(tab_vfs, vfs) || tab_vfs.host().is_some_and(|(host, _)| Arc::ptr_eq(&host, vfs)) {
                let home = tl_local::local_vfs().home();
                let sort = self.tabs[ix].sort;
                self.tabs[ix] = TabState::new(Location::local(home), false, sort);
                self.load(ix, None, cx);
            }
        }
        cx.emit(PaneEvent::LocationChanged);
    }

    // ----- cursor & marks -------------------------------------------------

    fn page_rows(&self) -> usize {
        let state = self.tab().scroll.0.borrow();
        match state.last_item_size {
            Some(size) if !self.tab().rows.is_empty() => {
                let row_h = size.contents.height / self.tab().rows.len() as f32;
                ((size.item.height / row_h).floor() as usize).saturating_sub(1).max(1)
            }
            _ => 20,
        }
    }

    fn move_cursor(&mut self, to: usize, cx: &mut Context<Self>) {
        let tab = self.tab_mut();
        if tab.rows.is_empty() {
            return;
        }
        tab.cursor = to.min(tab.rows.len() - 1);
        tab.anchor = tab.cursor;
        tab.scroll.scroll_to_item(tab.cursor, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn toggle_mark_at(&mut self, ix: usize) {
        let tab = self.tab_mut();
        if let Some(row) = tab.rows.get(ix) {
            if row.parent {
                return;
            }
            let name = row.entry.name.clone();
            if !tab.marked.remove(&name) {
                tab.marked.insert(name);
            }
        }
    }

    fn cursor_up(&mut self, _: &CursorUp, _: &mut Window, cx: &mut Context<Self>) {
        let to = self.tab().cursor.saturating_sub(1);
        self.move_cursor(to, cx);
    }

    fn cursor_down(&mut self, _: &CursorDown, _: &mut Window, cx: &mut Context<Self>) {
        let to = self.tab().cursor + 1;
        self.move_cursor(to, cx);
    }

    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        let to = self.tab().cursor.saturating_sub(self.page_rows());
        self.move_cursor(to, cx);
    }

    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        let to = self.tab().cursor + self.page_rows();
        self.move_cursor(to, cx);
    }

    fn cursor_home(&mut self, _: &CursorHome, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(0, cx);
    }

    fn cursor_end(&mut self, _: &CursorEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_cursor(usize::MAX, cx);
    }

    fn mark_up(&mut self, _: &MarkUp, _: &mut Window, cx: &mut Context<Self>) {
        let cur = self.tab().cursor;
        self.toggle_mark_at(cur);
        self.move_cursor(cur.saturating_sub(1), cx);
    }

    fn mark_down(&mut self, _: &MarkDown, _: &mut Window, cx: &mut Context<Self>) {
        let cur = self.tab().cursor;
        self.toggle_mark_at(cur);
        self.move_cursor(cur + 1, cx);
    }

    fn toggle_mark(&mut self, _: &ToggleMark, _: &mut Window, cx: &mut Context<Self>) {
        let cur = self.tab().cursor;
        self.toggle_mark_at(cur);
        self.move_cursor(cur + 1, cx);
    }

    fn mark_all(&mut self, _: &MarkAll, _: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab_mut();
        tab.marked = tab.rows.iter().filter(|r| !r.parent).map(|r| r.entry.name.clone()).collect();
        cx.notify();
    }

    fn unmark_all(&mut self, _: &UnmarkAll, _: &mut Window, cx: &mut Context<Self>) {
        self.tab_mut().marked.clear();
        cx.notify();
    }

    fn invert_marks(&mut self, _: &InvertMarks, _: &mut Window, cx: &mut Context<Self>) {
        self.invert(cx);
    }

    fn invert(&mut self, cx: &mut Context<Self>) {
        let tab = self.tab_mut();
        let all: HashSet<Vec<u8>> = tab.rows.iter().filter(|r| !r.parent && !r.entry.is_dir()).map(|r| r.entry.name.clone()).collect();
        let dirs: HashSet<Vec<u8>> = tab.marked.iter().filter(|n| !all.contains(*n)).cloned().collect();
        tab.marked = all.difference(&tab.marked).cloned().collect::<HashSet<_>>();
        tab.marked.extend(dirs);
        cx.notify();
    }

    fn mark_by_mask(&mut self, _: &MarkByMask, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_mask(true, window, cx);
    }

    fn unmark_by_mask(&mut self, _: &UnmarkByMask, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_mask(false, window, cx);
    }

    fn ask_mask(&mut self, mark: bool, window: &mut Window, cx: &mut Context<Self>) {
        let title = if mark { "Mark Files" } else { "Unmark Files" };
        let answer = dialogs::ask_input(
            window,
            cx,
            title,
            "Wildcards like *.log or img_??.png; separate several with ;",
            "*",
            None,
            if mark { "Mark" } else { "Unmark" },
        );
        cx.spawn(async move |this, cx| {
            let Some(mask) = answer.await else { return };
            let _ = this.update(cx, |this, cx| {
                let tab = this.tab_mut();
                for row in tab.rows.iter().filter(|r| !r.parent) {
                    if wildcard_match(&mask, &row.name) {
                        if mark {
                            tab.marked.insert(row.entry.name.clone());
                        } else {
                            tab.marked.remove(&row.entry.name);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ----- navigation -----------------------------------------------------

    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.cursor_row().cloned() else { return };
        if row.parent {
            self.go_up(cx);
        } else if row.entry.is_dir() {
            let path = self.location().path.child(&row.entry.name);
            self.navigate(path, None, cx);
        } else if Self::is_archive(&row.entry) && self.location().vfs.host().is_none() {
            self.open_archive(row.entry.name.clone(), cx);
        } else {
            cx.emit(PaneEvent::OpenFile(row.entry));
        }
    }

    /// A file the pane can open as a folder (archives in archives open
    /// with their app instead).
    pub fn is_archive(entry: &Entry) -> bool {
        !entry.is_dir() && ArchiveFormat::detect(&entry.name).is_some()
    }

    fn open_archive_action(&mut self, _: &OpenArchive, window: &mut Window, cx: &mut Context<Self>) {
        match self.cursor_row().cloned() {
            Some(row) if !row.parent && Self::is_archive(&row.entry) => {
                if self.location().vfs.host().is_some() {
                    cx.emit(PaneEvent::Error("Archives inside archives cannot be opened as folders.".into()));
                } else {
                    self.open_archive(row.entry.name.clone(), cx);
                }
            }
            _ => self.open(&Open, window, cx),
        }
    }

    fn go_up(&mut self, cx: &mut Context<Self>) {
        let path = self.location().path.clone();
        if let Some(parent) = path.parent() {
            let from = path.file_name().map(<[u8]>::to_vec);
            self.navigate(parent, from, cx);
        }
    }

    fn go_parent(&mut self, _: &GoParent, _: &mut Window, cx: &mut Context<Self>) {
        self.go_up(cx);
    }

    /// Backspace: one character off the filter, or up a folder when there
    /// is no filter to shorten.
    fn filter_backspace(&mut self, _: &FilterBackspace, _: &mut Window, cx: &mut Context<Self>) {
        let mut query = self.tab().filter.clone();
        if query.pop().is_none() {
            self.go_up(cx);
            return;
        }
        self.set_filter(query, cx);
    }

    fn go_back(&mut self, _: &GoBack, _: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab_mut();
        if let Some(prev) = tab.back.pop() {
            let cur = std::mem::replace(&mut tab.loc, prev);
            // Land on the folder (or archive) we came out of.
            let select = cur
                .path
                .relative_to(&tab.loc.path)
                .and_then(|rel| rel.split(|b| *b == b'/').next())
                .filter(|name| !name.is_empty())
                .map(<[u8]>::to_vec);
            tab.forward.push(cur);
            tab.reset_view();
            let ix = self.active;
            self.load(ix, select, cx);
            cx.emit(PaneEvent::LocationChanged);
        }
    }

    fn go_forward(&mut self, _: &GoForward, _: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab_mut();
        if let Some(next) = tab.forward.pop() {
            let cur = std::mem::replace(&mut tab.loc, next);
            tab.back.push(cur);
            tab.reset_view();
            let ix = self.active;
            self.load(ix, None, cx);
            cx.emit(PaneEvent::LocationChanged);
        }
    }

    fn go_home(&mut self, _: &GoHome, _: &mut Window, cx: &mut Context<Self>) {
        let home = self.location().vfs.home();
        self.navigate(home, None, cx);
    }

    fn go_to_path(&mut self, _: &GoToPath, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.location().path.display();
        let answer = dialogs::ask_input(
            window,
            cx,
            "Go to Folder",
            format!("Path on {}. Relative paths and ~ work.", self.location().vfs.label()),
            current,
            None,
            "Go",
        );
        cx.spawn(async move |this, cx| {
            let Some(text) = answer.await else { return };
            let _ = this.update(cx, |this, cx| {
                let loc = this.location().clone();
                let text = text.trim();
                let target = if text == "~" {
                    loc.vfs.home()
                } else if let Some(rest) = text.strip_prefix("~/") {
                    loc.vfs.home().join(rest)
                } else {
                    loc.path.join(text)
                };
                this.navigate(target, None, cx);
            });
        })
        .detach();
    }

    fn locations(&mut self, _: &Locations, window: &mut Window, cx: &mut Context<Self>) {
        self.open_locations(window, cx);
    }

    /// The location menu as a keyboard-driven popup (⌘K, ⌥F1/⌥F2).
    pub fn open_locations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = cx.weak_entity();
        let info = MenuInfo::of(self, cx);
        let menu = PopupMenu::build(window, cx, move |menu, window, cx| {
            location_menu(menu, pane.clone(), info.clone(), window, cx)
        });
        let handle = menu.focus_handle(cx);
        window.focus(&handle, cx);
        let sub = cx.subscribe_in(&menu, window, |this: &mut Self, _, _: &DismissEvent, window, cx| {
            this.popup = None;
            // Give focus back unless a menu item opened a dialog.
            if !window.has_active_dialog(cx) {
                window.focus(&this.focus, cx);
            }
            cx.notify();
        });
        self.popup = Some((menu, sub));
        cx.notify();
    }

    // ----- places ---------------------------------------------------------

    /// Which connection the row of saved folders belongs to. A pane inside
    /// an archive shows the places of the file system holding it, so a
    /// click there leaves the archive.
    pub fn host_key(&self) -> String {
        self.tab().loc.on_host().vfs.location_key().to_string()
    }

    /// Whether this host has any saved folder; with none, the row would be
    /// an empty strip, so it is left out.
    pub fn has_places(&self, cx: &App) -> bool {
        !cx.global::<AppState>().bookmarks.places(&self.host_key()).is_empty()
    }

    /// The folder on that host, or `None` inside an archive, where a path
    /// means nothing to the host.
    fn host_path(&self) -> Option<VPath> {
        let loc = &self.tab().loc;
        loc.vfs.host().is_none().then(|| loc.path.clone())
    }

    /// ⌘D: saves this folder on the row, or takes it off again.
    fn toggle_place(&mut self, _: &TogglePlace, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.host_path() else {
            cx.emit(PaneEvent::Error("A folder inside an archive cannot be saved.".into()));
            return;
        };
        let host = self.host_key();
        let state = cx.global_mut::<AppState>();
        let saved = state.bookmarks.place_of(&host, &path).map(|p| p.id.clone());
        let result = match saved {
            Some(id) => state.bookmarks.remove_place(&id),
            None => {
                let name = if path.is_root() { "/".to_string() } else { path.file_name_lossy() };
                state.bookmarks.add_place(Place::new(host, name, path))
            }
        };
        self.after_places(result, cx);
    }

    /// ⌘1…⌘9: the n-th folder on this host's row.
    fn go_to_place(&mut self, action: &GoToPlace, _: &mut Window, cx: &mut Context<Self>) {
        let host = self.host_key();
        let target = cx
            .global::<AppState>()
            .bookmarks
            .place_at(&host, action.index)
            .map(|p| p.path.clone());
        if let Some(path) = target {
            self.navigate(path, None, cx);
        }
    }

    fn ask_place_name(&mut self, id: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let answer = dialogs::ask_input(window, cx, "Rename Place", "", name, None, "Rename");
        cx.spawn(async move |this, cx| {
            let Some(answer) = answer.await else { return };
            let name = answer.trim().to_string();
            if name.is_empty() {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                let result = cx.global_mut::<AppState>().bookmarks.rename_place(&id, name);
                this.after_places(result, cx);
            });
        })
        .detach();
    }

    fn remove_place(&mut self, id: &str, cx: &mut Context<Self>) {
        let result = cx.global_mut::<AppState>().bookmarks.remove_place(id);
        self.after_places(result, cx);
    }

    fn move_place(&mut self, id: &str, delta: isize, cx: &mut Context<Self>) {
        let result = cx.global_mut::<AppState>().bookmarks.move_place(id, delta);
        self.after_places(result, cx);
    }

    /// Both panes draw their row from the store, so a change on one side
    /// has to reach the other.
    fn after_places(&mut self, result: std::io::Result<()>, cx: &mut Context<Self>) {
        match result {
            Ok(()) => {
                cx.emit(PaneEvent::PlacesChanged);
                cx.notify();
            }
            Err(err) => cx.emit(PaneEvent::Error(format!("Cannot save the places: {err}").into())),
        }
    }

    fn refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        self.reload(cx);
    }

    // ----- tabs -----------------------------------------------------------

    fn new_tab(&mut self, _: &NewTab, _: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab();
        let loc = tab.loc.clone();
        let elevated = tab.elevated;
        let sort = tab.sort;
        self.tabs.insert(self.active + 1, TabState::new(loc, elevated, sort));
        self.active += 1;
        let ix = self.active;
        self.load(ix, None, cx);
        cx.emit(PaneEvent::LocationChanged);
    }

    fn close_tab(&mut self, _: &CloseTab, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.active;
        self.close_tab_at(ix, cx);
    }

    fn close_tab_at(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 {
            return;
        }
        self.tabs.remove(ix);
        if self.active >= self.tabs.len() || self.active > ix {
            self.active = self.active.saturating_sub(1);
        }
        cx.emit(PaneEvent::LocationChanged);
        cx.notify();
    }

    fn select_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.tabs.len() {
            self.active = ix;
            cx.emit(PaneEvent::LocationChanged);
            cx.notify();
        }
    }

    fn next_tab(&mut self, _: &NextTab, _: &mut Window, cx: &mut Context<Self>) {
        let ix = (self.active + 1) % self.tabs.len();
        self.select_tab(ix, cx);
    }

    fn prev_tab(&mut self, _: &PrevTab, _: &mut Window, cx: &mut Context<Self>) {
        let ix = (self.active + self.tabs.len() - 1) % self.tabs.len();
        self.select_tab(ix, cx);
    }

    // ----- filter ---------------------------------------------------------

    #[cfg(test)]
    pub fn filter(&self) -> &str {
        &self.tab().filter
    }

    /// Narrows or widens the list, keeping the cursor on the file it was
    /// on for as long as that file still matches.
    fn set_filter(&mut self, query: String, cx: &mut Context<Self>) {
        let ix = self.active;
        let keep = self.cursor_row().filter(|r| !r.parent).map(|r| r.entry.name.clone());
        self.tabs[ix].filter = query;
        self.rebuild_rows(ix, keep.as_deref());
        let tab = &mut self.tabs[ix];
        let kept = keep.is_some_and(|name| tab.rows.get(tab.cursor).is_some_and(|r| r.entry.name == name));
        if !kept {
            // The file under the cursor no longer matches, and the old
            // index now points at whatever happens to sit there.
            tab.cursor = tab.rows.iter().position(|r| !r.parent).unwrap_or(0);
            tab.anchor = tab.cursor;
            tab.scroll.scroll_to_item(tab.cursor, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    fn clear_filter(&mut self, cx: &mut Context<Self>) {
        if !self.tab().filter.is_empty() {
            self.set_filter(String::new(), cx);
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let mods = ks.modifiers;
        if ks.key == "escape" {
            if !self.tab().filter.is_empty() {
                self.clear_filter(cx);
                cx.stop_propagation();
            }
            return;
        }
        if mods.platform || mods.control || mods.function || mods.alt {
            return;
        }
        let Some(ch) = ks.key_char.as_deref() else { return };
        if ch.is_empty() || ch.chars().any(char::is_control) {
            return;
        }
        if self.tab().filter.is_empty() {
            // The classic mask keys, which a filter already being typed
            // turns back into ordinary characters. `*` is not among them:
            // `*.log` is how anyone filters by extension, and inverting
            // the marks has ⌘⌥A and the Selection menu.
            match ch {
                "+" => {
                    self.ask_mask(true, window, cx);
                    cx.stop_propagation();
                    return;
                }
                "-" => {
                    self.ask_mask(false, window, cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        let query = format!("{}{ch}", self.tab().filter);
        self.set_filter(query, cx);
        cx.stop_propagation();
    }

    // ----- mouse ----------------------------------------------------------

    fn click_row(&mut self, ix: usize, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        let mods = ev.modifiers;
        let tab = self.tab_mut();
        if ix >= tab.rows.len() {
            return;
        }
        if mods.platform {
            tab.cursor = ix;
            self.toggle_mark_at(ix);
        } else if mods.shift {
            let (a, b) = if tab.anchor <= ix { (tab.anchor, ix) } else { (ix, tab.anchor) };
            for row in &tab.rows[a..=b] {
                if !row.parent {
                    tab.marked.insert(row.entry.name.clone());
                }
            }
            tab.cursor = ix;
        } else {
            tab.cursor = ix;
            tab.anchor = ix;
        }
        cx.notify();
    }

    fn sort_by(&mut self, key: SortKey, cx: &mut Context<Self>) {
        let tab = self.tab_mut();
        if tab.sort.key == key {
            tab.sort.desc = !tab.sort.desc;
        } else {
            tab.sort = Sort { key, desc: false };
        }
        let ix = self.active;
        self.rebuild_rows(ix, None);
        cx.notify();
    }

    // ----- rendering ------------------------------------------------------

    fn render_row(&self, ix: usize, focused: bool, drag_names: &Rc<Vec<Vec<u8>>>, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.tab();
        let row = &tab.rows[ix];
        let theme = cx.theme().clone();
        let entry = &row.entry;
        let is_cursor = ix == tab.cursor;
        let marked = tab.marked.contains(&entry.name);
        let hidden = !row.parent && entry.is_hidden();
        let is_dir = entry.is_dir();

        let icon = if row.parent { Lucide::CornerLeftUp } else { entry_icon(entry) };
        let icon_color = if is_dir {
            theme.blue
        } else if entry.is_executable() {
            theme.green
        } else {
            theme.muted_foreground
        };
        let accent = accent(&theme);
        let fg = if marked { marked_fg(&theme) } else { theme.foreground };
        let muted = theme.muted_foreground;

        let ext: SharedString = if is_dir || row.parent {
            SharedString::default()
        } else {
            entry.extension().map(|e| String::from_utf8_lossy(e).into_owned()).unwrap_or_default().into()
        };
        let size: SharedString = if row.parent {
            "UP".into()
        } else if is_dir {
            match tab.dir_sizes.get(&entry.name) {
                Some(size) => human_size(*size).into(),
                None => "DIR".into(),
            }
        } else {
            human_size(entry.size).into()
        };
        let general = cx.global::<AppState>().settings.general.clone();
        let date: SharedString = if row.parent {
            SharedString::default()
        } else {
            format_date(entry.mtime, general.date_format).into()
        };
        let mode: SharedString = if row.parent { SharedString::default() } else { entry.mode_string().into() };
        let link: Option<SharedString> = entry
            .link_target
            .as_ref()
            .map(|t| format!("→ {}", String::from_utf8_lossy(t)).into());

        let bg = row_bg(&theme, is_cursor, focused, marked);
        let narrow = self.narrow;
        let side = self.side;
        let names = if marked && !drag_names.is_empty() {
            drag_names.clone()
        } else {
            Rc::new(vec![entry.name.clone()])
        };
        let drag = DraggedFiles {
            side,
            vfs: tab.loc.vfs.clone(),
            dir: tab.loc.path.clone(),
            names,
            elevated: tab.elevated,
        };

        h_flex()
            .id(("row", ix))
            .h(row_height(cx))
            .w_full()
            .px_2()
            .gap_2()
            .text_size(font_size(cx))
            .text_color(fg)
            .when_some(bg, |el, bg| el.bg(bg))
            // Always the same 2px, so the row does not shift sideways as
            // the cursor arrives.
            .border_l_2()
            .border_color(if is_cursor && focused { accent } else { accent.opacity(0.) })
            .when(hidden, |el| el.opacity(0.6))
            .when(general.show_icons, |el| {
                el.child(Icon::new(icon).size(px(15.)).flex_shrink_0().text_color(icon_color))
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_shrink(1.)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .when(marked, |d| d.font_weight(FontWeight::SEMIBOLD))
                            .child(row.name.clone()),
                    )
                    .when_some(link, |el, link| {
                        el.child(div().flex_shrink_0().max_w(px(220.)).truncate().text_color(muted).text_xs().child(link))
                    }),
            )
            .when(narrow < 3, |el| {
                el.child(div().w(px(EXT_W)).flex_shrink_0().truncate().text_color(muted).child(ext))
            })
            .child(
                div()
                    .w(px(SIZE_W))
                    .flex_shrink_0()
                    .text_right()
                    .truncate()
                    .when(is_dir || row.parent, |d| d.text_color(muted).text_xs())
                    .child(size),
            )
            .when(narrow < 2, |el| {
                el.child(div().w(px(DATE_W)).flex_shrink_0().text_right().truncate().text_color(muted).child(date))
            })
            .when(narrow < 1, |el| {
                el.child(
                    div()
                        .w(px(MODE_W))
                        .flex_shrink_0()
                        .font_family(theme.mono_font_family.clone())
                        .text_xs()
                        .text_color(muted)
                        .child(mode),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _window, cx| this.click_row(ix, ev, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _ev: &MouseDownEvent, _window, cx| {
                    let tab = this.tab_mut();
                    let marked = tab.rows.get(ix).is_some_and(|r| tab.marked.contains(&r.entry.name));
                    if !marked {
                        tab.cursor = ix;
                        tab.anchor = ix;
                    }
                    cx.notify();
                }),
            )
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                if ev.click_count() == 2 {
                    this.tab_mut().cursor = ix;
                    this.open(&Open, window, cx);
                }
            }))
            .when(!row.parent, |el| {
                el.on_drag(drag, |drag: &DraggedFiles, _offset, _window, cx| {
                    let label = match drag.names.as_slice() {
                        [one] => String::from_utf8_lossy(one).into_owned(),
                        many => format!("{} items", many.len()),
                    };
                    cx.new(|_| DragPreview { label: label.into() })
                })
                .external_drag_payload(|drag: &DraggedFiles, _window, _cx| {
                    if drag.vfs.plugin_id() != tl_local::PLUGIN_ID {
                        return None;
                    }
                    let files = drag.names.iter().map(|n| {
                        let path = drag.dir.child(n).to_local();
                        let is_dir = path.is_dir();
                        (path, is_dir)
                    });
                    Some(ExternalDragPayload::Files(FileDragPaths::new(files)))
                })
            })
            .into_any_element()
    }

    fn render_header(&self, focused: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let tab = self.tab();
        let loc = &tab.loc;
        let caps = loc.vfs.capabilities();
        let in_archive = loc.vfs.host().is_some();
        let host = loc.on_host();
        let is_local = host.is_local();
        let label: SharedString = if is_local { "Local".into() } else { host.vfs.label().into() };
        let elevated = tab.elevated;
        let pane = cx.weak_entity();
        let info = MenuInfo::of(self, cx);

        // Breadcrumb; leading parts collapse into "…" when space runs out.
        let mut crumbs: Vec<(SharedString, VPath)> = Vec::new();
        let mut acc = VPath::root();
        crumbs.push(("/".into(), acc.clone()));
        for comp in loc.path.components() {
            acc = acc.child(comp);
            crumbs.push((String::from_utf8_lossy(comp).into_owned().into(), acc.clone()));
        }
        let last = crumbs.len() - 1;
        let mut budget = self.crumb_chars;
        let mut first_shown = crumbs.len();
        for (i, (name, _)) in crumbs.iter().enumerate().rev() {
            let cost = name.chars().count() + 3;
            if i != last && cost > budget {
                break;
            }
            budget = budget.saturating_sub(cost);
            first_shown = i;
        }
        let ellipsis = first_shown > 0;
        let crumb_els = crumbs.into_iter().enumerate().skip(first_shown).map(|(i, (name, path))| {
            h_flex()
                .flex_shrink_0()
                .when(i > 1 || (ellipsis && i == first_shown), |el| {
                    el.child(div().px(px(2.)).text_color(theme.muted_foreground).child(if ellipsis && i == first_shown { "… /" } else { "/" }))
                })
                .child(
                    div()
                        .id(("crumb", i))
                        .px(px(3.))
                        .rounded_sm()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.list_hover))
                        .when(i == last, |d| d.font_weight(FontWeight::SEMIBOLD).text_color(if elevated { theme.danger } else { theme.foreground }))
                        .when(i != last, |d| d.text_color(theme.muted_foreground))
                        .child(name)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            this.navigate(path.clone(), None, cx);
                        })),
                )
        });

        h_flex()
            .h(px(32.))
            .flex_shrink_0()
            .px_1()
            .gap_1()
            .border_b_1()
            // The active pane carries a tinted location line, so which one
            // it is does not depend on a cursor row being in view.
            .when(focused, |el| el.bg(accent(&theme).opacity(0.10)))
            .border_color(if focused { accent(&theme) } else { theme.border })
            .child(
                Button::new("location")
                    .icon(if in_archive {
                        Lucide::FileArchive
                    } else if is_local {
                        Lucide::HardDrive
                    } else {
                        Lucide::Server
                    })
                    .label(label)
                    .ghost()
                    .small()
                    .compact()
                    .tab_stop(false)
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, window, cx| location_menu(menu, pane.clone(), info.clone(), window, cx)),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_size(font_size(cx))
                    .children(crumb_els),
            )
            .when(tab.loading, |el| {
                el.child(Icon::new(IconName::LoaderCircle).small().text_color(theme.muted_foreground))
            })
            .when(caps.elevation, |el| {
                el.child(
                    Button::new("sudo")
                        .icon(if elevated { Lucide::LockOpen } else { Lucide::Lock })
                        .when(elevated, |b| b.label("sudo").danger())
                        .when(!elevated, |b| b.ghost())
                        .xsmall()
                        .tab_stop(false)
                        .tooltip(if elevated { "Running as root (sudo). Click to drop privileges." } else { "Elevate this pane with sudo (⌘⇧S)" })
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleSudo), cx)),
                )
            })
    }

    /// The quick-switch row: the folders saved for whatever host this pane
    /// is connected to, so switching connection switches the row.
    fn render_places(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let host = self.host_key();
        let here = self.host_path();
        let places = cx.global::<AppState>().bookmarks.places(&host);
        let last = places.len().saturating_sub(1);
        let saved = here
            .as_ref()
            .is_some_and(|path| places.iter().any(|p| &p.path == path));
        let pane = cx.weak_entity();

        let chips = places.into_iter().enumerate().map(|(ix, place)| {
            let active = here.as_ref() == Some(&place.path);
            let full = place.path.display();
            // The row can hold two folders of the same name, so the path is
            // what tells them apart.
            let tip: SharedString = match ix < 9 {
                true => format!("{full}  ⌘{}", ix + 1).into(),
                false => full.into(),
            };
            let (go, id) = (place.path.clone(), place.id.clone());
            let (menu_pane, name) = (pane.clone(), place.name.clone());
            div()
                .id(("place", ix))
                .flex_shrink_0()
                .px(px(6.))
                .rounded_sm()
                .cursor_pointer()
                .when(active, |d| d.bg(accent(&theme).opacity(0.22)).text_color(theme.foreground))
                .when(!active, |d| {
                    d.text_color(theme.muted_foreground).hover(|s| s.bg(theme.list_hover))
                })
                .child(SharedString::from(place.name))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.navigate(go.clone(), None, cx);
                }))
                .context_menu(move |menu, _window, _cx| {
                    let (p1, p2, p3, p4) = (
                        menu_pane.clone(),
                        menu_pane.clone(),
                        menu_pane.clone(),
                        menu_pane.clone(),
                    );
                    let (i1, i2, i3, i4) = (id.clone(), id.clone(), id.clone(), id.clone());
                    let name = name.clone();
                    menu.min_w(px(180.))
                        .item(PopupMenuItem::new("Rename…").icon(Lucide::PenLine).on_click(
                            move |_, window, cx| {
                                let (id, name) = (i1.clone(), name.clone());
                                let _ = p1.update(cx, |this, cx| this.ask_place_name(id, name, window, cx));
                            },
                        ))
                        .item(
                            PopupMenuItem::new("Move Left")
                                .icon(Lucide::ChevronLeft)
                                .disabled(ix == 0)
                                .on_click(move |_, _, cx| {
                                    let id = i2.clone();
                                    let _ = p2.update(cx, |this, cx| this.move_place(&id, -1, cx));
                                }),
                        )
                        .item(
                            PopupMenuItem::new("Move Right")
                                .icon(Lucide::ChevronRight)
                                .disabled(ix == last)
                                .on_click(move |_, _, cx| {
                                    let id = i3.clone();
                                    let _ = p3.update(cx, |this, cx| this.move_place(&id, 1, cx));
                                }),
                        )
                        .separator()
                        .item(PopupMenuItem::new("Remove").icon(Lucide::BookmarkX).on_click(
                            move |_, _, cx| {
                                let id = i4.clone();
                                let _ = p4.update(cx, |this, cx| this.remove_place(&id, cx));
                            },
                        ))
                })
        });

        h_flex()
            .h(px(24.))
            .flex_shrink_0()
            .px_1()
            .gap(px(3.))
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(12.))
            .child(Icon::new(Lucide::Bookmark).xsmall().text_color(theme.muted_foreground))
            .child(
                h_flex()
                    .id(("places", self.side as usize))
                    .flex_1()
                    .min_w_0()
                    .gap(px(3.))
                    // More folders than fit scroll sideways; ⌘1…⌘9 and the
                    // location popup reach the ones past the edge.
                    .overflow_x_scroll()
                    .children(chips),
            )
            .child(
                Button::new("toggle-place")
                    .icon(if saved { Lucide::BookmarkMinus } else { Lucide::BookmarkPlus })
                    .ghost()
                    .xsmall()
                    .tab_stop(false)
                    .disabled(here.is_none())
                    .tooltip(if saved {
                        "Take this folder off the row (⌘D)"
                    } else {
                        "Save this folder on the row (⌘D)"
                    })
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(TogglePlace), cx)),
            )
    }

    fn render_columns(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let sort = self.tab().sort;
        let narrow = self.narrow;
        let cell = |id: &'static str, label: &'static str, key: SortKey, cx: &mut Context<Self>| {
            let arrow = if sort.key == key { if sort.desc { " ↓" } else { " ↑" } } else { "" };
            div()
                .id(id)
                .cursor_pointer()
                .truncate()
                .hover(|s| s.text_color(theme.foreground))
                .child(format!("{label}{arrow}"))
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| this.sort_by(key, cx)))
        };
        h_flex()
            .h(px(22.))
            .flex_shrink_0()
            .px_2()
            .gap_2()
            .text_size(px(11.))
            .text_color(theme.muted_foreground)
            .border_b_1()
            .border_color(theme.border)
            .child(div().w(px(15.)).flex_shrink_0())
            .child(cell("col-name", "Name", SortKey::Name, cx).flex_1().min_w_0())
            .when(narrow < 3, |el| el.child(cell("col-ext", "Ext", SortKey::Ext, cx).w(px(EXT_W)).flex_shrink_0()))
            .child(cell("col-size", "Size", SortKey::Size, cx).w(px(SIZE_W)).flex_shrink_0().text_right())
            .when(narrow < 2, |el| el.child(cell("col-date", "Modified", SortKey::Modified, cx).w(px(DATE_W)).flex_shrink_0().text_right()))
            .when(narrow < 1, |el| el.child(div().w(px(MODE_W)).flex_shrink_0().child("Permissions")))
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let tab = self.tab();
        let files: Vec<&Row> = tab.rows.iter().filter(|r| !r.parent).collect();
        let text: SharedString = if tab.marked.is_empty() {
            let dirs = files.iter().filter(|r| r.entry.is_dir()).count();
            let bytes: u64 = files.iter().filter(|r| !r.entry.is_dir()).map(|r| r.entry.size).sum();
            match tab.rows.get(tab.cursor) {
                Some(row) if !row.parent => {
                    let e = &row.entry;
                    let owner = match (&e.user, &e.group) {
                        (Some(u), Some(g)) => format!(" · {u}:{g}"),
                        _ => String::new(),
                    };
                    format!(
                        "{} · {}{owner} · {} dirs, {} files, {}",
                        row.name,
                        if e.is_dir() { "folder".to_string() } else { human_size(e.size) },
                        dirs,
                        files.len() - dirs,
                        human_size(bytes)
                    )
                    .into()
                }
                _ => format!("{} dirs, {} files, {}", dirs, files.len() - dirs, human_size(bytes)).into(),
            }
        } else {
            let marked: Vec<&&Row> = files.iter().filter(|r| tab.marked.contains(&r.entry.name)).collect();
            let bytes: u64 = marked
                .iter()
                .map(|r| if r.entry.is_dir() { tab.dir_sizes.get(&r.entry.name).copied().unwrap_or(0) } else { r.entry.size })
                .sum();
            format!("{} of {} marked · {}", marked.len(), files.len(), human_size(bytes)).into()
        };
        let free: SharedString = tab
            .space
            .map(|s| format!("{} free", human_size(s.available)))
            .unwrap_or_default()
            .into();
        h_flex()
            .h(px(22.))
            .flex_shrink_0()
            .px_2()
            .gap_2()
            .text_size(px(11.))
            .text_color(theme.muted_foreground)
            .border_t_1()
            .border_color(theme.border)
            .child(div().flex_1().min_w_0().truncate().child(text))
            .child(div().flex_shrink_0().child(free))
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let titles: Vec<SharedString> = self.tabs.iter().map(TabState::title).collect();
        TabBar::new(("tabs", self.side as usize))
            .small()
            .selected_index(self.active)
            .children(titles.into_iter().enumerate().map(|(ix, title)| {
                Tab::new().label(title).suffix(
                    Button::new(("close-tab", ix))
                        .icon(IconName::Close)
                        .ghost()
                        .xsmall()
                        .tab_stop(false)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.close_tab_at(ix, cx);
                        })),
                )
            }))
            .on_click(cx.listener(|this, ix: &usize, _, cx| this.select_tab(*ix, cx)))
    }
}

/// What marks out the active pane.
///
/// Not `theme.primary`, which is what this used to use and cannot do the
/// job: it is a near-black in the light theme and a near-white in the dark
/// one, so it comes out the same colour as ordinary text and both panes
/// looked alike. The blue and the amber below are the theme's own accents
/// and read as accents in either.
pub(crate) fn accent(theme: &Theme) -> Hsla {
    theme.blue
}

/// The background of one row.
///
/// One pane out of the two is the active one, and the cursor is what says
/// which: a solid band of colour in that pane, a whisper of grey in the
/// other.
pub(crate) fn row_bg(theme: &Theme, is_cursor: bool, focused: bool, marked: bool) -> Option<Hsla> {
    if is_cursor && focused {
        Some(accent(theme).opacity(0.42))
    } else if is_cursor {
        Some(theme.muted)
    } else if marked {
        Some(marked_fg(theme).opacity(0.14))
    } else {
        None
    }
}

/// Marked rows, which an operation acts on, in the theme's amber.
pub(crate) fn marked_fg(theme: &Theme) -> Hsla {
    theme.warning
}

/// What the location popup needs to know about the pane. Read before the
/// menu is built, because the keyboard popup builds it while the pane is
/// being updated.
#[derive(Clone)]
pub struct MenuInfo {
    remote: bool,
    /// Inside an archive, where a saved folder means nothing.
    in_archive: bool,
    /// Whether this folder is already on the row.
    saved: bool,
    /// The saved folders of the host this pane is on.
    places: Vec<Place>,
}

impl MenuInfo {
    fn of(pane: &FilePane, cx: &App) -> Self {
        let loc = &pane.tab().loc;
        let here = pane.host_path();
        let places = cx.global::<AppState>().bookmarks.places(&pane.host_key());
        MenuInfo {
            remote: !loc.is_local(),
            in_archive: loc.vfs.host().is_some(),
            saved: here.as_ref().is_some_and(|p| places.iter().any(|place| &place.path == p)),
            places,
        }
    }
}

/// The location dropdown: this host's saved folders, this Mac, bookmarks,
/// connection commands.
fn location_menu(
    menu: PopupMenu,
    pane: WeakEntity<FilePane>,
    info: MenuInfo,
    _window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let local = tl_local::local_vfs();
    let home = local.home();
    let mut menu = menu.min_w(px(240.));

    // This host's own folders first: on a server, the items below are
    // somewhere else entirely.
    if !info.places.is_empty() {
        menu = menu.label("Places");
        for (ix, place) in info.places.iter().enumerate() {
            menu = menu.menu_with_icon(
                place.name.clone(),
                Lucide::Folder,
                Box::new(GoToPlace { index: ix }),
            );
        }
    }
    menu = menu
        .menu_with_disabled(
            if info.saved { "Remove This Folder" } else { "Save This Folder" },
            Box::new(TogglePlace),
            info.in_archive,
        )
        .separator()
        .label("This Mac");
    let mut places: Vec<(SharedString, Lucide, VPath)> = vec![("Home".into(), Lucide::House, home.clone())];
    for (name, icon) in [("Desktop", Lucide::Monitor), ("Documents", Lucide::FileText), ("Downloads", Lucide::Download)] {
        let path = home.child(name.as_bytes());
        if path.to_local().is_dir() {
            places.push((name.into(), icon, path));
        }
    }
    for (name, path) in tl_local::volumes() {
        let label = if path.is_root() { "Root (/)".to_string() } else { name };
        places.push((label.into(), Lucide::HardDrive, path));
    }
    for (label, icon, path) in places {
        let pane = pane.clone();
        menu = menu.item(PopupMenuItem::new(label).icon(icon).on_click(move |_, _, cx| {
            let path = path.clone();
            let _ = pane.update(cx, |p, cx| p.open_location(Location::local(path), false, cx));
        }));
    }

    let state = cx.global::<AppState>();
    let bookmarks: Vec<(String, String, String, bool)> = state
        .bookmarks
        .all()
        .iter()
        .map(|b| (b.id.clone(), b.name.clone(), b.plugin.clone(), state.connection(&b.id).is_some()))
        .collect();
    menu = menu.separator().label("Bookmarks");
    if bookmarks.is_empty() {
        menu = menu.item(PopupMenuItem::new("No bookmarks yet").disabled(true));
    }
    for (id, name, plugin, connected) in bookmarks {
        let pane = pane.clone();
        let icon = if plugin == tl_local::PLUGIN_ID { Lucide::FolderHeart } else { Lucide::Server };
        menu = menu.item(
            PopupMenuItem::new(name)
                .icon(icon)
                .checked(connected)
                .on_click(move |_, _, cx| {
                    let id = id.clone();
                    let _ = pane.update(cx, |_, cx| cx.emit(PaneEvent::OpenBookmark(id)));
                }),
        );
    }
    let (p1, p2, p3) = (pane.clone(), pane.clone(), pane.clone());
    menu = menu
        .separator()
        .item(PopupMenuItem::new("New Bookmark…").icon(Lucide::BookmarkPlus).on_click(move |_, _, cx| {
            let _ = p1.update(cx, |_, cx| cx.emit(PaneEvent::NewBookmark));
        }))
        .menu_with_disabled(
            "New Bookmark for This Folder…",
            Box::new(BookmarkConnection),
            info.in_archive,
        )
        .item(PopupMenuItem::new("Manage Bookmarks…").icon(Lucide::Bookmark).on_click(move |_, _, cx| {
            let _ = p2.update(cx, |_, cx| cx.emit(PaneEvent::ManageBookmarks));
        }));
    if info.remote {
        menu = menu.separator().item(PopupMenuItem::new("Disconnect").icon(Lucide::Unplug).on_click(move |_, _, cx| {
            let _ = p3.update(cx, |_, cx| cx.emit(PaneEvent::Disconnect));
        }));
    }
    menu
}

impl Render for FilePane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.contains_focused(window, cx);
        let theme = cx.theme().clone();
        let tab = self.tab();
        let count = tab.rows.len();
        let error = tab.error.clone();
        let disconnected = !tab.loc.vfs.is_connected();
        let side = self.side;
        let filter: SharedString = self.tab().filter.clone().into();
        // What the filter is hiding, so the count is the reason the list
        // looks short.
        let shown = tab.rows.iter().filter(|r| !r.parent).count();
        let total = tab.raw.iter().filter(|e| self.show_hidden || !e.is_hidden()).count();
        let counts: SharedString = format!("{shown} of {total}").into();
        let drag_names: Rc<Vec<Vec<u8>>> = Rc::new(
            tab.rows
                .iter()
                .filter(|r| !r.parent && tab.marked.contains(&r.entry.name))
                .map(|r| r.entry.name.clone())
                .collect(),
        );
        let selection_count = self.selection().map(|s| s.entries.len()).unwrap_or(0);
        let is_local = tab.loc.is_local();
        // Reveal works for local archives too (it shows the archive file).
        let local_host = tab.loc.on_host().is_local();
        let on_archive = self
            .cursor_row()
            .is_some_and(|r| !r.parent && Self::is_archive(&r.entry) && tab.loc.vfs.host().is_none());
        let scroll = tab.scroll.clone();
        let this = cx.weak_entity();

        let list = div()
            .id("list-area")
            .relative()
            .flex_1()
            .min_h_0()
            .child(
                uniform_list(
                    "rows",
                    count,
                    cx.processor(move |this, range: Range<usize>, _window, cx| {
                        range
                            .map(|ix| this.render_row(ix, focused, &drag_names, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&scroll)
                .size_full(),
            )
            .vertical_scrollbar(&scroll)
            .child(
                // Measure the width to drop columns in narrow panes.
                canvas(
                    move |bounds, _window, cx| {
                        let width = f32::from(bounds.size.width);
                        let narrow = if width < 360. { 3 } else if width < 470. { 2 } else if width < 590. { 1 } else { 0 };
                        let crumb_chars = (((width - 150.) / 7.5).max(8.) as usize / 4) * 4;
                        let origin = bounds.origin;
                        let _ = this.update(cx, |this, cx| {
                            this.list_origin = origin;
                            if this.narrow != narrow || this.crumb_chars != crumb_chars {
                                this.narrow = narrow;
                                this.crumb_chars = crumb_chars;
                                cx.notify();
                            }
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .when_some(error.filter(|_| count <= 1), |el, error| {
                el.child(
                    v_flex()
                        .absolute()
                        .top(px(40.))
                        .left_0()
                        .right_0()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .child(Icon::new(IconName::TriangleAlert).text_color(theme.warning))
                        .child(div().text_sm().text_center().text_color(theme.muted_foreground).child(error))
                        .child(
                            Button::new("retry")
                                .label("Retry")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                        ),
                )
            })
            .when(disconnected, |el| {
                el.child(
                    v_flex()
                        .absolute()
                        .inset_0()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .bg(theme.background.opacity(0.85))
                        .child(Icon::new(Lucide::Unplug).text_color(theme.warning))
                        .child(div().text_sm().child("Connection closed"))
                        .child(
                            Button::new("reconnect")
                                .label("Reconnect")
                                .primary()
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let vfs = this.location().vfs.clone();
                                    if let Some(id) = cx.global::<AppState>().bookmark_of(&vfs) {
                                        cx.emit(PaneEvent::OpenBookmark(id));
                                    }
                                })),
                        ),
                )
            })
            .when(!filter.is_empty() && shown == 0, |el| {
                el.child(
                    v_flex()
                        .absolute()
                        .top(px(40.))
                        .left_0()
                        .right_0()
                        .items_center()
                        .gap_1()
                        .px_4()
                        .child(Icon::new(IconName::Search).text_color(theme.muted_foreground))
                        .child(
                            div()
                                .text_sm()
                                .text_center()
                                .text_color(theme.muted_foreground)
                                .child(format!("Nothing here matches “{filter}”")),
                        )
                        .child(div().text_xs().text_color(theme.muted_foreground).child("Esc shows everything again")),
                )
            })
            .when(!filter.is_empty(), |el| {
                el.child(
                    h_flex()
                        .absolute()
                        .bottom(px(6.))
                        .left(px(8.))
                        .gap_1()
                        .px_2()
                        .py(px(2.))
                        .rounded_md()
                        .bg(theme.popover)
                        .border_1()
                        .border_color(accent(&theme))
                        .shadow_md()
                        .text_size(px(12.))
                        .child(Icon::new(IconName::Search).xsmall().text_color(theme.muted_foreground))
                        .child(filter)
                        .child(div().text_color(theme.muted_foreground).child(counts)),
                )
            })
            .drag_over::<DraggedFiles>(move |style, drag, _, cx| {
                if drag.side != side { style.bg(cx.theme().drop_target) } else { style }
            })
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.bg(cx.theme().drop_target))
            .on_drop(cx.listener(move |_this, drag: &DraggedFiles, _window, cx| {
                if drag.side != side {
                    cx.emit(PaneEvent::Dropped(drag.clone()));
                }
            }))
            .on_drop(cx.listener(|_this, paths: &ExternalPaths, _window, cx| {
                cx.emit(PaneEvent::DroppedExternal(paths.paths().to_vec()));
            }))
            .context_menu(move |menu, _window, _cx| {
                let one = selection_count == 1;
                menu.min_w(px(220.))
                    .menu("Open", Box::new(Open))
                    .menu_with_disabled("View", Box::new(ViewFile), !one)
                    .menu_with_disabled("Edit", Box::new(EditFile), !one)
                    .menu("Open Externally", Box::new(OpenExternally))
                    .separator()
                    .menu_with_disabled("Copy to Other Pane…", Box::new(CopyFiles), selection_count == 0)
                    .menu_with_disabled("Move to Other Pane…", Box::new(MoveFiles), selection_count == 0)
                    .menu_with_disabled("Rename…", Box::new(RenameFile), !one)
                    .menu_with_disabled("Multi-Rename…", Box::new(MultiRename), selection_count == 0)
                    .separator()
                    .when(on_archive, |m| {
                        m.menu("Open Archive", Box::new(OpenArchive))
                            .menu("Decompress…", Box::new(Decompress))
                            .menu("Extract to Other Pane…", Box::new(ExtractArchive))
                    })
                    .menu_with_disabled("Compress…", Box::new(Compress), selection_count == 0)
                    .menu_with_disabled("Pack into Archive in Other Pane…", Box::new(PackFiles), selection_count == 0)
                    .separator()
                    .menu("Copy", Box::new(ClipboardCopy))
                    .menu("Cut", Box::new(ClipboardCut))
                    .menu("Paste", Box::new(ClipboardPaste))
                    .menu("Copy Path", Box::new(CopyPath))
                    .separator()
                    .menu("New Folder…", Box::new(NewFolder))
                    .menu("New File…", Box::new(NewFile))
                    .menu("Calculate Size", Box::new(CalcSize))
                    .menu("Properties…", Box::new(Properties))
                    .separator()
                    .when(local_host, |m| m.menu(crate::util::REVEAL_LABEL, Box::new(RevealInFinder)))
                    .menu("Open Terminal Here", Box::new(OpenTerminal))
                    .separator()
                    .when(is_local, |m| m.menu_with_disabled("Move to Trash", Box::new(TrashFiles), selection_count == 0))
                    .menu_with_disabled("Delete Permanently…", Box::new(DeleteFiles), selection_count == 0)
            });

        v_flex()
            .id(("pane", self.side as usize))
            .key_context(PANE)
            .track_focus(&self.focus)
            .size_full()
            .bg(theme.background)
            .on_action(cx.listener(Self::cursor_up))
            .on_action(cx.listener(Self::cursor_down))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::cursor_home))
            .on_action(cx.listener(Self::cursor_end))
            .on_action(cx.listener(Self::mark_up))
            .on_action(cx.listener(Self::mark_down))
            .on_action(cx.listener(Self::toggle_mark))
            .on_action(cx.listener(Self::mark_all))
            .on_action(cx.listener(Self::unmark_all))
            .on_action(cx.listener(Self::invert_marks))
            .on_action(cx.listener(Self::mark_by_mask))
            .on_action(cx.listener(Self::unmark_by_mask))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::open_archive_action))
            .on_action(cx.listener(Self::go_parent))
            .on_action(cx.listener(Self::filter_backspace))
            .on_action(cx.listener(Self::go_back))
            .on_action(cx.listener(Self::go_forward))
            .on_action(cx.listener(Self::go_home))
            .on_action(cx.listener(Self::go_to_path))
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::locations))
            .on_action(cx.listener(Self::toggle_place))
            .on_action(cx.listener(Self::go_to_place))
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::prev_tab))
            .on_key_down(cx.listener(Self::on_key))
            .child(self.render_header(focused, cx))
            .when(self.tabs.len() > 1, |el| el.child(self.render_tabs(cx)))
            .when(self.has_places(cx), |el| el.child(self.render_places(cx)))
            .child(self.render_columns(cx))
            .child(list)
            .child(self.render_status(cx))
            .when_some(self.popup.as_ref().map(|(m, _)| m.clone()), |el, menu| {
                el.child(deferred(anchored().position(self.list_origin).snap_to_window().child(menu)).with_priority(1))
            })
    }
}
