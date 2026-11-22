//! A pane's file list: a `GtkColumnView` over [`RowModel`], a
//! `gio::ListModel` whose items are fetched lazily, by page, from
//! `Core::rows`.
//!
//! The model is positional: item *n* is "row *n* of the pane", and its
//! object stays the same while the view holds it. When the pane's
//! `listing_version` moves, the pages are dropped and every item the view
//! still holds is refetched in place (its cells redraw on the item's
//! `changed` signal); `items-changed` is emitted only for rows added or
//! removed at the end. So the list is never rebuilt for a snapshot, and its
//! scroll position survives a refresh.
//!
//! The list keeps no state of its own: clicks, keys, drags and drops go to
//! the core, and the cursor comes back in the snapshot. GTK's selection is
//! kept on the cursor row, so accessibility tools follow it and the cursor
//! band is the row's `:selected` style.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib, graphene};
use tl_ffi::Core;
use tl_ffi::input::{Command, Dragged, Input, PaneInput};
use tl_ffi::rows::{IconKind, Row};
use tl_ffi::snapshot::PaneSnapshot;
use tl_ffi::types::{Side, SortKey, TransferMode};

use crate::shell;

/// Rows fetched at a time.
const PAGE: u32 = 64;
/// Pages kept before the cache starts again.
const MAX_PAGES: usize = 64;
/// The drag payload between panes.
pub const DRAG_MIME: &str = "application/x-translocator-files";

// ----- items ----------------------------------------------------------------

mod item_imp {
    use super::*;
    use gtk::glib::subclass::Signal;

    #[derive(Default)]
    pub struct RowItem {
        pub row: RefCell<Option<Row>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowItem {
        const NAME: &'static str = "TlRowItem";
        type Type = super::RowItem;
    }

    impl ObjectImpl for RowItem {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("changed").build()])
        }
    }
}

glib::wrapper! {
    /// Row *n* of a pane.
    pub struct RowItem(ObjectSubclass<item_imp::RowItem>);
}

impl RowItem {
    fn new(row: Row) -> RowItem {
        let item: RowItem = glib::Object::new();
        item.imp().row.replace(Some(row));
        item
    }

    pub fn row(&self) -> Option<Row> {
        self.imp().row.borrow().clone()
    }

    pub fn index(&self) -> Option<u32> {
        self.imp().row.borrow().as_ref().map(|r| r.index)
    }

    /// Replaces the row; the cells showing it redraw if it differs.
    fn set(&self, row: Row) {
        if self.imp().row.borrow().as_ref() == Some(&row) {
            return;
        }
        self.imp().row.replace(Some(row));
        self.emit_by_name::<()>("changed", &[]);
    }

    /// Redraws the cells without a change (the icon setting changed).
    fn redraw(&self) {
        self.emit_by_name::<()>("changed", &[]);
    }
}

// ----- the model ----------------------------------------------------------------

mod model_imp {
    use super::*;

    #[derive(Default)]
    pub struct RowModel {
        pub core: OnceCell<Arc<Core>>,
        pub side: Cell<Option<Side>>,
        pub count: Cell<u32>,
        /// Rows by page start.
        pub pages: RefCell<HashMap<u32, Vec<Row>>>,
        /// The items handed out, by position, while anyone holds them.
        pub live: RefCell<HashMap<u32, glib::WeakRef<RowItem>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowModel {
        const NAME: &'static str = "TlRowModel";
        type Type = super::RowModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for RowModel {}

    impl ListModelImpl for RowModel {
        fn item_type(&self) -> glib::Type {
            RowItem::static_type()
        }

        fn n_items(&self) -> u32 {
            self.count.get()
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            if position >= self.count.get() {
                return None;
            }
            if let Some(item) = self.live.borrow().get(&position).and_then(|w| w.upgrade()) {
                return Some(item.upcast());
            }
            let item = RowItem::new(self.row_at(position)?);
            let mut live = self.live.borrow_mut();
            if live.len() > 4096 {
                live.retain(|_, w| w.upgrade().is_some());
            }
            live.insert(position, item.downgrade());
            Some(item.upcast())
        }
    }

    impl RowModel {
        pub fn row_at(&self, position: u32) -> Option<Row> {
            let start = position / PAGE * PAGE;
            let mut pages = self.pages.borrow_mut();
            if !pages.contains_key(&start) {
                let (core, side) = (self.core.get()?, self.side.get()?);
                if pages.len() >= MAX_PAGES {
                    pages.clear();
                }
                pages.insert(start, core.rows(side, start, PAGE));
            }
            pages.get(&start)?.get((position - start) as usize).cloned()
        }
    }
}

glib::wrapper! {
    /// The rows of one pane, fetched by page.
    pub struct RowModel(ObjectSubclass<model_imp::RowModel>)
        @implements gio::ListModel;
}

impl RowModel {
    fn new(core: Arc<Core>, side: Side) -> RowModel {
        let model: RowModel = glib::Object::new();
        let _ = model.imp().core.set(core);
        model.imp().side.set(Some(side));
        model
    }

    /// Takes in a pane's row count; with `changed`, its rows changed too.
    fn update(&self, count: u32, changed: bool) {
        let imp = self.imp();
        let old = imp.count.replace(count);
        if changed {
            imp.pages.borrow_mut().clear();
            let live: Vec<(u32, RowItem)> = imp
                .live
                .borrow_mut()
                .drain()
                .filter(|(pos, _)| *pos < count)
                .filter_map(|(pos, weak)| weak.upgrade().map(|item| (pos, item)))
                .collect();
            for (pos, item) in &live {
                if let Some(row) = imp.row_at(*pos) {
                    item.set(row);
                }
            }
            imp.live
                .borrow_mut()
                .extend(live.into_iter().map(|(pos, item)| (pos, item.downgrade())));
        }
        if count > old {
            self.items_changed(old, 0, count - old);
        } else if count < old {
            self.items_changed(count, old - count, 0);
        }
    }

    fn redraw(&self) {
        let live: Vec<RowItem> = self.imp().live.borrow().values().filter_map(|w| w.upgrade()).collect();
        for item in live {
            item.redraw();
        }
    }
}

// ----- the view -------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Col {
    Name,
    Ext,
    Size,
    Date,
    Mode,
}

impl Col {
    const ALL: [Col; 5] = [Col::Name, Col::Ext, Col::Size, Col::Date, Col::Mode];

    fn title(self) -> &'static str {
        match self {
            Col::Name => "Name",
            Col::Ext => "Ext",
            Col::Size => "Size",
            Col::Date => "Modified",
            Col::Mode => "Permissions",
        }
    }

    fn width(self) -> i32 {
        match self {
            Col::Name => 240,
            Col::Ext => 56,
            Col::Size => 84,
            Col::Date => 136,
            Col::Mode => 100,
        }
    }

    fn sort_key(self) -> Option<SortKey> {
        match self {
            Col::Name => Some(SortKey::Name),
            Col::Ext => Some(SortKey::Ext),
            Col::Size => Some(SortKey::Size),
            Col::Date => Some(SortKey::Modified),
            Col::Mode => None,
        }
    }
}

/// The widgets of one cell, kept on its list item.
struct CellState {
    col: Col,
    side: Side,
    root: gtk::Box,
    icon: gtk::Image,
    label: gtk::Label,
    link: gtk::Label,
    item: RefCell<Option<(RowItem, glib::SignalHandlerId)>>,
    /// Whether icons are drawn (the settings' "show icons").
    icons: Rc<Cell<bool>>,
}

impl CellState {
    fn index(&self) -> Option<u32> {
        self.item.borrow().as_ref().and_then(|(item, _)| item.index())
    }

    fn render(&self) {
        let row = self.item.borrow().as_ref().and_then(|(item, _)| item.row());
        let Some(row) = row else {
            self.label.set_text("");
            return;
        };
        let root = &self.root;
        match self.col {
            Col::Name => {
                // Marks and hidden files style the whole row: the row
                // widget is the cell's parent, and its cells all show this
                // item. Not while the view is placing its rows (binding
                // happens then): after.
                let weak = root.downgrade();
                let (marked, hidden) = (row.marked, row.hidden);
                glib::idle_add_local_once(move || {
                    if let Some(row_widget) = weak.upgrade().and_then(|r| r.parent()).and_then(|cell| cell.parent()) {
                        set_class(&row_widget, "marked", marked);
                        set_class(&row_widget, "hidden-file", hidden);
                    }
                });
                self.label.set_text(&row.name);
                self.label.set_tooltip_text(None);
                let show_icon = self.icons.get();
                self.icon.set_visible(show_icon);
                if show_icon {
                    self.icon.set_from_gicon(&icon(row.icon));
                    set_class(&self.icon, "folder", row.is_dir);
                    set_class(&self.icon, "exec", !row.is_dir && row.executable);
                }
                match &row.link {
                    Some(link) => {
                        self.link.set_text(link);
                        self.link.set_visible(true);
                    }
                    None => self.link.set_visible(false),
                }
                let mut description = if row.is_parent { "Enclosing folder".to_string() } else { row.name.clone() };
                if row.is_dir && !row.is_parent {
                    description.push_str(", folder");
                }
                if row.marked {
                    description.push_str(", marked");
                }
                root.update_property(&[gtk::accessible::Property::Label(&description)]);
            }
            Col::Ext => self.label.set_text(&row.ext),
            Col::Size => {
                self.label.set_text(&row.size);
                set_class(&self.label, "tl-muted", row.is_dir || row.is_parent);
                set_class(&self.label, "tl-small", row.is_dir || row.is_parent);
            }
            Col::Date => self.label.set_text(&row.date),
            Col::Mode => self.label.set_text(&row.mode),
        }
    }
}

fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

/// The symbolic icon for a row, with fallbacks for icon themes that lack
/// the specific one.
pub fn icon(kind: IconKind) -> gio::ThemedIcon {
    let names: &[&str] = match kind {
        IconKind::Parent => &["go-up-symbolic"],
        IconKind::Folder => &["folder-symbolic"],
        IconKind::FolderSymlink => &["folder-remote-symbolic", "folder-symbolic"],
        IconKind::FileSymlink => &["emblem-symbolic-link-symbolic", "insert-link-symbolic", "text-x-generic-symbolic"],
        IconKind::Special => &["drive-harddisk-symbolic", "text-x-generic-symbolic"],
        IconKind::Archive => &["package-x-generic-symbolic", "text-x-generic-symbolic"],
        IconKind::Image => &["image-x-generic-symbolic", "text-x-generic-symbolic"],
        IconKind::Audio => &["audio-x-generic-symbolic", "text-x-generic-symbolic"],
        IconKind::Video => &["video-x-generic-symbolic", "text-x-generic-symbolic"],
        IconKind::Code => &["text-x-script-symbolic", "text-x-generic-symbolic"],
        IconKind::Config => &["emblem-system-symbolic", "text-x-generic-symbolic"],
        IconKind::Script => &["application-x-executable-symbolic", "text-x-generic-symbolic"],
        IconKind::Text => &["text-x-generic-symbolic"],
        IconKind::Key => &["dialog-password-symbolic", "channel-secure-symbolic", "text-x-generic-symbolic"],
        IconKind::File => &["text-x-generic-symbolic"],
    };
    gio::ThemedIcon::from_names(names)
}

pub struct FileList {
    pub side: Side,
    /// What the pane puts in its layout.
    pub scrolled: gtk::ScrolledWindow,
    pub view: gtk::ColumnView,
    model: RowModel,
    selection: gtk::SingleSelection,
    columns: Vec<(Col, gtk::ColumnViewColumn)>,
    /// How many columns are hidden for the width.
    narrow: Cell<i32>,
    /// Set while the list itself changes the selection or the sort.
    updating: Cell<bool>,
    icons: Rc<Cell<bool>>,
    row_height: Cell<f32>,
    /// The pane the list last showed, as far as the list is concerned.
    shown: RefCell<Option<(u64, u32, u32)>>,
}

impl FileList {
    pub fn new(core: Arc<Core>, side: Side) -> Rc<FileList> {
        let model = RowModel::new(core, side);
        let selection = gtk::SingleSelection::new(Some(model.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let view = gtk::ColumnView::new(Some(selection.clone()));
        view.add_css_class("tl-list");
        view.set_reorderable(false);
        view.set_show_column_separators(false);
        view.set_show_row_separators(false);
        view.set_enable_rubberband(false);
        view.set_single_click_activate(false);
        view.update_property(&[gtk::accessible::Property::Label(&format!("{} pane files", side_name(side)))]);

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .hexpand(true)
            .child(&view)
            .build();
        scrolled.add_css_class("tl-list-scroll");

        let icons = Rc::new(Cell::new(true));
        let mut columns = Vec::new();
        for col in Col::ALL {
            let factory = gtk::SignalListItemFactory::new();
            let column = gtk::ColumnViewColumn::new(Some(col.title()), Some(factory.clone()));
            column.set_resizable(true);
            if col == Col::Name {
                column.set_expand(true);
            } else {
                column.set_fixed_width(col.width());
            }
            if col.sort_key().is_some() {
                column.set_sorter(Some(&gtk::CustomSorter::new(|_, _| gtk::Ordering::Equal)));
            }
            setup_factory(&factory, col, side, icons.clone(), &view);
            view.append_column(&column);
            columns.push((col, column));
        }

        let list = Rc::new(FileList {
            side,
            scrolled,
            view,
            model,
            selection,
            columns,
            narrow: Cell::new(-1),
            updating: Cell::new(false),
            icons,
            row_height: Cell::new(24.0),
            shown: RefCell::new(None),
        });
        list.connect();
        list
    }

    fn connect(self: &Rc<Self>) {
        let side = self.side;
        // Header clicks sort through the core; the indicator follows the
        // snapshot.
        if let Some(sorter) = self.view.sorter() {
            let weak = Rc::downgrade(self);
            sorter.connect_changed(move |sorter, _| {
                let Some(list) = weak.upgrade() else { return };
                if list.updating.get() {
                    return;
                }
                let Some(sorter) = sorter.downcast_ref::<gtk::ColumnViewSorter>() else { return };
                let Some(column) = sorter.primary_sort_column() else { return };
                if let Some(key) = list.columns.iter().find(|(_, c)| *c == column).and_then(|(col, _)| col.sort_key()) {
                    shell::send(Input::Pane {
                        side,
                        input: PaneInput::SortBy { key },
                    });
                }
            });
        }

        // GTK's selection is the cursor's mirror: put it back if anything
        // but the list moved it.
        let weak = Rc::downgrade(self);
        self.selection.connect_selection_changed(move |_, _, _| {
            let Some(list) = weak.upgrade() else { return };
            if list.updating.get() {
                return;
            }
            let weak = Rc::downgrade(&list);
            glib::idle_add_local_once(move || {
                if let Some(list) = weak.upgrade()
                    && let Some(cursor) = list.shown.borrow().map(|(_, _, cursor)| cursor)
                {
                    list.select(cursor, false);
                }
            });
        });

        // Columns drop out as the pane narrows.
        if let Some(adj) = self.view.hadjustment() {
            let weak = Rc::downgrade(self);
            adj.connect_page_size_notify(move |adj| {
                if let Some(list) = weak.upgrade() {
                    list.fit_columns(adj.page_size());
                }
            });
        }

        // Keys: the shared key table for the pane, then the type-ahead
        // filter. Capture phase, so the view's own bindings never move its
        // focus or selection.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(move |ctrl, keyval, keycode, state| {
            let group = ctrl
                .current_event()
                .and_then(|e| e.downcast::<gdk::KeyEvent>().ok())
                .map(|e| e.layout())
                .unwrap_or(0);
            let display = ctrl.widget().map(|w| w.display()).unwrap_or_else(|| gdk::Display::default().unwrap());
            shell::pane_key(side, &display, keyval, keycode, state, group)
        });
        self.view.add_controller(keys);

        // Drops: files from the other pane, and from other applications.
        self.install_drop_target();
    }

    /// Hides Permissions, then Modified, then Ext as the list narrows.
    fn fit_columns(&self, width: f64) {
        if width <= 1.0 {
            return;
        }
        let n = if width < 360.0 {
            3
        } else if width < 470.0 {
            2
        } else if width < 590.0 {
            1
        } else {
            0
        };
        if self.narrow.replace(n) == n {
            return;
        }
        for (col, column) in &self.columns {
            let hidden = match col {
                Col::Mode => n >= 1,
                Col::Date => n >= 2,
                Col::Ext => n >= 3,
                _ => false,
            };
            column.set_visible(!hidden);
        }
    }

    /// How many rows Page Up / Page Down move by: what fits, less one.
    pub fn page_rows(&self) -> u32 {
        let height = self.view.vadjustment().map(|a| a.page_size()).unwrap_or(400.0);
        ((height as f32 / self.row_height.get().max(1.0)) as u32).saturating_sub(1).max(1)
    }

    /// Font, row height and icons (the content CSS carries the sizes).
    pub fn set_general(&self, row_height: f32, show_icons: bool) {
        self.row_height.set(row_height);
        if self.icons.replace(show_icons) != show_icons {
            self.model.redraw();
        }
    }

    pub fn set_focused(&self, focused: bool) {
        set_class(&self.view, "focused", focused);
    }

    pub fn grab_focus(&self) {
        if let Some((_, _, cursor)) = *self.shown.borrow() {
            self.select(cursor, true);
        }
        self.view.grab_focus();
    }

    pub fn has_focus(&self) -> bool {
        let root = self.view.root();
        let focus = root.and_then(|r| r.focus());
        focus.is_some_and(|f| f == *self.view.upcast_ref::<gtk::Widget>() || f.is_ancestor(&self.view))
    }

    /// Takes in a pane's snapshot.
    pub fn apply(&self, old: Option<&PaneSnapshot>, pane: &PaneSnapshot) {
        let changed = old.is_none_or(|o| o.listing_version != pane.listing_version || o.tab_id != pane.tab_id);
        let count_changed = old.is_none_or(|o| o.row_count != pane.row_count);
        self.updating.set(true);
        if changed || count_changed {
            self.model.update(pane.row_count, changed);
        }
        if old.is_none_or(|o| o.sort_key != pane.sort_key || o.sort_descending != pane.sort_descending) {
            self.show_sort(pane.sort_key, pane.sort_descending);
        }
        self.updating.set(false);
        let moved = old.is_none_or(|o| o.cursor != pane.cursor) || changed;
        self.shown.replace(Some((pane.tab_id, pane.row_count, pane.cursor)));
        if moved && pane.cursor < pane.row_count {
            self.select(pane.cursor, true);
        }
    }

    /// Puts GTK's selection (and, with `scroll`, the view) on `row`; the
    /// keyboard focus follows when the list has it.
    fn select(&self, row: u32, scroll: bool) {
        if row >= self.model.n_items() {
            return;
        }
        self.updating.set(true);
        if scroll {
            let mut flags = gtk::ListScrollFlags::SELECT;
            if self.has_focus() {
                flags |= gtk::ListScrollFlags::FOCUS;
            }
            self.view.scroll_to(row, None, flags, None);
        }
        if self.selection.selected() != row {
            self.selection.set_selected(row);
        }
        self.updating.set(false);
    }

    fn show_sort(&self, key: SortKey, descending: bool) {
        let column = self.columns.iter().find(|(col, _)| col.sort_key() == Some(key)).map(|(_, c)| c);
        let order = if descending {
            gtk::SortType::Descending
        } else {
            gtk::SortType::Ascending
        };
        self.view.sort_by_column(column, order);
    }

    fn install_drop_target(&self) {
        let side = self.side;
        let formats = gdk::ContentFormatsBuilder::new()
            .add_mime_type(DRAG_MIME)
            .add_type(gdk::FileList::static_type())
            .build();
        let target = gtk::DropTargetAsync::new(Some(formats), gdk::DragAction::COPY | gdk::DragAction::MOVE);
        let action = |target: &gtk::DropTargetAsync| {
            let state = target.current_event_state();
            if state.intersects(gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::CONTROL_MASK) {
                gdk::DragAction::MOVE
            } else {
                gdk::DragAction::COPY
            }
        };
        target.connect_drag_enter(move |target, drop, _, _| {
            if drop.formats().contain_mime_type(DRAG_MIME) && payload_side_of(drop) == Some(side) {
                return gdk::DragAction::empty();
            }
            action(target)
        });
        target.connect_drag_motion(move |target, drop, _, _| {
            if drop.formats().contain_mime_type(DRAG_MIME) && payload_side_of(drop) == Some(side) {
                return gdk::DragAction::empty();
            }
            action(target)
        });
        target.connect_drop(move |target, drop, _, _| {
            let mode = if action(target) == gdk::DragAction::MOVE {
                TransferMode::Move
            } else {
                TransferMode::Copy
            };
            let drop = drop.clone();
            glib::spawn_future_local(async move {
                let done = receive_drop(&drop, side, mode).await;
                drop.finish(if done {
                    match mode {
                        TransferMode::Move => gdk::DragAction::MOVE,
                        TransferMode::Copy => gdk::DragAction::COPY,
                    }
                } else {
                    gdk::DragAction::empty()
                });
            });
            true
        });
        self.scrolled.add_controller(target);
    }
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Left => "Left",
        Side::Right => "Right",
    }
}

/// The side a drag of ours started on, while it is in this process.
fn payload_side_of(drop: &gdk::Drop) -> Option<Side> {
    // A drag from this application carries its source; the payload says
    // the side, and the source pane is whichever started it.
    drop.drag().and_then(|_| shell::dragging_from())
}

async fn receive_drop(drop: &gdk::Drop, side: Side, mode: TransferMode) -> bool {
    if drop.formats().contain_mime_type(DRAG_MIME) {
        let Ok((stream, _)) = drop.read_future(&[DRAG_MIME], glib::Priority::DEFAULT).await else {
            return false;
        };
        let mut bytes = Vec::new();
        loop {
            match stream.read_bytes_future(64 * 1024, glib::Priority::DEFAULT).await {
                Ok(chunk) if !chunk.is_empty() => bytes.extend_from_slice(&chunk),
                Ok(_) => break,
                Err(_) => return false,
            }
        }
        let Some(drag) = decode_payload(&bytes) else { return false };
        if drag.side == side {
            return false;
        }
        shell::send(Input::Pane {
            side,
            input: PaneInput::Drop { drag, mode },
        });
        return true;
    }
    let Ok(value) = drop
        .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT)
        .await
    else {
        return false;
    };
    let Ok(files) = value.get::<gdk::FileList>() else { return false };
    let paths: Vec<String> = files
        .files()
        .iter()
        .filter_map(|f| f.path())
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if paths.is_empty() {
        return false;
    }
    shell::send(Input::DropExternal { side, paths });
    true
}

fn encode_payload(d: &Dragged) -> Vec<u8> {
    serde_json::json!({
        "side": match d.side { Side::Left => "left", Side::Right => "right" },
        "conn": d.conn,
        "dir": d.dir,
        "names": d.names,
        "elevated": d.elevated,
    })
    .to_string()
    .into_bytes()
}

fn decode_payload(bytes: &[u8]) -> Option<Dragged> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let bytes_of = |v: &serde_json::Value| -> Option<Vec<u8>> {
        v.as_array()?.iter().map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok())).collect()
    };
    Some(Dragged {
        side: match v.get("side")?.as_str()? {
            "left" => Side::Left,
            _ => Side::Right,
        },
        conn: v.get("conn")?.as_u64()?,
        dir: bytes_of(v.get("dir")?)?,
        names: v.get("names")?.as_array()?.iter().map(bytes_of).collect::<Option<_>>()?,
        elevated: v.get("elevated")?.as_bool()?,
    })
}

fn setup_factory(factory: &gtk::SignalListItemFactory, col: Col, side: Side, icons: Rc<Cell<bool>>, view: &gtk::ColumnView) {
    let view = view.downgrade();
    factory.connect_setup(move |_, obj| {
        let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else { return };
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        root.add_css_class("tl-cell");
        root.set_hexpand(true);
        let icon = gtk::Image::new();
        icon.add_css_class("tl-icon");
        let label = gtk::Label::new(None);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_xalign(0.0);
        label.set_hexpand(true);
        let link = gtk::Label::new(None);
        link.add_css_class("tl-muted");
        link.add_css_class("tl-small");
        link.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        link.set_max_width_chars(30);
        link.set_visible(false);
        match col {
            Col::Name => {
                root.append(&icon);
                root.append(&label);
                root.append(&link);
                label.set_hexpand(false);
                link.set_hexpand(true);
                link.set_xalign(0.0);
                // The name takes what it needs; the link target what is left.
                label.set_width_chars(1);
            }
            Col::Size | Col::Date => {
                label.set_xalign(1.0);
                label.add_css_class("numeric");
                if col == Col::Date {
                    label.add_css_class("tl-muted");
                }
                root.append(&label);
            }
            Col::Ext => {
                label.add_css_class("tl-muted");
                root.append(&label);
            }
            Col::Mode => {
                label.add_css_class("monospace");
                label.add_css_class("tl-muted");
                label.add_css_class("tl-small");
                root.append(&label);
            }
        }
        let state = Rc::new(CellState {
            col,
            side,
            root: root.clone(),
            icon,
            label,
            link,
            item: RefCell::new(None),
            icons: icons.clone(),
        });
        install_gestures(&state, view.clone());
        list_item.set_child(Some(&root));
        // SAFETY: the key is only ever used for `Rc<CellState>`.
        unsafe { list_item.set_data("tl-cell", state) };
    });
    factory.connect_bind(move |_, obj| {
        let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else { return };
        let Some(item) = list_item.item().and_then(|i| i.downcast::<RowItem>().ok()) else { return };
        // SAFETY: set in setup, with this type.
        let Some(state) = (unsafe { list_item.data::<Rc<CellState>>("tl-cell") }) else { return };
        let state = unsafe { state.as_ref() }.clone();
        let weak = Rc::downgrade(&state);
        let handler = item.connect_local("changed", false, move |_| {
            if let Some(state) = weak.upgrade() {
                state.render();
            }
            None
        });
        if let Some((old, id)) = state.item.replace(Some((item, handler))) {
            old.disconnect(id);
        }
        state.render();
    });
    factory.connect_unbind(move |_, obj| {
        let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else { return };
        // SAFETY: set in setup, with this type.
        let Some(state) = (unsafe { list_item.data::<Rc<CellState>>("tl-cell") }) else { return };
        let state = unsafe { state.as_ref() };
        if let Some((item, id)) = state.item.take() {
            item.disconnect(id);
        }
    });
}

/// Clicks, the context menu and dragging out, on a cell.
fn install_gestures(state: &Rc<CellState>, view: glib::WeakRef<gtk::ColumnView>) {
    let side = state.side;
    let root = &state.root;

    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_PRIMARY);
    let weak = Rc::downgrade(state);
    let view_ = view.clone();
    click.connect_pressed(move |gesture, n_press, _, _| {
        let Some(row) = weak.upgrade().and_then(|s| s.index()) else { return };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        if let Some(view) = view_.upgrade() {
            view.grab_focus();
        }
        let mods = gesture.current_event_state();
        let input = if n_press >= 2 {
            PaneInput::DoubleClick { row }
        } else {
            let toggle = mods.contains(gdk::ModifierType::CONTROL_MASK);
            PaneInput::Click {
                row,
                toggle,
                range: !toggle && mods.contains(gdk::ModifierType::SHIFT_MASK),
            }
        };
        shell::focus_pane(side);
        shell::send(Input::Pane { side, input });
    });

    let context = gtk::GestureClick::new();
    context.set_button(gdk::BUTTON_SECONDARY);
    let weak = Rc::downgrade(state);
    let view_ = view.clone();
    context.connect_pressed(move |gesture, _, x, y| {
        let Some(state) = weak.upgrade() else { return };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let Some(view) = view_.upgrade() else { return };
        view.grab_focus();
        shell::focus_pane(side);
        if let Some(row) = state.index() {
            shell::send(Input::Pane {
                side,
                input: PaneInput::ContextClick { row },
            });
        }
        let point = state
            .root
            .compute_point(&view, &graphene::Point::new(x as f32, y as f32))
            .unwrap_or_else(|| graphene::Point::new(x as f32, y as f32));
        shell::show_context_menu(side, view.upcast_ref(), point.x() as f64, point.y() as f64);
    });

    let drag = gtk::DragSource::new();
    drag.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE);
    let weak = Rc::downgrade(state);
    drag.connect_prepare(move |_, _, _| {
        let row = weak.upgrade().and_then(|s| s.index())?;
        let items = shell::core().drag(side, row)?;
        let mut providers = vec![gdk::ContentProvider::for_bytes(
            DRAG_MIME,
            &glib::Bytes::from_owned(encode_payload(&items.dragged)),
        )];
        if !items.local_paths.is_empty() {
            let files: Vec<gio::File> = items.local_paths.iter().map(gio::File::for_path).collect();
            providers.push(gdk::ContentProvider::for_value(&gdk::FileList::from_array(&files).to_value()));
        }
        shell::set_dragging_from(Some(side), items.label);
        Some(gdk::ContentProvider::new_union(&providers))
    });
    drag.connect_drag_begin(move |_, drag| {
        let label = gtk::Label::new(Some(&shell::dragging_label()));
        label.add_css_class("tl-drag-label");
        let icon = gtk::DragIcon::for_drag(drag);
        icon.set_child(Some(&label));
    });
    drag.connect_drag_end(|_, _, _| shell::set_dragging_from(None, String::new()));
    drag.group_with(&click);

    root.add_controller(click);
    root.add_controller(context);
    root.add_controller(drag);
}

/// The command for a key in the file list, with Page Up / Down filled in.
pub fn fill_rows(command: Command, rows: u32) -> Command {
    match command {
        Command::PageUp { .. } => Command::PageUp { rows },
        Command::PageDown { .. } => Command::PageDown { rows },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_drag_payload_round_trips_raw_bytes() {
        let drag = Dragged {
            side: Side::Right,
            conn: 7,
            dir: b"/srv/caf\xe9".to_vec(),
            names: vec![b"a.txt".to_vec(), vec![0xff, 0x41]],
            elevated: true,
        };
        assert_eq!(decode_payload(&encode_payload(&drag)), Some(drag));
        assert_eq!(decode_payload(b"not json"), None);
    }
}
