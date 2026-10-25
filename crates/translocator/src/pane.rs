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
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{prelude::*, *};
use tl_core::archive::ArchiveFormat;
use tl_core::util::{human_size, wildcard_match};
use tl_core::{Entry, EntryKind, OpCtx, SpaceInfo, VPath, Vfs, VfsError};

use crate::actions::*;
use crate::app_state::AppState;
use crate::dialogs;
use crate::util::{blocking, entry_icon, format_date, natural_cmp};

pub const ROW_H: f32 = 22.;
const FONT: f32 = 13.;
const EXT_W: f32 = 52.;
const SIZE_W: f32 = 74.;
const DATE_W: f32 = 116.;
const MODE_W: f32 = 84.;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Ext,
    Size,
    Modified,
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
            .text_size(px(FONT))
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
    fn new(loc: Location, elevated: bool) -> Self {
        TabState {
            loc,
            rows: Vec::new(),
            raw: Vec::new(),
            cursor: 0,
            anchor: 0,
            marked: HashSet::new(),
            sort: Sort {
                key: SortKey::Name,
                desc: false,
            },
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
    quick: String,
    show_hidden: bool,
    narrow: u8,
    /// How many characters of path fit in the header.
    crumb_chars: usize,
    /// Top-left of the file list, where the keyboard locations menu opens.
    list_origin: Point<Pixels>,
    popup: Option<(Entity<PopupMenu>, Subscription)>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PaneEvent> for FilePane {}

impl Focusable for FilePane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn build_rows(raw: &[Entry], sort: Sort, show_hidden: bool, at_root: bool) -> Vec<Row> {
    let mut rows: Vec<Row> = raw
        .iter()
        .filter(|e| show_hidden || !e.is_hidden())
        .map(|e| Row {
            entry: e.clone(),
            name: e.name_lossy().into(),
            parent: false,
        })
        .collect();
    rows.sort_by(|a, b| {
        let (ad, bd) = (a.entry.is_dir(), b.entry.is_dir());
        if ad != bd {
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
        ];
        let show_hidden = cx.global::<AppState>().settings.show_hidden;
        let mut pane = FilePane {
            side,
            focus,
            tabs: vec![TabState::new(loc, elevated)],
            active: 0,
            quick: String::new(),
            show_hidden,
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

    pub fn set_show_hidden(&mut self, show: bool, cx: &mut Context<Self>) {
        self.show_hidden = show;
        for ix in 0..self.tabs.len() {
            self.rebuild_rows(ix, None);
        }
        cx.notify();
    }

    // ----- loading --------------------------------------------------------

    fn rebuild_rows(&mut self, ix: usize, select: Option<&[u8]>) {
        let show_hidden = self.show_hidden;
        let tab = &mut self.tabs[ix];
        let keep = select
            .map(<[u8]>::to_vec)
            .or_else(|| tab.rows.get(tab.cursor).map(|r| r.entry.name.clone()));
        tab.rows = build_rows(&tab.raw, tab.sort, show_hidden, tab.loc.path.is_root());
        let names: HashSet<&[u8]> = tab.rows.iter().map(|r| r.entry.name.as_slice()).collect();
        tab.marked.retain(|n| names.contains(n.as_slice()));
        tab.cursor = keep
            .and_then(|name| tab.rows.iter().position(|r| r.entry.name == name))
            .unwrap_or_else(|| tab.cursor.min(tab.rows.len().saturating_sub(1)));
        tab.scroll.scroll_to_item(tab.cursor, ScrollStrategy::Nearest);
    }

    /// (Re)loads tab `ix`, putting the cursor on `select` if given.
    fn load(&mut self, ix: usize, select: Option<Vec<u8>>, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[ix];
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
                        this.rebuild_rows(ix, select.as_deref());
                        this.watch(ix, cx);
                    }
                    Err(err) => {
                        tab.error = Some(err.to_string().into());
                        tab.raw.clear();
                        tab.rows = build_rows(&[], tab.sort, true, tab.loc.path.is_root());
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
        self.quick.clear();
        let tab = self.tab_mut();
        if tab.loc.path != path {
            let old = tab.loc.clone();
            tab.loc.path = path;
            tab.back.push(old);
            tab.forward.clear();
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
        self.quick.clear();
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
        self.quick.clear();
        let tab = self.tab_mut();
        let same_fs = Arc::ptr_eq(&tab.loc.vfs, &loc.vfs);
        if same_fs {
            self.navigate(loc.path, None, cx);
            return;
        }
        *tab = TabState::new(loc, elevated);
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
                self.tabs[ix] = TabState::new(Location::local(home), false);
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
        self.quick.clear();
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
        self.quick.clear();
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
        if !self.quick.is_empty() {
            self.quick.pop();
            cx.notify();
            return;
        }
        self.go_up(cx);
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
        let remote = !self.location().is_local();
        let menu = PopupMenu::build(window, cx, move |menu, window, cx| location_menu(menu, pane.clone(), remote, window, cx));
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

    fn refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        self.reload(cx);
    }

    // ----- tabs -----------------------------------------------------------

    fn new_tab(&mut self, _: &NewTab, _: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab();
        let loc = tab.loc.clone();
        let elevated = tab.elevated;
        self.tabs.insert(self.active + 1, TabState::new(loc, elevated));
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
            self.quick.clear();
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

    // ----- type-ahead -----------------------------------------------------

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let mods = ks.modifiers;
        if ks.key == "escape" {
            if !self.quick.is_empty() {
                self.quick.clear();
                cx.stop_propagation();
                cx.notify();
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
        if self.quick.is_empty() {
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
                "*" => {
                    self.invert(cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        let query = format!("{}{}", self.quick, ch).to_lowercase();
        let tab = self.tab();
        let found = tab
            .rows
            .iter()
            .position(|r| !r.parent && r.name.to_lowercase().starts_with(&query));
        if let Some(ix) = found {
            self.quick.push_str(ch);
            let tab = self.tab_mut();
            tab.cursor = ix;
            tab.anchor = ix;
            tab.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
        cx.stop_propagation();
        cx.notify();
    }

    // ----- mouse ----------------------------------------------------------

    fn click_row(&mut self, ix: usize, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        self.quick.clear();
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
        let fg = if marked { theme.primary } else { theme.foreground };
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
        let date: SharedString = if row.parent { SharedString::default() } else { format_date(entry.mtime).into() };
        let mode: SharedString = if row.parent { SharedString::default() } else { entry.mode_string().into() };
        let link: Option<SharedString> = entry
            .link_target
            .as_ref()
            .map(|t| format!("→ {}", String::from_utf8_lossy(t)).into());

        let bg = if is_cursor && focused {
            Some(theme.list_active)
        } else if is_cursor {
            Some(theme.muted)
        } else if marked {
            Some(theme.primary.opacity(0.08))
        } else {
            None
        };
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
            .h(px(ROW_H))
            .w_full()
            .px_2()
            .gap_2()
            .text_size(px(FONT))
            .text_color(fg)
            .when_some(bg, |el, bg| el.bg(bg))
            .when(is_cursor && focused, |el| el.border_l_2().border_color(theme.primary))
            .when(hidden, |el| el.opacity(0.6))
            .child(Icon::new(icon).size(px(15.)).flex_shrink_0().text_color(icon_color))
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
            .border_color(if focused { theme.primary.opacity(0.6) } else { theme.border })
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
                    .dropdown_menu(move |menu, window, cx| location_menu(menu, pane.clone(), !is_local, window, cx)),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_size(px(FONT))
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

/// The location dropdown: this Mac, bookmarks, connection commands.
/// `remote` is passed in rather than read from the pane: the keyboard
/// popup builds this menu while the pane is being updated.
fn location_menu(
    menu: PopupMenu,
    pane: WeakEntity<FilePane>,
    remote: bool,
    _window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let local = tl_local::local_vfs();
    let home = local.home();
    let mut menu = menu.min_w(px(240.)).label("This Mac");
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
        .item(PopupMenuItem::new("Manage Bookmarks…").icon(Lucide::Bookmark).on_click(move |_, _, cx| {
            let _ = p2.update(cx, |_, cx| cx.emit(PaneEvent::ManageBookmarks));
        }));
    if remote {
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
        let quick = self.quick.clone();
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
            .when(!quick.is_empty(), |el| {
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
                        .border_color(theme.primary)
                        .shadow_md()
                        .text_size(px(12.))
                        .child(Icon::new(IconName::Search).xsmall().text_color(theme.muted_foreground))
                        .child(quick),
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
                            .menu("Extract to Other Pane…", Box::new(ExtractArchive))
                    })
                    .menu_with_disabled("Pack into Archive…", Box::new(PackFiles), selection_count == 0)
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
                    .when(local_host, |m| m.menu("Reveal in Finder", Box::new(RevealInFinder)))
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
            .on_action(cx.listener(Self::go_back))
            .on_action(cx.listener(Self::go_forward))
            .on_action(cx.listener(Self::go_home))
            .on_action(cx.listener(Self::go_to_path))
            .on_action(cx.listener(Self::refresh))
            .on_action(cx.listener(Self::locations))
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::prev_tab))
            .on_key_down(cx.listener(Self::on_key))
            .child(self.render_header(focused, cx))
            .when(self.tabs.len() > 1, |el| el.child(self.render_tabs(cx)))
            .child(self.render_columns(cx))
            .child(list)
            .child(self.render_status(cx))
            .when_some(self.popup.as_ref().map(|(m, _)| m.clone()), |el, menu| {
                el.child(deferred(anchored().position(self.list_origin).snap_to_window().child(menu)).with_priority(1))
            })
    }
}
