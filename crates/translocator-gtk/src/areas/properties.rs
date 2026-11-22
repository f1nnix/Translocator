//! STUB: the properties dialog (`DialogKindSnapshot::Properties`).
//!
//! Must become an `adw::Dialog` built from [`PropertiesSnapshot`]
//! (`tl_ffi::properties`): `details` as `adw::ActionRow`s (label, value,
//! selectable), the permission grid (`columns` × `rows`, plus `specials`)
//! as check buttons enabled by `can_chmod`, each toggle sent as
//! `Input::Properties { dialog, input: PropertiesInput::.. }`; `mode_text`
//! in a monospace label; owner and group entry rows when `can_chown`; the
//! recursive check (`recursive_label`) when `any_dir`; `elevated_note` in
//! the error colour; Apply (`DialogInput::Submit`) and Cancel
//! (`DialogInput::Cancel`).
//!
//! Called by `crate::dialogs` as `build(&PropertiesSnapshot, &Sender)`, and
//! `DialogView::update(&DialogKindSnapshot::Properties { .. })` for newer
//! snapshots of the same dialog. Swift: `macos/.../Areas/PropertiesDialog.swift`.

use tl_ffi::properties::PropertiesSnapshot;

use super::Placeholder;
use crate::dialogs::{DialogView, Sender};

pub fn build(dialog: &PropertiesSnapshot, sender: &Sender) -> Box<dyn DialogView> {
    let detail = dialog
        .details
        .iter()
        .map(|d| format!("{}: {}", d.label, d.value))
        .collect::<Vec<_>>()
        .join("\n");
    Box::new(Placeholder::new(
        &dialog.title,
        &format!("{detail}\n\nEditing properties is not available in the GTK app yet."),
        sender,
    ))
}
