//! The synchronize-directories dialog (`DialogKindSnapshot::Sync`), after
//! the GPUI shell's `crates/translocator/src/sync.rs` and the Swift
//! `macos/.../Areas/SyncDialog.swift`.
//!
//! A large `adw::Dialog` with Cancel / Synchronize in its header bar,
//! built from [`SyncSnapshot`]: the two folders, the options (Subfolders,
//! the direction as a linked group of toggles, Show identical, Compare
//! Again), the body by `body` (a spinner while comparing, the error, the
//! empty text, or the plan), the plan as a `GtkColumnView` over
//! `Core::sync_rows`, fetched by page and refreshed in place when
//! `rows_version` moves (each row's action button cycles its action), and
//! the totals and the note. Every change is `Input::Sync`; Synchronize (or
//! Return) is `DialogInput::Submit`, which the core takes as
//! `SyncInput::Synchronize`; Cancel stops a comparison still running.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use tl_ffi::input::{DialogInput, Input};
use tl_ffi::snapshot::DialogKindSnapshot;
use tl_ffi::sync::{SyncActionIcon, SyncBody, SyncDirection, SyncInput, SyncRow, SyncSnapshot, SyncTint};
use tl_ffi::types::Side;

use super::common::{self, PagedModel, set_class};
use crate::dialogs::{DialogView, Sender};
use crate::shell;

fn send(sender: &Sender, input: SyncInput) {
    if !sender.dropped.get() {
        shell::send(Input::Sync {
            dialog: sender.id,
            input,
        });
    }
}

struct SyncView {
    dialog: adw::Dialog,
    left: gtk::Label,
    right: gtk::Label,
    recursive: gtk::CheckButton,
    show_same: gtk::CheckButton,
    directions: Vec<(SyncDirection, gtk::ToggleButton)>,
    compare: gtk::Button,
    body: gtk::Stack,
    spinner: gtk::Spinner,
    comparing: gtk::Label,
    error: gtk::Label,
    empty: gtk::Label,
    model: PagedModel,
    version: Cell<u64>,
    summary: gtk::Label,
    note: gtk::Label,
    synchronize: gtk::Button,
    /// Set while the view itself sets the toggles.
    updating: Rc<Cell<bool>>,
}

impl DialogView for SyncView {
    fn dialog(&self) -> adw::Dialog {
        self.dialog.clone()
    }

    fn update(&self, kind: &DialogKindSnapshot) {
        if let DialogKindSnapshot::Sync { dialog } = kind {
            self.apply(dialog);
        }
    }
}

impl SyncView {
    fn apply(&self, d: &SyncSnapshot) {
        self.dialog.set_title(&d.title);
        for (label, text) in [(&self.left, &d.left_display), (&self.right, &d.right_display)] {
            label.set_text(text);
            label.set_tooltip_text(Some(text));
        }
        self.updating.set(true);
        if self.recursive.is_active() != d.recursive {
            self.recursive.set_active(d.recursive);
        }
        if self.show_same.is_active() != d.show_same {
            self.show_same.set_active(d.show_same);
        }
        for (direction, button) in &self.directions {
            if button.is_active() != (*direction == d.direction) {
                button.set_active(*direction == d.direction);
            }
        }
        self.updating.set(false);
        self.compare.set_sensitive(d.can_compare);

        let page = match &d.body {
            SyncBody::Comparing { text } => {
                self.comparing.set_text(text);
                "comparing"
            }
            SyncBody::Error { text } => {
                self.error.set_text(text);
                "error"
            }
            SyncBody::Empty { text } => {
                self.empty.set_text(text);
                "empty"
            }
            SyncBody::List => "list",
        };
        self.body.set_visible_child_name(page);
        self.spinner.set_spinning(page == "comparing");
        let changed = self.version.replace(d.rows_version) != d.rows_version;
        self.model.update(d.row_count, changed);

        self.summary.set_text(&d.summary);
        self.note.set_text(&d.note);
        self.synchronize.set_sensitive(d.can_synchronize);
    }
}

pub fn build(d: &SyncSnapshot, sender: &Sender) -> Box<dyn DialogView> {
    common::install_css();
    let updating = Rc::new(Cell::new(false));

    // The two folders.
    let ends = gtk::Grid::builder().column_spacing(10).row_spacing(4).build();
    let end = |row: i32, title: &str| {
        let label = gtk::Label::new(Some(title));
        label.add_css_class("dim-label");
        label.set_xalign(0.0);
        let path = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .selectable(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        path.update_property(&[gtk::accessible::Property::Label(&format!("{title} folder"))]);
        ends.attach(&label, 0, row, 1, 1);
        ends.attach(&path, 1, row, 1, 1);
        path
    };
    let left = end(0, "Left");
    let right = end(1, "Right");

    // The options.
    let recursive = gtk::CheckButton::with_label("Subfolders");
    recursive.set_active(d.recursive);
    let (s, guard) = (sender.clone(), updating.clone());
    recursive.connect_toggled(move |check| {
        if !guard.get() {
            send(&s, SyncInput::Recursive { on: check.is_active() });
        }
    });
    let direction_group = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    direction_group.add_css_class("linked");
    direction_group.update_property(&[gtk::accessible::Property::Label("Direction")]);
    let mut directions = Vec::new();
    let mut first: Option<gtk::ToggleButton> = None;
    for option in &d.directions {
        let button = gtk::ToggleButton::with_label(&option.label);
        button.set_active(option.direction == d.direction);
        if let Some(first) = &first {
            button.set_group(Some(first));
        } else {
            first = Some(button.clone());
        }
        let (s, guard, direction) = (sender.clone(), updating.clone(), option.direction);
        button.connect_toggled(move |button| {
            if button.is_active() && !guard.get() {
                send(&s, SyncInput::Direction { direction });
            }
        });
        direction_group.append(&button);
        directions.push((option.direction, button));
    }
    let show_same = gtk::CheckButton::with_label("Show identical");
    show_same.set_active(d.show_same);
    let (s, guard) = (sender.clone(), updating.clone());
    show_same.connect_toggled(move |check| {
        if !guard.get() {
            send(&s, SyncInput::ShowSame { on: check.is_active() });
        }
    });
    let compare = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name("view-refresh-symbolic")
                .label("Compare Again")
                .build(),
        )
        .halign(gtk::Align::End)
        .hexpand(true)
        .build();
    let s = sender.clone();
    compare.connect_clicked(move |_| send(&s, SyncInput::CompareAgain));
    let options = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    options.append(&recursive);
    options.append(&direction_group);
    options.append(&show_same);
    options.append(&compare);

    // The body: comparing, an error, nothing to list, or the plan.
    let spinner = gtk::Spinner::new();
    spinner.set_size_request(24, 24);
    let comparing = gtk::Label::new(None);
    comparing.add_css_class("dim-label");
    let comparing_page = gtk::Box::new(gtk::Orientation::Vertical, 10);
    comparing_page.set_valign(gtk::Align::Center);
    comparing_page.set_halign(gtk::Align::Center);
    comparing_page.append(&spinner);
    comparing_page.append(&comparing);

    let error = common::note_label(&["error"]);
    error.set_selectable(true);
    error.set_valign(gtk::Align::Start);
    error.set_margin_top(10);
    error.set_margin_bottom(10);
    error.set_margin_start(12);
    error.set_margin_end(12);
    let error_page = gtk::ScrolledWindow::builder()
        .child(&error)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();

    let empty = gtk::Label::new(None);
    empty.add_css_class("dim-label");
    empty.set_wrap(true);
    empty.set_justify(gtk::Justification::Center);
    empty.set_valign(gtk::Align::Center);

    let model = PagedModel::new(d.row_count, {
        let id = sender.id;
        move |start, count| shell::core().sync_rows(id, start, count)
    });
    let view = gtk::ColumnView::new(Some(gtk::NoSelection::new(Some(model.clone()))));
    view.add_css_class("data-table");
    view.add_css_class("tl-compact-list");
    view.set_reorderable(false);
    view.update_property(&[gtk::accessible::Property::Label("Items to synchronize")]);
    let column = |title: &str, factory: gtk::SignalListItemFactory, width: Option<i32>| {
        let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
        match width {
            Some(width) => column.set_fixed_width(width),
            None => column.set_expand(true),
        }
        column.set_resizable(width.is_some() || title.is_empty());
        view.append_column(&column);
    };
    column("", action_cell(sender), Some(44));
    column("Name", name_cell(), None);
    column("State", state_cell(), Some(100));
    column("Left", side_cell(Side::Left), Some(210));
    column("Right", side_cell(Side::Right), Some(210));
    let list_page = gtk::ScrolledWindow::builder()
        .child(&view)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();

    let body = gtk::Stack::new();
    body.add_named(&comparing_page, Some("comparing"));
    body.add_named(&error_page, Some("error"));
    body.add_named(&empty, Some("empty"));
    body.add_named(&list_page, Some("list"));
    body.set_vexpand(true);
    let frame = gtk::Frame::new(None);
    frame.add_css_class("view");
    frame.add_css_class("tl-frame");
    frame.set_child(Some(&body));

    // The totals and the note.
    let summary = common::note_label(&[]);
    let note = common::note_label(&["caption", "dim-label"]);
    let footer = gtk::Box::new(gtk::Orientation::Vertical, 2);
    footer.append(&summary);
    footer.append(&note);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(6);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&ends);
    content.append(&options);
    content.append(&frame);
    content.append(&footer);

    let cancel = gtk::Button::with_label("Cancel");
    let synchronize = gtk::Button::with_label("Synchronize");
    synchronize.add_css_class("suggested-action");
    let s = sender.clone();
    cancel.connect_clicked(move |_| s.send(DialogInput::Cancel));
    let s = sender.clone();
    synchronize.connect_clicked(move |_| s.send(DialogInput::Submit));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&common::header_bar(&cancel, &synchronize));
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .title(&d.title)
        .content_width(1060)
        .content_height(680)
        .child(&toolbar)
        .can_close(false)
        .build();
    dialog.set_default_widget(Some(&synchronize));
    let s = sender.clone();
    dialog.connect_close_attempt(move |_| s.send(DialogInput::Cancel));
    common::enter_activates(&dialog, &synchronize);

    let view = SyncView {
        dialog,
        left,
        right,
        recursive,
        show_same,
        directions,
        compare,
        body,
        spinner,
        comparing,
        error,
        empty,
        model,
        version: Cell::new(d.rows_version),
        summary,
        note,
        synchronize,
        updating,
    };
    view.apply(d);
    Box::new(view)
}

/// The action button of a row: what happens to the item; a click cycles
/// it (`SyncInput::Cycle` with the item's index in the plan).
fn action_cell(sender: &Sender) -> gtk::SignalListItemFactory {
    struct Action {
        button: gtk::Button,
        plan_index: Rc<Cell<Option<u32>>>,
    }
    let sender = sender.clone();
    common::cell_factory::<SyncRow, Action>(
        move || {
            let button = gtk::Button::new();
            button.add_css_class("flat");
            button.add_css_class("tl-sync-action");
            button.set_halign(gtk::Align::Center);
            button.set_can_focus(false);
            button.set_focus_on_click(false);
            let plan_index = Rc::new(Cell::new(None));
            let (s, index) = (sender.clone(), plan_index.clone());
            button.connect_clicked(move |_| {
                if let Some(index) = index.get() {
                    send(&s, SyncInput::Cycle { index });
                }
            });
            (button.clone().upcast(), Action { button, plan_index })
        },
        |action, row| {
            let button = &action.button;
            action.plan_index.set(row.map(|r| r.plan_index));
            button.set_visible(row.is_some());
            let Some(row) = row else { return };
            match row.icon {
                SyncActionIcon::ArrowRight => button.set_icon_name("go-next-symbolic"),
                SyncActionIcon::ArrowLeft => button.set_icon_name("go-previous-symbolic"),
                SyncActionIcon::TriangleAlert => button.set_icon_name("dialog-warning-symbolic"),
                SyncActionIcon::Minus => button.set_icon_name("list-remove-symbolic"),
                // No "equal" in the symbolic icon theme.
                SyncActionIcon::Equal => button.set_label("="),
            }
            set_class(button, "accent", row.tint == SyncTint::Accent);
            set_class(button, "warning", row.tint == SyncTint::Warning);
            set_class(button, "muted", row.tint == SyncTint::Muted);
            button.set_tooltip_text(Some(&row.tooltip));
            button.update_property(&[gtk::accessible::Property::Label(&row.tooltip)]);
        },
    )
}

/// The item's icon and its path below both roots.
fn name_cell() -> gtk::SignalListItemFactory {
    common::cell_factory::<SyncRow, (gtk::Image, gtk::Label)>(
        || {
            let root = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            root.set_margin_start(4);
            root.set_margin_end(6);
            let icon = gtk::Image::new();
            let label = gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .build();
            root.append(&icon);
            root.append(&label);
            (root.upcast(), (icon, label))
        },
        |(icon, label), row| {
            let Some(row) = row else {
                icon.set_icon_name(None);
                label.set_text("");
                return;
            };
            icon.set_icon_name(Some(if row.is_dir { "folder-symbolic" } else { "text-x-generic-symbolic" }));
            set_class(icon, "accent", row.is_dir);
            set_class(icon, "dim-label", !row.is_dir);
            label.set_text(&row.rel);
            label.set_tooltip_text(Some(&row.rel));
            let name = if row.is_dir { format!("{}, folder", row.rel) } else { row.rel.clone() };
            label.update_property(&[gtk::accessible::Property::Label(&name)]);
        },
    )
}

fn state_cell() -> gtk::SignalListItemFactory {
    common::cell_factory::<SyncRow, gtk::Label>(
        || {
            let label = gtk::Label::builder().xalign(0.0).margin_start(6).margin_end(6).build();
            label.add_css_class("caption");
            label.add_css_class("dim-label");
            (label.clone().upcast(), label)
        },
        |label, row| label.set_text(row.map_or("", |r| r.state.as_str())),
    )
}

/// One side's size and date (in the success colour on the newer side).
fn side_cell(side: Side) -> gtk::SignalListItemFactory {
    common::cell_factory::<SyncRow, gtk::Label>(
        || {
            let label = gtk::Label::builder()
                .xalign(0.0)
                .margin_start(6)
                .margin_end(6)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            label.add_css_class("caption");
            label.add_css_class("numeric");
            (label.clone().upcast(), label)
        },
        move |label, row| {
            let (text, newer) = match row {
                Some(row) => {
                    let text = if side == Side::Left { &row.left } else { &row.right };
                    (text.clone(), row.newer == Some(side))
                }
                None => (None, false),
            };
            label.set_text(text.as_deref().unwrap_or(""));
            label.set_tooltip_text(text.as_deref());
            set_class(label, "success", newer);
            set_class(label, "dim-label", !newer);
            let name = if side == Side::Left { "Left" } else { "Right" };
            let description = match &text {
                Some(text) if newer => format!("{name}: {text}, newer"),
                Some(text) => format!("{name}: {text}"),
                None => format!("{name}: none"),
            };
            label.update_property(&[gtk::accessible::Property::Label(&description)]);
        },
    )
}
