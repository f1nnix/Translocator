//! AREA: the synchronize-directories dialog (`DialogKind::Sync`).
//!
//! Owns [`SyncSnapshot`] (carried by `DialogKindSnapshot::Sync`) and
//! [`SyncInput`], the full mirror of `tl_app::sync::SyncInput`, sent as
//! `Input::Sync { dialog, input }`.
//!
//! STUB: the snapshot carries the title and the two folders only. The next
//! step adds the options (subfolders, direction from
//! `tl_app::sync::DIRECTIONS`, show identical), the body state
//! (`tl_app::sync::Body`), the totals and the plan rows. The rows are a
//! long list: `row_count` and `rows_version` (a `crate::versions::Tracker`
//! keyed by the `shown` `Arc`'s address) in the snapshot, and a range fetch
//! on `Core` (`sync_rows(dialog, start, count)`) built with
//! `SyncDialog::row`. The Swift side is
//! `macos/Sources/Translocator/Areas/SyncDialog.swift`.

use tl_app::sync;
use tl_app::tl_model::sync::SyncDirection as CoreDirection;

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SyncSnapshot {
    /// "Synchronize Directories".
    pub title: String,
    pub left_display: String,
    pub right_display: String,
}

pub(crate) fn snapshot(dialog: &sync::SyncDialog) -> SyncSnapshot {
    SyncSnapshot {
        title: sync::TITLE.to_string(),
        left_display: dialog.left_display.clone(),
        right_display: dialog.right_display.clone(),
    }
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyncDirection {
    Both,
    LeftToRight,
    RightToLeft,
}

impl From<SyncDirection> for CoreDirection {
    fn from(d: SyncDirection) -> Self {
        match d {
            SyncDirection::Both => CoreDirection::Both,
            SyncDirection::LeftToRight => CoreDirection::LeftToRight,
            SyncDirection::RightToLeft => CoreDirection::RightToLeft,
        }
    }
}

#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum SyncInput {
    /// The "Subfolders" box: compares again.
    Recursive { on: bool },
    /// A direction button: resets every item's action, without comparing.
    Direction { direction: SyncDirection },
    /// "Show identical".
    ShowSame { on: bool },
    CompareAgain,
    /// An item's action button, by its index in the plan: cycles to the
    /// next possible action.
    Cycle { index: u32 },
    /// The Synchronize button, or Enter.
    Synchronize,
}

impl From<SyncInput> for sync::SyncInput {
    fn from(input: SyncInput) -> Self {
        use sync::SyncInput as S;
        match input {
            SyncInput::Recursive { on } => S::Recursive(on),
            SyncInput::Direction { direction } => S::Direction(direction.into()),
            SyncInput::ShowSame { on } => S::ShowSame(on),
            SyncInput::CompareAgain => S::CompareAgain,
            SyncInput::Cycle { index } => S::Cycle(index as usize),
            SyncInput::Synchronize => S::Synchronize,
        }
    }
}
