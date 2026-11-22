//! The bookmark editor (`DialogKindSnapshot::BookmarkEdit`) and the
//! bookmark manager (`DialogKindSnapshot::Bookmarks`).
//!
//! The editor is generic, as in the core: the type picker (linked toggle
//! buttons, new bookmarks only), the name, then whatever fields the
//! selected plugin declares, each as the libadwaita row for its kind
//! (entry, password, switch, combo; a path with "Choose…", which asks
//! `gtk::FileDialog` and answers with `BookmarksInput::PathChosen`), the
//! advanced ones in an expander. The header has Cancel, Save and "Save &
//! Open" (the suggested action, and what Enter in a field does); `saving`
//! shows a spinner there, `error` a banner in the error colour under the
//! header. Validation, summaries and saving are the core's: every edit is
//! sent at once as `Input::Bookmarks`, and the rows are rebuilt only when
//! the fields themselves change (another type, the advanced ones shown).
//!
//! The manager lists the bookmarks as rows (icon, name, summary, connected
//! state; up, down, edit, delete; Connect / Open), with New Bookmark… and
//! Close below; a double click opens a bookmark.
//!
//! Swift: `macos/.../Areas/BookmarksDialogs.swift`; GPUI:
//! `crates/translocator/src/bookmarks.rs`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gio, glib};
use tl_ffi::bookmarks::{
    BookmarkEditSnapshot, BookmarkField, BookmarkFieldKind, BookmarkIcon, BookmarkRow, BookmarksInput,
    BookmarksManagerSnapshot,
};
use tl_ffi::input::{DialogInput, Input};
use tl_ffi::snapshot::DialogKindSnapshot;

use crate::dialogs::{DialogView, Sender};
use crate::shell;

/// Sends a bookmarks input, unless the shell is closing the dialog.
fn send(sender: &Sender, input: BookmarksInput) {
    if !sender.dropped.get() {
        shell::send(Input::Bookmarks { input });
    }
}

// ----- text the core owns -------------------------------------------------------

/// The texts sent for each field that the core has not shown back yet:
/// its echoes lag the typing, and must not overwrite what was typed since.
#[derive(Default)]
struct Echoes(RefCell<HashMap<String, Vec<String>>>);

impl Echoes {
    fn sent(&self, key: &str, text: &str) {
        self.0.borrow_mut().entry(key.to_string()).or_default().push(text.to_string());
    }

    /// Whether `core` is to replace the field's text: not when it is an
    /// echo of something sent (or what is shown already).
    fn take(&self, key: &str, core: &str, shown: &str) -> bool {
        let mut all = self.0.borrow_mut();
        let pending = all.entry(key.to_string()).or_default();
        if let Some(pos) = pending.iter().position(|t| t == core) {
            pending.drain(..=pos);
            return false;
        }
        pending.clear();
        core != shown
    }

    fn forget(&self, key: &str) {
        self.0.borrow_mut().remove(key);
    }
}

/// Sets what an empty field shows, on the `gtk::Text` inside it.
fn set_placeholder(editable: &impl IsA<gtk::Editable>, placeholder: &str) {
    let mut target = editable.clone().upcast::<gtk::Editable>();
    while let Some(inner) = target.delegate() {
        target = inner;
    }
    if let Some(text) = target.downcast_ref::<gtk::Text>() {
        text.set_placeholder_text((!placeholder.is_empty()).then_some(placeholder));
    }
}

// ----- the editor -----------------------------------------------------------------

/// What decides the rows: rebuilt when this changes, updated in place
/// otherwise.
#[derive(Clone, PartialEq)]
struct Shape {
    plugin: String,
    fields: Vec<FieldShape>,
}

/// A field's key, label, kind and choices (value, label).
type FieldShape = (String, String, BookmarkFieldKind, Vec<(String, String)>);

impl Shape {
    fn of(plugin: &str, fields: &[&BookmarkField]) -> Shape {
        Shape {
            plugin: plugin.to_string(),
            fields: fields
                .iter()
                .map(|f| {
                    (
                        f.key.clone(),
                        f.label.clone(),
                        f.kind,
                        f.choices.iter().map(|c| (c.value.clone(), c.label.clone())).collect(),
                    )
                })
                .collect(),
        }
    }
}

/// A field's row.
enum FieldRow {
    Entry(adw::EntryRow),
    Switch(adw::SwitchRow),
    Combo(adw::ComboRow, Vec<String>),
}

impl FieldRow {
    fn widget(&self) -> gtk::Widget {
        match self {
            FieldRow::Entry(row) => row.clone().upcast(),
            FieldRow::Switch(row) => row.clone().upcast(),
            FieldRow::Combo(row, _) => row.clone().upcast(),
        }
    }
}

struct Edit {
    me: Weak<Edit>,
    sender: Sender,
    dialog: adw::Dialog,
    type_group: adw::PreferencesGroup,
    types: Vec<(String, gtk::ToggleButton)>,
    description: gtk::Label,
    name: adw::EntryRow,
    /// The name and the plain fields.
    fields: adw::PreferencesGroup,
    advanced_group: adw::PreferencesGroup,
    advanced: adw::ExpanderRow,
    error: adw::Banner,
    spinner: gtk::Spinner,
    save: gtk::Button,
    save_open: gtk::Button,
    /// By field key, with whether the field is an advanced one.
    rows: RefCell<Vec<(String, bool, FieldRow)>>,
    /// The plain fields' and the advanced fields' shapes.
    shapes: RefCell<(Option<Shape>, Option<Shape>)>,
    echoes: Echoes,
    updating: Cell<bool>,
    last: RefCell<BookmarkEditSnapshot>,
}

struct EditView(Rc<Edit>);

impl DialogView for EditView {
    fn dialog(&self) -> adw::Dialog {
        self.0.dialog.clone()
    }

    fn update(&self, kind: &DialogKindSnapshot) {
        if let DialogKindSnapshot::BookmarkEdit { dialog } = kind {
            self.0.update(dialog);
        }
    }
}

pub fn build_edit(dialog: &BookmarkEditSnapshot, sender: &Sender) -> Box<dyn DialogView> {
    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Save");
    let save_open = gtk::Button::with_label("Save & Open");
    save_open.add_css_class("suggested-action");
    let spinner = gtk::Spinner::new();
    spinner.update_property(&[gtk::accessible::Property::Label("Checking the bookmark")]);
    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    header.pack_start(&cancel);
    header.pack_end(&save_open);
    header.pack_end(&save);
    header.pack_end(&spinner);

    // The type picker, for a new bookmark.
    let type_group = adw::PreferencesGroup::new();
    let linked = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    linked.add_css_class("linked");
    linked.set_halign(gtk::Align::Center);
    linked.update_property(&[gtk::accessible::Property::Label("Connection type")]);
    let mut types: Vec<(String, gtk::ToggleButton)> = Vec::new();
    for plugin in &dialog.plugins {
        let button = gtk::ToggleButton::with_label(&plugin.name);
        if let Some((_, first)) = types.first() {
            button.set_group(Some(first));
        }
        linked.append(&button);
        types.push((plugin.id.clone(), button));
    }
    let description = gtk::Label::builder()
        .wrap(true)
        .justify(gtk::Justification::Center)
        .margin_top(10)
        .build();
    description.add_css_class("dim-label");
    let picker = gtk::Box::new(gtk::Orientation::Vertical, 0);
    picker.append(&linked);
    picker.append(&description);
    type_group.add(&picker);
    type_group.set_visible(dialog.is_new && dialog.plugins.len() > 1);

    let name = adw::EntryRow::builder().title("Name").use_markup(false).build();
    name.set_text(&dialog.name);
    set_placeholder(&name, &dialog.name_placeholder);
    let fields = adw::PreferencesGroup::new();
    fields.add(&name);
    let advanced = adw::ExpanderRow::builder().use_markup(false).build();
    let advanced_group = adw::PreferencesGroup::new();
    advanced_group.add(&advanced);

    // Why the last save failed, where it is seen however far the fields
    // are scrolled.
    install_css();
    let error = adw::Banner::builder().use_markup(false).build();
    error.add_css_class("tl-error-banner");

    let page = adw::PreferencesPage::new();
    page.add(&type_group);
    page.add(&fields);
    page.add(&advanced_group);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.add_top_bar(&error);
    view.set_content(Some(&page));
    let adw_dialog = adw::Dialog::builder()
        .title(&dialog.title)
        .content_width(620)
        .content_height(640)
        .child(&view)
        .can_close(false)
        .build();
    adw_dialog.set_default_widget(Some(&save_open));

    let edit = Rc::new_cyclic(|me| Edit {
        me: me.clone(),
        sender: sender.clone(),
        dialog: adw_dialog,
        type_group,
        types,
        description,
        name,
        fields,
        advanced_group,
        advanced,
        error,
        spinner,
        save,
        save_open,
        rows: RefCell::new(Vec::new()),
        shapes: RefCell::new((None, None)),
        echoes: Echoes::default(),
        updating: Cell::new(false),
        last: RefCell::new(dialog.clone()),
    });
    edit.connect(&cancel);
    edit.apply(dialog, true);
    Box::new(EditView(edit))
}

impl Edit {
    fn id(&self) -> u64 {
        self.sender.id
    }

    fn connect(&self, cancel: &gtk::Button) {
        let s = self.sender.clone();
        cancel.connect_clicked(move |_| s.send(DialogInput::Cancel));
        let s = self.sender.clone();
        self.dialog.connect_close_attempt(move |_| s.send(DialogInput::Cancel));
        for (button, open) in [(&self.save, false), (&self.save_open, true)] {
            let weak = self.me.clone();
            button.connect_clicked(move |_| {
                if let Some(edit) = weak.upgrade() {
                    edit.save(open);
                }
            });
        }
        for (plugin, button) in &self.types {
            let (weak, plugin) = (self.me.clone(), plugin.clone());
            button.connect_toggled(move |button| {
                let Some(edit) = weak.upgrade() else { return };
                if button.is_active() && !edit.updating.get() {
                    send(
                        &edit.sender,
                        BookmarksInput::Plugin {
                            dialog: edit.id(),
                            plugin: plugin.clone(),
                        },
                    );
                }
            });
        }
        let weak = self.me.clone();
        self.name.connect_changed(move |row| {
            let Some(edit) = weak.upgrade() else { return };
            if edit.updating.get() {
                return;
            }
            let text = row.text().to_string();
            edit.echoes.sent("", &text);
            send(&edit.sender, BookmarksInput::Name { dialog: edit.id(), text });
        });
        let weak = self.me.clone();
        self.name.connect_entry_activated(move |_| {
            if let Some(edit) = weak.upgrade() {
                edit.save(true);
            }
        });
        let weak = self.me.clone();
        self.advanced.connect_expanded_notify(move |row| {
            let Some(edit) = weak.upgrade() else { return };
            if !edit.updating.get() && row.is_expanded() != edit.last.borrow().advanced {
                send(&edit.sender, BookmarksInput::ToggleAdvanced { dialog: edit.id() });
            }
        });
        // The keyboard starts where the core says, once the dialog is up.
        let weak = self.me.clone();
        self.dialog.connect_map(move |_| {
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                if let Some(edit) = weak.upgrade() {
                    let focus = edit.last.borrow().autofocus.clone();
                    edit.focus(focus.as_deref());
                }
            });
        });
    }

    fn save(&self, open: bool) {
        if self.last.borrow().saving {
            return;
        }
        send(&self.sender, BookmarksInput::Save { dialog: self.id(), open });
    }

    fn update(&self, dialog: &BookmarkEditSnapshot) {
        if *self.last.borrow() == *dialog {
            return;
        }
        let refocus = self.last.borrow().autofocus != dialog.autofocus;
        self.apply(dialog, false);
        if refocus {
            self.focus(dialog.autofocus.as_deref());
        }
    }

    fn apply(&self, dialog: &BookmarkEditSnapshot, first: bool) {
        self.last.replace(dialog.clone());
        self.updating.set(true);
        self.dialog.set_title(&dialog.title);
        for (plugin, button) in &self.types {
            if *plugin == dialog.plugin && !button.is_active() {
                button.set_active(true);
            }
        }
        self.description.set_text(&dialog.description);
        self.description.set_visible(!dialog.description.is_empty());
        if !first && self.echoes.take("", &dialog.name, &self.name.text()) {
            self.name.set_text(&dialog.name);
        }

        let plain: Vec<&BookmarkField> = dialog.fields.iter().filter(|f| !f.advanced).collect();
        let extra: Vec<&BookmarkField> = dialog.fields.iter().filter(|f| f.advanced).collect();
        let (plain_shape, extra_shape) = (Shape::of(&dialog.plugin, &plain), Shape::of(&dialog.plugin, &extra));
        let (rebuild_plain, rebuild_extra) = {
            let shapes = self.shapes.borrow();
            (
                shapes.0.as_ref() != Some(&plain_shape),
                shapes.1.as_ref() != Some(&extra_shape),
            )
        };
        if rebuild_plain || rebuild_extra {
            let mut rows = self.rows.borrow_mut();
            rows.retain(|(_, advanced, row)| {
                let rebuild = if *advanced { rebuild_extra } else { rebuild_plain };
                if rebuild {
                    if *advanced {
                        self.advanced.remove(&row.widget());
                    } else {
                        self.fields.remove(&row.widget());
                    }
                }
                !rebuild
            });
            if rebuild_plain {
                for field in &plain {
                    let row = self.field_row(field, dialog.picks_directories);
                    self.fields.add(&row.widget());
                    rows.push((field.key.clone(), false, row));
                }
            }
            if rebuild_extra {
                for field in &extra {
                    let row = self.field_row(field, dialog.picks_directories);
                    self.advanced.add_row(&row.widget());
                    rows.push((field.key.clone(), true, row));
                }
            }
            if rebuild_plain {
                // Another type: what was typed for the old one is gone.
                self.echoes.0.borrow_mut().retain(|key, _| key.is_empty());
            }
            self.shapes.replace((Some(plain_shape), Some(extra_shape)));
        } else {
            for (key, _, row) in self.rows.borrow().iter() {
                let Some(field) = dialog.fields.iter().find(|f| f.key == *key) else { continue };
                self.update_row(field, row);
            }
        }

        self.advanced_group.set_visible(dialog.has_advanced);
        self.advanced.set_title(&dialog.advanced_label);
        if self.advanced.is_expanded() != dialog.advanced {
            self.advanced.set_expanded(dialog.advanced);
        }
        if let Some(error) = &dialog.error {
            self.error.set_title(error);
            self.error
                .upcast_ref::<gtk::Widget>()
                .update_property(&[gtk::accessible::Property::Label(&format!("Error: {error}"))]);
        }
        self.error.set_revealed(dialog.error.is_some());
        self.spinner.set_visible(dialog.saving);
        self.spinner.set_spinning(dialog.saving);
        self.save.set_sensitive(!dialog.saving);
        self.save_open.set_sensitive(!dialog.saving);
        self.type_group.set_visible(dialog.is_new && dialog.plugins.len() > 1);
        self.updating.set(false);
    }

    /// Puts the keyboard in a field (by key), or in the name.
    fn focus(&self, key: Option<&str>) {
        let rows = self.rows.borrow();
        let target = key
            .and_then(|key| rows.iter().find(|(k, ..)| k == key))
            .map(|(_, _, row)| row.widget())
            .unwrap_or_else(|| self.name.clone().upcast());
        target.grab_focus();
    }

    fn field_row(&self, field: &BookmarkField, picks_directories: bool) -> FieldRow {
        let id = self.id();
        let key = field.key.clone();
        match field.kind {
            BookmarkFieldKind::Toggle => {
                let row = adw::SwitchRow::builder()
                    .title(&field.label)
                    .use_markup(false)
                    .active(field.on)
                    .build();
                if !field.help.is_empty() {
                    row.set_subtitle(&field.help);
                }
                let weak = self.me.clone();
                row.connect_active_notify(move |row| {
                    let Some(edit) = weak.upgrade() else { return };
                    if !edit.updating.get() {
                        send(
                            &edit.sender,
                            BookmarksInput::Toggle {
                                dialog: id,
                                key: key.clone(),
                                on: row.is_active(),
                            },
                        );
                    }
                });
                FieldRow::Switch(row)
            }
            BookmarkFieldKind::Choice => {
                let labels: Vec<&str> = field.choices.iter().map(|c| c.label.as_str()).collect();
                let values: Vec<String> = field.choices.iter().map(|c| c.value.clone()).collect();
                let row = adw::ComboRow::builder()
                    .title(&field.label)
                    .use_markup(false)
                    .model(&gtk::StringList::new(&labels))
                    .build();
                if !field.help.is_empty() {
                    row.set_subtitle(&field.help);
                }
                if let Some(ix) = values.iter().position(|v| *v == field.value) {
                    row.set_selected(ix as u32);
                }
                let (weak, choices) = (self.me.clone(), values.clone());
                row.connect_selected_notify(move |row| {
                    let Some(edit) = weak.upgrade() else { return };
                    let Some(value) = choices.get(row.selected() as usize).cloned() else { return };
                    if !edit.updating.get() {
                        send(
                            &edit.sender,
                            BookmarksInput::Field {
                                dialog: id,
                                key: key.clone(),
                                value,
                            },
                        );
                    }
                });
                FieldRow::Combo(row, values)
            }
            kind => {
                let row: adw::EntryRow = if kind == BookmarkFieldKind::Secret {
                    adw::PasswordEntryRow::new().upcast()
                } else {
                    adw::EntryRow::new()
                };
                row.set_title(&field.label);
                row.set_use_markup(false);
                row.set_text(&field.value);
                set_placeholder(&row, &field.placeholder);
                if kind == BookmarkFieldKind::Number {
                    row.set_input_purpose(gtk::InputPurpose::Digits);
                }
                if !field.help.is_empty() {
                    row.add_suffix(&help_button(&field.label, &field.help));
                }
                if kind == BookmarkFieldKind::LocalPath {
                    let choose = gtk::Button::with_label("Choose…");
                    choose.add_css_class("flat");
                    choose.set_valign(gtk::Align::Center);
                    choose.update_property(&[gtk::accessible::Property::Label(&format!("Choose {}", field.label))]);
                    let (weak, key, label) = (self.me.clone(), key.clone(), field.label.clone());
                    choose.connect_clicked(move |_| {
                        if let Some(edit) = weak.upgrade() {
                            edit.choose(&key, &label, picks_directories);
                        }
                    });
                    row.add_suffix(&choose);
                }
                let weak = self.me.clone();
                let sent_key = key.clone();
                row.connect_changed(move |row| {
                    let Some(edit) = weak.upgrade() else { return };
                    if edit.updating.get() {
                        return;
                    }
                    let value = row.text().to_string();
                    edit.echoes.sent(&sent_key, &value);
                    send(
                        &edit.sender,
                        BookmarksInput::Field {
                            dialog: id,
                            key: sent_key.clone(),
                            value,
                        },
                    );
                });
                let weak = self.me.clone();
                row.connect_entry_activated(move |_| {
                    if let Some(edit) = weak.upgrade() {
                        edit.save(true);
                    }
                });
                FieldRow::Entry(row)
            }
        }
    }

    /// With `updating` set.
    fn update_row(&self, field: &BookmarkField, row: &FieldRow) {
        match row {
            FieldRow::Entry(row) => {
                if self.echoes.take(&field.key, &field.value, &row.text()) {
                    row.set_text(&field.value);
                    row.set_position(-1);
                }
            }
            FieldRow::Switch(row) => {
                if row.is_active() != field.on {
                    row.set_active(field.on);
                }
            }
            FieldRow::Combo(row, values) => {
                if let Some(ix) = values.iter().position(|v| *v == field.value)
                    && row.selected() != ix as u32
                {
                    row.set_selected(ix as u32);
                }
            }
        }
    }

    /// "Choose…": the desktop's file (or folder) chooser; its answer goes
    /// to the core, which fills the field in.
    fn choose(&self, key: &str, label: &str, folders: bool) {
        let chooser = gtk::FileDialog::builder().title(label).modal(true).build();
        let current = self
            .last
            .borrow()
            .fields
            .iter()
            .find(|f| f.key == key)
            .map(|f| f.value.clone())
            .unwrap_or_default();
        if !current.is_empty() {
            let expanded = match current.strip_prefix("~/") {
                Some(rest) => glib::home_dir().join(rest),
                None if current == "~" => glib::home_dir(),
                None => std::path::PathBuf::from(&current),
            };
            let file = gio::File::for_path(&expanded);
            if folders {
                chooser.set_initial_folder(Some(&file));
            } else if expanded.is_file() {
                chooser.set_initial_file(Some(&file));
            } else if let Some(parent) = file.parent() {
                chooser.set_initial_folder(Some(&parent));
            }
        }
        let window = self.dialog.root().and_downcast::<gtk::Window>();
        let (weak, key) = (self.me.clone(), key.to_string());
        let done = move |result: Result<gio::File, glib::Error>| {
            let (Some(edit), Ok(file)) = (weak.upgrade(), result) else { return };
            let Some(path) = file.path() else { return };
            use std::os::unix::ffi::OsStrExt;
            edit.echoes.forget(&key);
            send(
                &edit.sender,
                BookmarksInput::PathChosen {
                    dialog: edit.id(),
                    key: key.clone(),
                    path: path.as_os_str().as_bytes().to_vec(),
                },
            );
        };
        if folders {
            chooser.select_folder(window.as_ref(), gio::Cancellable::NONE, done);
        } else {
            chooser.open(window.as_ref(), gio::Cancellable::NONE, done);
        }
    }
}

/// An ⓘ at the end of a field: its help, in a popover.
fn help_button(label: &str, help: &str) -> gtk::MenuButton {
    let text = gtk::Label::builder()
        .label(help)
        .wrap(true)
        .max_width_chars(40)
        .xalign(0.0)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    let popover = gtk::Popover::builder().child(&text).build();
    let button = gtk::MenuButton::builder()
        .icon_name("help-about-symbolic")
        .popover(&popover)
        .valign(gtk::Align::Center)
        .tooltip_text(help)
        .build();
    button.add_css_class("flat");
    button.update_property(&[
        gtk::accessible::Property::Label(&format!("About {label}")),
        gtk::accessible::Property::Description(help),
    ]);
    button
}

// ----- the manager ------------------------------------------------------------------

/// Which button of a row had the keyboard, to give it back after the rows
/// are rebuilt (moving a bookmark with the keyboard keeps going).
#[derive(Clone, Copy, PartialEq)]
enum Role {
    Up,
    Down,
    Edit,
    Delete,
    Open,
}

type RowButtons = Vec<(Role, gtk::Button)>;

struct Manager {
    sender: Sender,
    dialog: adw::Dialog,
    stack: gtk::Stack,
    group: adw::PreferencesGroup,
    empty: adw::StatusPage,
    /// By bookmark id, with the buttons that can have the keyboard.
    rows: RefCell<Vec<(String, adw::ActionRow, RowButtons)>>,
    last: RefCell<BookmarksManagerSnapshot>,
}

struct ManagerView(Rc<Manager>);

impl DialogView for ManagerView {
    fn dialog(&self) -> adw::Dialog {
        self.0.dialog.clone()
    }

    fn update(&self, kind: &DialogKindSnapshot) {
        if let DialogKindSnapshot::Bookmarks { dialog } = kind {
            self.0.update(dialog);
        }
    }
}

pub fn build_manager(dialog: &BookmarksManagerSnapshot, sender: &Sender) -> Box<dyn DialogView> {
    let header = adw::HeaderBar::new();
    header.set_show_end_title_buttons(false);
    header.set_show_start_title_buttons(false);

    let group = adw::PreferencesGroup::new();
    let page = adw::PreferencesPage::new();
    page.add(&group);
    let empty = adw::StatusPage::builder()
        .icon_name("user-bookmarks-symbolic")
        .title(&dialog.empty_text)
        .build();
    empty.add_css_class("compact");
    let stack = gtk::Stack::new();
    stack.add_named(&page, Some("list"));
    stack.add_named(&empty, Some("empty"));
    stack.set_vexpand(true);

    let new = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name("list-add-symbolic")
                .label("New Bookmark…")
                .use_underline(false)
                .build(),
        )
        .build();
    let close = gtk::Button::with_label("Close");
    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    bottom.set_margin_top(6);
    bottom.set_margin_bottom(6);
    bottom.set_margin_start(12);
    bottom.set_margin_end(12);
    new.set_hexpand(true);
    new.set_halign(gtk::Align::Start);
    bottom.append(&new);
    bottom.append(&close);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&stack));
    view.add_bottom_bar(&bottom);
    let adw_dialog = adw::Dialog::builder()
        .title(&dialog.title)
        .content_width(640)
        .content_height(480)
        .child(&view)
        .can_close(false)
        .build();
    adw_dialog.set_default_widget(Some(&close));

    let s = sender.clone();
    new.connect_clicked(move |_| send(&s, BookmarksInput::New { manager: s.id }));
    let s = sender.clone();
    close.connect_clicked(move |_| s.send(DialogInput::Submit));
    let s = sender.clone();
    adw_dialog.connect_close_attempt(move |_| s.send(DialogInput::Cancel));
    // The keyboard starts in the list (or on New Bookmark… without one).
    adw_dialog.connect_map(move |_| {
        let new = new.clone();
        glib::idle_add_local_once(move || {
            let _ = new.grab_focus();
        });
    });

    let manager = Rc::new(Manager {
        sender: sender.clone(),
        dialog: adw_dialog,
        stack,
        group,
        empty,
        rows: RefCell::new(Vec::new()),
        last: RefCell::new(dialog.clone()),
    });
    manager.fill(dialog);
    Box::new(ManagerView(manager))
}

impl Manager {
    fn update(&self, dialog: &BookmarksManagerSnapshot) {
        if *self.last.borrow() == *dialog {
            return;
        }
        self.last.replace(dialog.clone());
        self.dialog.set_title(&dialog.title);
        self.empty.set_title(&dialog.empty_text);
        // The button with the keyboard, given back after the rebuild.
        let focus = self.dialog.focus();
        let had = self.rows.borrow().iter().find_map(|(id, _, buttons)| {
            buttons.iter().find_map(|(role, button)| {
                focus
                    .as_ref()
                    .is_some_and(|f| f == button.upcast_ref::<gtk::Widget>() || f.is_ancestor(button))
                    .then(|| (id.clone(), *role))
            })
        });
        self.fill(dialog);
        if let Some((id, role)) = had {
            let rows = self.rows.borrow();
            let target = rows
                .iter()
                .find(|(i, ..)| *i == id)
                .and_then(|(_, _, buttons)| buttons.iter().find(|(r, b)| *r == role && b.is_sensitive()))
                .map(|(_, b)| b.clone().upcast::<gtk::Widget>())
                .or_else(|| rows.iter().find(|(i, ..)| *i == id).map(|(_, row, _)| row.clone().upcast()));
            if let Some(target) = target {
                target.grab_focus();
            }
        }
    }

    fn fill(&self, dialog: &BookmarksManagerSnapshot) {
        for (_, row, _) in self.rows.take() {
            self.group.remove(&row);
        }
        let mut rows = Vec::new();
        for bookmark in &dialog.rows {
            let (row, buttons) = self.row(bookmark);
            self.group.add(&row);
            rows.push((bookmark.id.clone(), row, buttons));
        }
        self.rows.replace(rows);
        self.stack
            .set_visible_child_name(if dialog.rows.is_empty() { "empty" } else { "list" });
    }

    fn row(&self, b: &BookmarkRow) -> (adw::ActionRow, Vec<(Role, gtk::Button)>) {
        let row = adw::ActionRow::builder().title(&b.name).use_markup(false).build();
        if !b.summary.is_empty() {
            row.set_subtitle(&b.summary);
            row.set_subtitle_lines(1);
        }
        let icon = gtk::Image::from_icon_name(match b.icon {
            BookmarkIcon::Local => "folder-symbolic",
            BookmarkIcon::Remote => "network-server-symbolic",
        });
        if b.connected {
            icon.add_css_class("success");
        } else {
            icon.add_css_class("dim-label");
        }
        row.add_prefix(&icon);
        if b.connected {
            let state = gtk::Label::new(Some("Connected"));
            state.add_css_class("success");
            state.add_css_class("caption");
            state.set_valign(gtk::Align::Center);
            row.add_suffix(&state);
            row.upcast_ref::<gtk::Widget>()
                .update_property(&[gtk::accessible::Property::Description("Connected")]);
        }
        let mut buttons = Vec::new();
        let name = &b.name;
        for (role, icon, tooltip, label, input, enabled) in [
            (
                Role::Up,
                "go-up-symbolic",
                "Move Up",
                format!("Move {name} up"),
                BookmarksInput::Move { id: b.id.clone(), delta: -1 },
                b.can_move_up,
            ),
            (
                Role::Down,
                "go-down-symbolic",
                "Move Down",
                format!("Move {name} down"),
                BookmarksInput::Move { id: b.id.clone(), delta: 1 },
                b.can_move_down,
            ),
            (
                Role::Edit,
                "document-edit-symbolic",
                "Edit",
                format!("Edit {name}"),
                BookmarksInput::Edit {
                    manager: self.sender.id,
                    id: b.id.clone(),
                },
                true,
            ),
            (
                Role::Delete,
                "user-trash-symbolic",
                "Delete",
                format!("Delete {name}"),
                BookmarksInput::Delete { id: b.id.clone() },
                true,
            ),
        ] {
            let button = gtk::Button::from_icon_name(icon);
            button.add_css_class("flat");
            button.set_valign(gtk::Align::Center);
            button.set_tooltip_text(Some(tooltip));
            button.update_property(&[gtk::accessible::Property::Label(&label)]);
            button.set_sensitive(enabled);
            let s = self.sender.clone();
            button.connect_clicked(move |_| send(&s, input.clone()));
            row.add_suffix(&button);
            buttons.push((role, button));
        }
        let open = gtk::Button::with_label(&b.open_label);
        open.set_valign(gtk::Align::Center);
        open.update_property(&[gtk::accessible::Property::Label(&format!("{} {name}", b.open_label))]);
        let open_input = BookmarksInput::Open {
            manager: self.sender.id,
            id: b.id.clone(),
        };
        let (s, input) = (self.sender.clone(), open_input.clone());
        open.connect_clicked(move |_| send(&s, input.clone()));
        row.add_suffix(&open);
        buttons.push((Role::Open, open));
        // A double click on the row opens it too.
        let click = gtk::GestureClick::new();
        let (s, input) = (self.sender.clone(), open_input);
        click.connect_pressed(move |gesture, n, _, _| {
            if n == 2 {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                send(&s, input.clone());
            }
        });
        row.add_controller(click);
        (row, buttons)
    }
}

const CSS: &str = r#"
banner.tl-error-banner > revealer > widget { background-color: alpha(@error_bg_color, 0.16); color: @error_color; }
"#;

/// The editor's style rules, once.
fn install_css() {
    thread_local! {
        static DONE: Cell<bool> = const { Cell::new(false) };
    }
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    let Some(display) = gtk::gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}
