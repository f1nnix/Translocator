//! STUB: the bookmark editor (`DialogKindSnapshot::BookmarkEdit`) and the
//! bookmark manager (`DialogKindSnapshot::Bookmarks`).
//!
//! The editor must become an `adw::Dialog` with an `adw::PreferencesPage`
//! built from [`BookmarkEditSnapshot`] (`tl_ffi::bookmarks`): the type
//! picker (`plugins`, an `adw::ComboRow`, only when `is_new`), the name
//! row, then one row per `fields` entry by its kind (entry, password,
//! number, switch, choice, path with a "Choose…" `gtk::FileDialog` that
//! answers with `BookmarksInput::PathChosen`), the advanced disclosure
//! (`has_advanced` / `advanced` / `advanced_label`), `error` in the error
//! colour, `saving` as a spinner on Save; every edit is
//! `Input::Bookmarks { input: BookmarksInput::.. }`, Save and Cancel too
//! (see `BookmarksInput`). `autofocus` names the field the keyboard starts
//! in.
//!
//! The manager must become an `adw::Dialog` listing
//! [`BookmarksManagerSnapshot::rows`] as `adw::ActionRow`s (icon, name,
//! `summary`, connected state; Open/Connect `open_label`, Edit, Move Up /
//! Down by `can_move_up` / `can_move_down`, Delete), `empty_text` when
//! there are none, and New Bookmark; again all through `BookmarksInput`.
//!
//! Called by `crate::dialogs` as `build_edit(&BookmarkEditSnapshot,
//! &Sender)` and `build_manager(&BookmarksManagerSnapshot, &Sender)`, and
//! `DialogView::update(..)` for newer snapshots of the same dialog. Swift:
//! `macos/.../Areas/BookmarksDialogs.swift`.

use tl_ffi::bookmarks::{BookmarkEditSnapshot, BookmarksManagerSnapshot};

use super::Placeholder;
use crate::dialogs::{DialogView, Sender};

pub fn build_edit(dialog: &BookmarkEditSnapshot, sender: &Sender) -> Box<dyn DialogView> {
    Box::new(Placeholder::new(
        &dialog.title,
        "Editing bookmarks is not available in the GTK app yet.",
        sender,
    ))
}

pub fn build_manager(dialog: &BookmarksManagerSnapshot, sender: &Sender) -> Box<dyn DialogView> {
    let names = dialog.rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ");
    let detail = if names.is_empty() { dialog.empty_text.clone() } else { names };
    Box::new(Placeholder::new(
        &dialog.title,
        &format!("{detail}\n\nManaging bookmarks is not available in the GTK app yet."),
        sender,
    ))
}
