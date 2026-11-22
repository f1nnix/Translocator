//! AREA: the synchronize-directories dialog (`DialogKind::Sync`).
//!
//! Owns [`SyncSnapshot`] (carried by `DialogKindSnapshot::Sync`) and
//! [`SyncInput`], the full mirror of `tl_app::sync::SyncInput`, sent as
//! `Input::Sync { dialog, input }`.
//!
//! The snapshot carries the options, the body state, the totals and the
//! core's notes. The plan rows are a long list: `row_count` and
//! `rows_version` (a [`Tracker`] keyed by the `shown` and plan `Arc`s'
//! addresses and the date format) in the snapshot, the rows by range from
//! [`Core::sync_rows`], built with `SyncDialog::row`. Only the dates are
//! written here, in local time as the settings say. The Swift side is
//! `macos/Sources/Translocator/Areas/SyncDialog.swift`.

use std::collections::HashMap;

use tl_app::dialog::{Dialog, DialogKind};
use tl_app::settings::DateFormat;
use tl_app::sync;
use tl_app::tl_model::sync::{SyncDirection as CoreDirection, SyncSide};

use crate::Core;
use crate::types::{Side, usize32};
use crate::versions::{Tracker, addr};

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SyncSnapshot {
    /// "Synchronize Directories".
    pub title: String,
    /// After "Left" / "Right" in the header.
    pub left_display: String,
    pub right_display: String,
    /// "Subfolders".
    pub recursive: bool,
    pub direction: SyncDirection,
    /// The direction buttons, in order.
    pub directions: Vec<DirectionOption>,
    /// "Show identical".
    pub show_same: bool,
    pub body: SyncBody,
    /// The plan's totals.
    pub to_right: u32,
    pub to_left: u32,
    pub conflicts: u32,
    pub same: u32,
    /// "→ 2 (1.5 KB) · ← 1 (20 B) · 1 to decide · 4 identical".
    pub summary: String,
    /// The muted line under the summary.
    pub note: String,
    /// Both folders are on one server, so copies run there.
    pub server_side: bool,
    /// The Synchronize button (and Return) can run.
    pub can_synchronize: bool,
    /// "Compare Again" is enabled.
    pub can_compare: bool,
    /// Rows listed (when `body` is `List`).
    pub row_count: u32,
    /// Moves exactly when the rows (the plan, the filter or the date
    /// format) changed: drop cached rows.
    pub rows_version: u64,
}

/// A direction button.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct DirectionOption {
    pub direction: SyncDirection,
    /// "Both ways", "Left → right", "Right → left".
    pub label: String,
}

/// What the body of the dialog shows.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq, Hash)]
pub enum SyncBody {
    /// A spinner and this text.
    Comparing { text: String },
    /// The comparison failed (danger text).
    Error { text: String },
    /// Nothing to list: this text, centred.
    Empty { text: String },
    /// The rows (`row_count` of them, from `Core::sync_rows`).
    List,
}

/// The icon of an item's action button.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyncActionIcon {
    ArrowRight,
    ArrowLeft,
    Equal,
    TriangleAlert,
    Minus,
}

/// The colour of an action icon.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyncTint {
    /// The accent colour: something is copied.
    Accent,
    Muted,
    /// A conflict to decide.
    Warning,
}

/// One listed item of the plan.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SyncRow {
    /// The row's position in the list.
    pub index: u32,
    /// The item's index in the plan: what `SyncInput::Cycle` takes.
    pub plan_index: u32,
    pub icon: SyncActionIcon,
    /// "Copy to the right (click to change)".
    pub tooltip: String,
    pub tint: SyncTint,
    pub is_dir: bool,
    /// The path below both roots.
    pub rel: String,
    /// "only left", "right newer", "same"…
    pub state: String,
    /// "1.5 KB · 2026-10-15 14:32", "folder · 3 files, 2 KB"; `None` when
    /// the item is not on that side.
    pub left: Option<String>,
    pub right: Option<String>,
    /// The side that is newer (drawn in the success colour).
    pub newer: Option<Side>,
}

type RowsKey = (usize, usize, DateFormat);

/// The rows counters, one per open sync dialog; kept by `Core` between
/// snapshots.
#[derive(Default)]
pub(crate) struct Lists(HashMap<u64, Tracker<RowsKey>>);

impl Lists {
    /// The snapshot of dialog `id`, with its rows' version.
    pub(crate) fn snapshot(
        &mut self,
        id: u64,
        dialog: &sync::SyncDialog,
        dates: DateFormat,
    ) -> SyncSnapshot {
        let plan = dialog.plan.as_ref().map_or(0, addr);
        let version = self
            .0
            .entry(id)
            .or_default()
            .version((addr(&dialog.shown), plan, dates));
        snapshot(dialog, version)
    }

    /// Forgets the dialogs that are gone.
    pub(crate) fn retain(&mut self, dialogs: &[Dialog]) {
        self.0.retain(|id, _| dialogs.iter().any(|d| d.id.0 == *id));
    }
}

pub(crate) fn snapshot(dialog: &sync::SyncDialog, rows_version: u64) -> SyncSnapshot {
    let t = &dialog.totals;
    let body = match dialog.body() {
        sync::Body::Comparing(text) => SyncBody::Comparing { text: text.into() },
        sync::Body::Error(text) => SyncBody::Error { text },
        sync::Body::Empty(text) => SyncBody::Empty { text: text.into() },
        sync::Body::List => SyncBody::List,
    };
    SyncSnapshot {
        title: sync::TITLE.to_string(),
        left_display: dialog.left_display.clone(),
        right_display: dialog.right_display.clone(),
        recursive: dialog.options.recursive,
        direction: dialog.options.direction.into(),
        directions: sync::DIRECTIONS
            .iter()
            .map(|(direction, label)| DirectionOption {
                direction: (*direction).into(),
                label: (*label).to_string(),
            })
            .collect(),
        show_same: dialog.show_same,
        row_count: if body == SyncBody::List {
            usize32(dialog.shown.len())
        } else {
            0
        },
        body,
        to_right: usize32(t.to_right),
        to_left: usize32(t.to_left),
        conflicts: usize32(t.conflicts),
        same: usize32(t.same),
        summary: dialog.summary(),
        note: dialog.note().to_string(),
        server_side: dialog.server_side,
        can_synchronize: dialog.can_synchronize(),
        can_compare: dialog.can_compare(),
        rows_version,
    }
}

/// Rows `start..start + count` of `dialog` (fewer at the end).
pub(crate) fn rows(
    dialog: &sync::SyncDialog,
    dates: DateFormat,
    start: u32,
    count: u32,
) -> Vec<SyncRow> {
    let len = dialog.shown.len();
    let start = (start as usize).min(len);
    let end = start.saturating_add(count as usize).min(len);
    (start..end)
        .filter_map(|row| {
            let (ix, item) = dialog.row(row)?;
            let (icon, tooltip) = sync::action_button(item);
            let (icon, tint) = match icon {
                sync::ActionIcon::ArrowRight => (SyncActionIcon::ArrowRight, SyncTint::Accent),
                sync::ActionIcon::ArrowLeft => (SyncActionIcon::ArrowLeft, SyncTint::Accent),
                sync::ActionIcon::Equal => (SyncActionIcon::Equal, SyncTint::Muted),
                sync::ActionIcon::TriangleAlert => {
                    (SyncActionIcon::TriangleAlert, SyncTint::Warning)
                }
                sync::ActionIcon::Minus => (SyncActionIcon::Minus, SyncTint::Muted),
            };
            let newer = if sync::is_newer(item, true) {
                Some(Side::Left)
            } else if sync::is_newer(item, false) {
                Some(Side::Right)
            } else {
                None
            };
            Some(SyncRow {
                index: usize32(row),
                plan_index: usize32(ix),
                icon,
                tooltip: tooltip.to_string(),
                tint,
                is_dir: sync::is_dir(item),
                rel: item.rel_lossy(),
                state: sync::state_label(item.state).to_string(),
                left: item.left.as_ref().map(|s| side(s, dates)),
                right: item.right.as_ref().map(|s| side(s, dates)),
                newer,
            })
        })
        .collect()
}

/// One side's text, its date (for a file) in the settings' format.
fn side(side: &SyncSide, dates: DateFormat) -> String {
    match sync::side_text(side) {
        (text, Some(mtime)) => format!("{text} · {}", crate::format::date(Some(mtime), dates)),
        (text, None) => text,
    }
}

#[uniffi::export]
impl Core {
    /// Rows `start..start + count` of a synchronize dialog (fetch again
    /// when `SyncSnapshot.rows_version` moves).
    pub fn sync_rows(&self, dialog: u64, start: u32, count: u32) -> Vec<SyncRow> {
        let view = self.view();
        let dates = view.settings.general.date_format;
        view.dialogs
            .iter()
            .find_map(|d| match &d.kind {
                DialogKind::Sync(s) if d.id.0 == dialog => Some(rows(s, dates, start, count)),
                _ => None,
            })
            .unwrap_or_default()
    }
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyncDirection {
    Both,
    LeftToRight,
    RightToLeft,
}

impl From<CoreDirection> for SyncDirection {
    fn from(d: CoreDirection) -> Self {
        match d {
            CoreDirection::Both => SyncDirection::Both,
            CoreDirection::LeftToRight => SyncDirection::LeftToRight,
            CoreDirection::RightToLeft => SyncDirection::RightToLeft,
        }
    }
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
    Recursive {
        on: bool,
    },
    /// A direction button: resets every item's action, without comparing.
    Direction {
        direction: SyncDirection,
    },
    /// "Show identical".
    ShowSame {
        on: bool,
    },
    CompareAgain,
    /// An item's action button, by its index in the plan: cycles to the
    /// next possible action.
    Cycle {
        index: u32,
    },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{Command, Input};
    use crate::rename::tests::Fixture;
    use crate::snapshot::{DialogKindSnapshot, Snapshot};

    fn sync_dialog(snap: &Snapshot) -> Option<(u64, SyncSnapshot)> {
        snap.dialogs.iter().find_map(|d| match &d.kind {
            DialogKindSnapshot::Sync { dialog } => Some((d.id, dialog.clone())),
            _ => None,
        })
    }

    #[test]
    fn the_sync_dialog_mirrors_the_plan_and_its_rows_come_by_range() {
        let f = Fixture::start();
        f.command(Command::SyncDirs);
        let snap = f.wait(|s| sync_dialog(s).is_some_and(|(_, d)| d.body == SyncBody::List));
        let (id, d) = sync_dialog(&snap).unwrap();
        assert_eq!(d.title, "Synchronize Directories");
        assert!(d.left_display.ends_with("left") && d.right_display.ends_with("right"));
        assert!(d.recursive && !d.show_same);
        assert_eq!(d.direction, SyncDirection::Both);
        assert_eq!(
            d.directions
                .iter()
                .map(|o| o.label.as_str())
                .collect::<Vec<_>>(),
            ["Both ways", "Left → right", "Right → left"]
        );
        assert!(d.can_compare && d.can_synchronize && !d.server_side);
        assert!(d.note.starts_with("Newer files win"));
        assert_eq!(d.row_count, 3);
        assert!(d.to_right >= 2, "{d:?}");
        assert!(d.summary.starts_with("→ "), "{}", d.summary);

        let rows = f.core.sync_rows(id, 0, 100);
        assert_eq!(rows.len(), 3);
        let only_left: Vec<&SyncRow> = rows.iter().filter(|r| r.state == "only left").collect();
        assert_eq!(only_left.len(), 2);
        for row in &only_left {
            assert_eq!(
                (row.icon, row.tint),
                (SyncActionIcon::ArrowRight, SyncTint::Accent)
            );
            assert_eq!(row.tooltip, "Copy to the right (click to change)");
            assert!(row.left.is_some() && row.right.is_none());
        }
        let dir = rows.iter().find(|r| r.rel == "dir").unwrap();
        assert!(dir.is_dir);
        assert!(dir.left.as_deref().unwrap().starts_with("folder"));
        let file = rows.iter().find(|r| r.rel == "b.rs").unwrap();
        // "12 B · 2026-11-22 10:00": the size, then the date in ISO form.
        let text = file.left.as_deref().unwrap();
        assert!(
            text.starts_with("12 B · ") && text.len() == "12 B · ".len() + 16,
            "{text}"
        );
        assert_eq!(f.core.sync_rows(id, 2, 10).len(), 1);
        assert!(f.core.sync_rows(id, 3, 10).is_empty());

        // Clicking an action cycles it and moves the version.
        let version = d.rows_version;
        assert_eq!(
            sync_dialog(&f.core.snapshot()).unwrap().1.rows_version,
            version
        );
        let snap = f.send(Input::Sync {
            dialog: id,
            input: SyncInput::Cycle {
                index: file.plan_index,
            },
        });
        let (_, after) = sync_dialog(&snap).unwrap();
        assert_ne!(after.rows_version, version);
        assert_eq!(after.to_right, d.to_right - 1);
        let file = f.core.sync_rows(id, file.index, 1).remove(0);
        assert_eq!(file.rel, "b.rs");
        assert_ne!(file.icon, SyncActionIcon::ArrowRight);

        // Direction and filter.
        let snap = f.send(Input::Sync {
            dialog: id,
            input: SyncInput::Direction {
                direction: SyncDirection::RightToLeft,
            },
        });
        assert_eq!(
            sync_dialog(&snap).unwrap().1.direction,
            SyncDirection::RightToLeft
        );
        let snap = f.send(Input::Sync {
            dialog: id,
            input: SyncInput::ShowSame { on: true },
        });
        assert!(sync_dialog(&snap).unwrap().1.show_same);

        // Comparing again shows the spinner state until it is done.
        let snap = f.send(Input::Sync {
            dialog: id,
            input: SyncInput::Recursive { on: false },
        });
        let (_, d) = sync_dialog(&snap).unwrap();
        assert!(!d.recursive);
        f.wait(|s| sync_dialog(s).is_some_and(|(_, d)| d.can_compare));

        f.send(Input::Dialog {
            id,
            input: crate::input::DialogInput::Cancel,
        });
        f.wait(|s| s.dialogs.is_empty());
        f.rt.shutdown();
    }
}
