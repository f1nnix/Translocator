//! AREA: the multi-rename dialog (`DialogKind::Rename` in the core).
//!
//! Owns [`RenameSnapshot`] (carried by `DialogKindSnapshot::Rename`) and
//! [`RenameInput`], the full mirror of `tl_app::rename::RenameInput`, sent
//! as `Input::Rename { dialog, input }`.
//!
//! STUB: the snapshot carries the title only. The next step adds the
//! fields (masks, find/replace, counter, regex/case toggles, the case
//! picker from `tl_app::rename::CASES`) and the preview. The preview is a
//! long list, so it follows the rows pattern: `preview_count` and a
//! `preview_version` (from a `crate::versions::Tracker` keyed by the
//! preview `Arc`'s address) in the snapshot, and a range fetch on `Core`
//! (`rename_preview(dialog, start, count)`). The Swift side is
//! `macos/Sources/Translocator/Areas/RenameDialog.swift`.

use tl_app::rename;
use tl_app::tl_model::multirename;

/// What the multi-rename dialog shows.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct RenameSnapshot {
    /// "Multi-Rename “a.txt”" / "Multi-Rename 3 items".
    pub title: String,
}

pub(crate) fn snapshot(dialog: &rename::RenameDialog) -> RenameSnapshot {
    RenameSnapshot {
        title: dialog.title.clone(),
    }
}

/// The dialog's text fields.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RenameField {
    Name,
    Ext,
    Find,
    Replace,
    Start,
    Step,
    Digits,
}

impl From<RenameField> for rename::Field {
    fn from(field: RenameField) -> Self {
        match field {
            RenameField::Name => rename::Field::Name,
            RenameField::Ext => rename::Field::Ext,
            RenameField::Find => rename::Field::Find,
            RenameField::Replace => rename::Field::Replace,
            RenameField::Start => rename::Field::Start,
            RenameField::Step => rename::Field::Step,
            RenameField::Digits => rename::Field::Digits,
        }
    }
}

/// Letter case applied to new names.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CaseChange {
    Keep,
    Lower,
    Upper,
    /// First letter upper case, the rest lower case.
    FirstUpper,
    /// Every word starts with an upper-case letter.
    Title,
}

impl From<CaseChange> for multirename::CaseChange {
    fn from(case: CaseChange) -> Self {
        use multirename::CaseChange as C;
        match case {
            CaseChange::Keep => C::Keep,
            CaseChange::Lower => C::Lower,
            CaseChange::Upper => C::Upper,
            CaseChange::FirstUpper => C::FirstUpper,
            CaseChange::Title => C::Title,
        }
    }
}

/// Edits in the multi-rename dialog.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum RenameInput {
    Text { field: RenameField, text: String },
    /// A placeholder button: appends `token` to the field.
    Insert { field: RenameField, token: String },
    Regex { on: bool },
    MatchCase { on: bool },
    CaseChange { change: CaseChange },
    /// The Rename button, or Enter: with problems the dialog stays open.
    Apply,
}

impl From<RenameInput> for rename::RenameInput {
    fn from(input: RenameInput) -> Self {
        use rename::RenameInput as R;
        match input {
            RenameInput::Text { field, text } => R::Text(field.into(), text),
            RenameInput::Insert { field, token } => R::Insert(field.into(), token),
            RenameInput::Regex { on } => R::Regex(on),
            RenameInput::MatchCase { on } => R::MatchCase(on),
            RenameInput::CaseChange { change } => R::Case(change.into()),
            RenameInput::Apply => R::Apply,
        }
    }
}
