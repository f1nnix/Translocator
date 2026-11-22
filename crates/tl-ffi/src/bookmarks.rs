//! AREA: the bookmark editor (`DialogKind::BookmarkEdit`) and the bookmark
//! manager (`DialogKind::Bookmarks`).
//!
//! Owns [`BookmarkEditSnapshot`], [`BookmarksManagerSnapshot`] and
//! [`BookmarksInput`], the full mirror of `tl_app::bookmarks::BookmarksInput`
//! (sent as `Input::Bookmarks { input }`). The bookmark list itself, which
//! the locations menu needs as well, is `Snapshot.bookmarks`
//! (`crate::snapshot::BookmarkItem`).
//!
//! The editor is generic, as in the core: it shows the fields the selected
//! plugin declares (`ViewModel::plugins`), so a new connection type needs no
//! UI code. Everything here is read from `tl_app::bookmarks::EditDialog`'s
//! own helpers. "Choose…" for path fields is an `NSOpenPanel` in Swift
//! that answers with `BookmarksInput::PathChosen`. The Swift side is
//! `macos/Sources/Translocator/Areas/BookmarksDialogs.swift`.

use tl_app::DialogId;
use tl_app::ViewModel;
use tl_app::bookmarks;
use tl_app::conn::LOCAL_PLUGIN;
use tl_app::tl_model::FieldKind;

use crate::types::{Side, vpath};

/// A connection type the editor's picker offers.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct PluginChoice {
    /// What `BookmarksInput::Plugin` names it by ("ssh").
    pub id: String,
    /// "SSH (shell commands)".
    pub name: String,
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BookmarkFieldKind {
    Text,
    /// Digits only (the core checks).
    Number,
    /// A switch: `BookmarkField.on`, sent as `BookmarksInput::Toggle`.
    Toggle,
    /// A path on this machine, with "Choose…".
    LocalPath,
    /// Typed hidden, with a button that shows it.
    Secret,
    /// One of `BookmarkField.choices`; the value is the choice's `value`.
    Choice,
}

/// One option of a `Choice` field.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FieldChoice {
    pub value: String,
    pub label: String,
}

/// A field of the selected plugin, as shown (advanced ones only while the
/// advanced options are).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct BookmarkField {
    /// What `Field`, `Toggle` and `PathChosen` name it by.
    pub key: String,
    pub label: String,
    /// The line under the field; empty for none.
    pub help: String,
    pub placeholder: String,
    pub kind: BookmarkFieldKind,
    /// The text of a non-toggle field.
    pub value: String,
    /// A toggle's state.
    pub on: bool,
    pub required: bool,
    pub advanced: bool,
    /// The options of a `Choice` field.
    pub choices: Vec<FieldChoice>,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct BookmarkEditSnapshot {
    /// "New Bookmark" / "Edit Bookmark".
    pub title: String,
    /// A new bookmark: the type picker is shown.
    pub is_new: bool,
    /// Every connection type, in the core's order.
    pub plugins: Vec<PluginChoice>,
    /// The selected one's id.
    pub plugin: String,
    /// The line under the type picker.
    pub description: String,
    pub name: String,
    /// "Shown in menus".
    pub name_placeholder: String,
    /// What follows the name, in order.
    pub fields: Vec<BookmarkField>,
    /// The plugin has advanced fields: the disclosure button is shown.
    pub has_advanced: bool,
    pub advanced: bool,
    /// "Show advanced options" / "Hide advanced options".
    pub advanced_label: String,
    /// Why the last save failed: danger text under the fields.
    pub error: Option<String>,
    /// A save waits for the plugin's answer.
    pub saving: bool,
    /// The field the keyboard starts in (a field key); `None`: the name.
    pub autofocus: Option<String>,
    /// "Choose…" picks folders rather than files.
    pub picks_directories: bool,
}

fn field_kind(kind: &FieldKind) -> BookmarkFieldKind {
    match kind {
        FieldKind::Text => BookmarkFieldKind::Text,
        FieldKind::Number => BookmarkFieldKind::Number,
        FieldKind::Toggle => BookmarkFieldKind::Toggle,
        FieldKind::LocalPath => BookmarkFieldKind::LocalPath,
        FieldKind::Secret => BookmarkFieldKind::Secret,
        FieldKind::Choice(_) => BookmarkFieldKind::Choice,
    }
}

pub(crate) fn edit_snapshot(dialog: &bookmarks::EditDialog, view: &ViewModel) -> BookmarkEditSnapshot {
    let plugins = view.plugins.as_slice();
    BookmarkEditSnapshot {
        title: dialog.title().to_string(),
        is_new: dialog.is_new,
        plugins: plugins
            .iter()
            .map(|p| PluginChoice {
                id: p.id.clone(),
                name: p.name.clone(),
            })
            .collect(),
        plugin: dialog.plugin.clone(),
        description: dialog.description(plugins).to_string(),
        name: dialog.name.clone(),
        name_placeholder: bookmarks::NAME_PLACEHOLDER.to_string(),
        fields: dialog
            .fields(plugins)
            .into_iter()
            .map(|f| BookmarkField {
                key: f.key.clone(),
                label: f.label.clone(),
                help: f.help.clone(),
                placeholder: f.placeholder.clone(),
                kind: field_kind(&f.kind),
                value: dialog.value(&f.key).to_string(),
                on: dialog.toggle(&f.key),
                required: f.required,
                advanced: f.advanced,
                choices: match &f.kind {
                    FieldKind::Choice(options) => options
                        .iter()
                        .map(|(value, label)| FieldChoice {
                            value: value.clone(),
                            label: label.clone(),
                        })
                        .collect(),
                    _ => Vec::new(),
                },
            })
            .collect(),
        has_advanced: dialog.has_advanced(plugins),
        advanced: dialog.advanced,
        advanced_label: dialog.advanced_label().to_string(),
        error: dialog.error.clone(),
        saving: dialog.saving.is_some(),
        autofocus: dialog.autofocus.clone(),
        picks_directories: dialog.picks_directories(),
    }
}

/// How a manager row's icon is drawn.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BookmarkIcon {
    /// A folder on this machine.
    Local,
    /// A server.
    Remote,
}

/// One bookmark in the manager.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct BookmarkRow {
    pub id: String,
    pub name: String,
    /// The plugin's one-line description of it ("deploy@web-1:22"), or
    /// "unknown type “x”"; empty until the plugin answered.
    pub summary: String,
    pub icon: BookmarkIcon,
    /// It has a live connection (drawn in the success colour).
    pub connected: bool,
    pub can_move_up: bool,
    pub can_move_down: bool,
    /// "Connect", or "Open" when connected.
    pub open_label: String,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct BookmarksManagerSnapshot {
    /// "Bookmarks".
    pub title: String,
    /// "Connect" opens bookmarks in this side.
    pub side: Side,
    /// In their order.
    pub rows: Vec<BookmarkRow>,
    /// What the list says without bookmarks.
    pub empty_text: String,
}

pub(crate) fn manager_snapshot(dialog: &bookmarks::ManageDialog, view: &ViewModel) -> BookmarksManagerSnapshot {
    let all = view.bookmarks.all();
    BookmarksManagerSnapshot {
        title: bookmarks::MANAGER_TITLE.to_string(),
        side: dialog.side.into(),
        rows: all
            .iter()
            .enumerate()
            .map(|(ix, b)| {
                let connected = view.connected.contains(&b.id);
                BookmarkRow {
                    id: b.id.clone(),
                    name: b.name.clone(),
                    summary: bookmarks::summary(b, &view.plugins, &view.summaries),
                    icon: if b.plugin == LOCAL_PLUGIN {
                        BookmarkIcon::Local
                    } else {
                        BookmarkIcon::Remote
                    },
                    connected,
                    can_move_up: ix > 0,
                    can_move_down: ix + 1 < all.len(),
                    open_label: bookmarks::open_label(connected).to_string(),
                }
            })
            .collect(),
        empty_text: bookmarks::NO_BOOKMARKS.to_string(),
    }
}

#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum BookmarksInput {
    // ----- the editor -----
    /// Save (`open: false`), "Save & Open" or Enter (`open: true`).
    Save { dialog: u64, open: bool },
    Name { dialog: u64, text: String },
    /// A text field of the selected plugin.
    Field { dialog: u64, key: String, value: String },
    /// A toggle of the selected plugin.
    Toggle { dialog: u64, key: String, on: bool },
    /// "Choose…" picked a local file or folder for a field (raw path bytes).
    PathChosen { dialog: u64, key: String, path: Vec<u8> },
    /// The type picker (new bookmarks only).
    Plugin { dialog: u64, plugin: String },
    /// "Show / Hide advanced options".
    ToggleAdvanced { dialog: u64 },
    // ----- the manager -----
    /// The up (-1) and down (+1) arrows.
    Move { id: String, delta: i32 },
    Edit { manager: u64, id: String },
    /// Asks first.
    Delete { id: String },
    /// "Connect" / "Open": closes the manager.
    Open { manager: u64, id: String },
    /// "New Bookmark…".
    New { manager: u64 },
}

impl From<BookmarksInput> for bookmarks::BookmarksInput {
    fn from(input: BookmarksInput) -> Self {
        use bookmarks::BookmarksInput as B;
        match input {
            BookmarksInput::Save { dialog, open } => B::Save {
                dialog: DialogId(dialog),
                open,
            },
            BookmarksInput::Name { dialog, text } => B::Name {
                dialog: DialogId(dialog),
                text,
            },
            BookmarksInput::Field { dialog, key, value } => B::Field {
                dialog: DialogId(dialog),
                key,
                value,
            },
            BookmarksInput::Toggle { dialog, key, on } => B::Toggle {
                dialog: DialogId(dialog),
                key,
                on,
            },
            BookmarksInput::PathChosen { dialog, key, path } => B::PathChosen {
                dialog: DialogId(dialog),
                key,
                path: vpath(&path),
            },
            BookmarksInput::Plugin { dialog, plugin } => B::Plugin {
                dialog: DialogId(dialog),
                plugin,
            },
            BookmarksInput::ToggleAdvanced { dialog } => B::ToggleAdvanced {
                dialog: DialogId(dialog),
            },
            BookmarksInput::Move { id, delta } => B::Move {
                id,
                delta: delta as isize,
            },
            BookmarksInput::Edit { manager, id } => B::Edit {
                manager: DialogId(manager),
                id,
            },
            BookmarksInput::Delete { id } => B::Delete { id },
            BookmarksInput::Open { manager, id } => B::Open {
                manager: DialogId(manager),
                id,
            },
            BookmarksInput::New { manager } => B::New {
                manager: DialogId(manager),
            },
        }
    }
}
