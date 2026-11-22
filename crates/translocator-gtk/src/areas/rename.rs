//! STUB: the multi-rename dialog (`DialogKindSnapshot::Rename`).
//!
//! Must become an `adw::Dialog` (header bar with Cancel / Rename) built
//! from [`RenameSnapshot`] (`tl_ffi::rename`):
//! - the mask fields (`fields`, in `RenameField` order: name, extension,
//!   find, replace, counter start, step, digits) as `adw::EntryRow`s, each
//!   edit sending `Input::Rename { dialog, input: RenameInput::Field { .. } }`
//!   (see `tl_ffi::rename::RenameInput` for the full set: regex and match
//!   case toggles, `case` from `cases`, the placeholder buttons
//!   `name_tokens` / `ext_tokens` that insert a token);
//! - the preview as a virtualized `GtkColumnView` (old name → new name,
//!   problems in the error colour) over `Core::rename_preview(dialog,
//!   start, count)`, paged like `crate::file_list::RowModel` and reset when
//!   `preview_version` moves; `error` replaces the list;
//! - `summary`, `problems_text`, `elevated_note` (error colour) and `help`
//!   under the masks; Rename enabled by `can_apply`, sending
//!   `DialogInput::Submit` with `Input::Dialog`, Cancel / Escape sending
//!   `DialogInput::Cancel`.
//!
//! Called by `crate::dialogs` as `build(&RenameSnapshot, &Sender)`, and
//! `DialogView::update(&DialogKindSnapshot::Rename { .. })` for newer
//! snapshots of the same dialog. Swift: `macos/.../Areas/RenameDialog.swift`.

use tl_ffi::rename::RenameSnapshot;

use super::Placeholder;
use crate::dialogs::{DialogView, Sender};

pub fn build(dialog: &RenameSnapshot, sender: &Sender) -> Box<dyn DialogView> {
    Box::new(Placeholder::new(
        &dialog.title,
        "Multi-rename is not available in the GTK app yet.",
        sender,
    ))
}
