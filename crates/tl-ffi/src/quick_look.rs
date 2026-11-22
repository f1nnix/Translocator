//! AREA: quick look (⌘Y), the preview drawn in place of the pane opposite
//! the active one.
//!
//! Owns [`QuickLookSnapshot`], carried in `Snapshot.quick_look` while the
//! preview is on. Quick look has no input of its own: it follows the
//! cursor, and ⌘Y (`Command::ToggleQuickLook`) turns it off.
//!
//! STUB: the snapshot carries the side, the name and the revision only.
//! The next step adds the header icon (`crate::rows::IconKind` of the
//! entry), the body (`tl_app::quick_look::Body`: nothing, reading, note,
//! image bytes with their format, text with its language, error) and the
//! footer (kind, date through `crate::format::date_long`, owner, mode,
//! "shown"). Bodies can be large: fetch them with a `Core` method keyed by
//! `revision` instead of carrying them in every snapshot. The Swift side is
//! `macos/Sources/Translocator/Areas/QuickLookView.swift`.

use tl_app::quick_look::QuickLookView;

use crate::types::Side;

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct QuickLookSnapshot {
    /// Drawn in place of this pane: the one opposite the keyboard.
    pub side: Side,
    /// The file previewed; empty when there is none.
    pub name: String,
    /// Changes whenever the body changes.
    pub revision: u64,
}

pub(crate) fn snapshot(view: &QuickLookView) -> QuickLookSnapshot {
    QuickLookSnapshot {
        side: view.side.into(),
        name: view.name.clone(),
        revision: view.revision,
    }
}
