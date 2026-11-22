//! STUB: the synchronize-directories dialog (`DialogKindSnapshot::Sync`).
//!
//! Must become a large `adw::Dialog` built from [`SyncSnapshot`]
//! (`tl_ffi::sync`): a header with `left_display` / `right_display`, the
//! options (Subfolders `recursive`, the `directions` as a linked toggle
//! group, Show identical `show_same`), each change sent as
//! `Input::Sync { dialog, input: SyncInput::.. }`; the body by `body`
//! (comparing spinner, message, or the plan list); the plan as a
//! virtualized `GtkColumnView` over `Core::sync_rows(dialog, start, count)`
//! (action icon and tint per row, per-row direction changes through
//! `SyncInput`), reset when `rows_version` moves; `summary`, `note` and the
//! server-side note under it; Compare Again (`can_compare`), Synchronize
//! (`can_synchronize`, `DialogInput::Submit`) and Cancel (`DialogInput::Cancel`).
//!
//! Called by `crate::dialogs` as `build(&SyncSnapshot, &Sender)`, and
//! `DialogView::update(&DialogKindSnapshot::Sync { .. })` for newer
//! snapshots of the same dialog. Swift: `macos/.../Areas/SyncDialog.swift`.

use tl_ffi::sync::SyncSnapshot;

use super::Placeholder;
use crate::dialogs::{DialogView, Sender};

pub fn build(dialog: &SyncSnapshot, sender: &Sender) -> Box<dyn DialogView> {
    Box::new(Placeholder::new(
        &dialog.title,
        "Synchronizing folders is not available in the GTK app yet.",
        sender,
    ))
}
