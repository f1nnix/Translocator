//! AREA: the bookmark editor (`DialogKind::BookmarkEdit`) and the bookmark
//! manager (`DialogKind::Bookmarks`).
//!
//! Owns [`BookmarkEditSnapshot`], [`BookmarksManagerSnapshot`] and
//! [`BookmarksInput`], the full mirror of `tl_app::bookmarks::BookmarksInput`
//! (sent as `Input::Bookmarks { input }`). The bookmark list itself, which
//! the locations menu needs as well, is `Snapshot.bookmarks`
//! (`crate::snapshot::BookmarkItem`).
//!
//! STUB: the snapshots carry the title (and the manager's side) only. The
//! next step adds the editor's plugin picker and fields (from
//! `Snapshot`-level plugin infos: `tl_app::view::ViewModel::plugins`), the
//! advanced toggle, error and saving state, and the manager's rows with
//! summaries. "Choose…" for path fields is an `NSOpenPanel` in Swift that
//! answers with `BookmarksInput::PathChosen`. The Swift side is
//! `macos/Sources/Translocator/Areas/BookmarksDialogs.swift`.

use tl_app::bookmarks;
use tl_app::DialogId;

use crate::types::{Side, vpath};

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct BookmarkEditSnapshot {
    /// "New Bookmark" / "Edit Bookmark".
    pub title: String,
    pub is_new: bool,
}

pub(crate) fn edit_snapshot(dialog: &bookmarks::EditDialog) -> BookmarkEditSnapshot {
    BookmarkEditSnapshot {
        title: dialog.title().to_string(),
        is_new: dialog.is_new,
    }
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct BookmarksManagerSnapshot {
    /// "Bookmarks".
    pub title: String,
    /// "Connect" opens bookmarks in this side.
    pub side: Side,
}

pub(crate) fn manager_snapshot(dialog: &bookmarks::ManageDialog) -> BookmarksManagerSnapshot {
    BookmarksManagerSnapshot {
        title: bookmarks::MANAGER_TITLE.to_string(),
        side: dialog.side.into(),
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
