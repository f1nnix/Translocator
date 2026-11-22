//! Small value types shared by inputs and snapshots, mirrored from the core
//! with conversions both ways. They carry no behaviour of their own.

use tl_app::tl_model;

/// One of the two panes.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

impl From<Side> for tl_app::Side {
    fn from(side: Side) -> Self {
        match side {
            Side::Left => tl_app::Side::Left,
            Side::Right => tl_app::Side::Right,
        }
    }
}

impl From<tl_app::Side> for Side {
    fn from(side: tl_app::Side) -> Self {
        match side {
            tl_app::Side::Left => Side::Left,
            tl_app::Side::Right => Side::Right,
        }
    }
}

/// Column a list is sorted by.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SortKey {
    Name,
    Ext,
    Size,
    Modified,
}

impl From<SortKey> for tl_app::settings::SortKey {
    fn from(key: SortKey) -> Self {
        use tl_app::settings::SortKey as K;
        match key {
            SortKey::Name => K::Name,
            SortKey::Ext => K::Ext,
            SortKey::Size => K::Size,
            SortKey::Modified => K::Modified,
        }
    }
}

impl From<tl_app::settings::SortKey> for SortKey {
    fn from(key: tl_app::settings::SortKey) -> Self {
        use tl_app::settings::SortKey as K;
        match key {
            K::Name => SortKey::Name,
            K::Ext => SortKey::Ext,
            K::Size => SortKey::Size,
            K::Modified => SortKey::Modified,
        }
    }
}

/// How a date is written in the file list.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DateFormat {
    /// `2026-10-15 14:32`
    Iso,
    /// `15.10.2026 14:32`
    DayFirst,
    /// `10/15/2026 2:32 PM`
    MonthFirst,
    /// `today 14:32`, `Oct 3 09:12`, `2024-03-01 11:40`
    Relative,
}

impl From<DateFormat> for tl_app::settings::DateFormat {
    fn from(format: DateFormat) -> Self {
        use tl_app::settings::DateFormat as F;
        match format {
            DateFormat::Iso => F::Iso,
            DateFormat::DayFirst => F::DayFirst,
            DateFormat::MonthFirst => F::MonthFirst,
            DateFormat::Relative => F::Relative,
        }
    }
}

impl From<tl_app::settings::DateFormat> for DateFormat {
    fn from(format: tl_app::settings::DateFormat) -> Self {
        use tl_app::settings::DateFormat as F;
        match format {
            F::Iso => DateFormat::Iso,
            F::DayFirst => DateFormat::DayFirst,
            F::MonthFirst => DateFormat::MonthFirst,
            F::Relative => DateFormat::Relative,
        }
    }
}

/// Light, dark, or whatever the system uses.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppearanceMode {
    System,
    Light,
    Dark,
}

impl From<AppearanceMode> for tl_app::settings::AppearanceMode {
    fn from(mode: AppearanceMode) -> Self {
        use tl_app::settings::AppearanceMode as M;
        match mode {
            AppearanceMode::System => M::System,
            AppearanceMode::Light => M::Light,
            AppearanceMode::Dark => M::Dark,
        }
    }
}

impl From<tl_app::settings::AppearanceMode> for AppearanceMode {
    fn from(mode: tl_app::settings::AppearanceMode) -> Self {
        use tl_app::settings::AppearanceMode as M;
        match mode {
            M::System => AppearanceMode::System,
            M::Light => AppearanceMode::Light,
            M::Dark => AppearanceMode::Dark,
        }
    }
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransferMode {
    Copy,
    Move,
}

impl From<TransferMode> for tl_model::transfer::TransferMode {
    fn from(mode: TransferMode) -> Self {
        match mode {
            TransferMode::Copy => tl_model::transfer::TransferMode::Copy,
            TransferMode::Move => tl_model::transfer::TransferMode::Move,
        }
    }
}

/// How a transfer settles a name clash.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConflictChoice {
    /// File: replace it. Folder: merge, overwriting files inside.
    Overwrite,
    /// Replace only what is older than the incoming item.
    OverwriteOlder,
    /// Folder: merge, keeping existing files. File: same as `Skip`.
    SkipExisting,
    Skip,
    /// Transfer under a new, unused name.
    KeepBoth,
    Cancel,
}

impl From<ConflictChoice> for tl_model::transfer::ConflictChoice {
    fn from(choice: ConflictChoice) -> Self {
        use tl_model::transfer::ConflictChoice as C;
        match choice {
            ConflictChoice::Overwrite => C::Overwrite,
            ConflictChoice::OverwriteOlder => C::OverwriteOlder,
            ConflictChoice::SkipExisting => C::SkipExisting,
            ConflictChoice::Skip => C::Skip,
            ConflictChoice::KeepBoth => C::KeepBoth,
            ConflictChoice::Cancel => C::Cancel,
        }
    }
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

impl From<tl_app::ToastKind> for ToastKind {
    fn from(kind: tl_app::ToastKind) -> Self {
        match kind {
            tl_app::ToastKind::Info => ToastKind::Info,
            tl_app::ToastKind::Success => ToastKind::Success,
            tl_app::ToastKind::Error => ToastKind::Error,
        }
    }
}

/// What a pane's folder is on.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocationKind {
    Local,
    Remote,
    /// An archive opened as a folder.
    Archive,
}

impl From<tl_app::view::LocationKind> for LocationKind {
    fn from(kind: tl_app::view::LocationKind) -> Self {
        use tl_app::view::LocationKind as K;
        match kind {
            K::Local => LocationKind::Local,
            K::Remote => LocationKind::Remote,
            K::Archive => LocationKind::Archive,
        }
    }
}

/// What a file system can do.
#[derive(uniffi::Record, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Caps {
    /// Operations can run as root (sudo).
    pub elevation: bool,
    /// Deleting can move items to a recoverable trash.
    pub trash: bool,
    /// Shell commands can run in a folder (the command line).
    pub exec: bool,
    pub chmod: bool,
    pub chown: bool,
    pub symlink: bool,
    /// Names differing only in case are the same file.
    pub case_insensitive: bool,
}

impl From<tl_model::Capabilities> for Caps {
    fn from(c: tl_model::Capabilities) -> Self {
        Caps {
            elevation: c.elevation,
            trash: c.trash,
            exec: c.exec,
            chmod: c.chmod,
            chown: c.chown,
            symlink: c.symlink,
            case_insensitive: c.case_insensitive,
        }
    }
}

/// A range of text in UTF-16 code units, the way AppKit counts.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Utf16Range {
    pub start: u32,
    pub length: u32,
}

impl Utf16Range {
    /// A byte range of `text` (clamped to it), in UTF-16 units.
    pub fn from_bytes(text: &str, start: usize, end: usize) -> Utf16Range {
        let floor = |mut at: usize| {
            at = at.min(text.len());
            while !text.is_char_boundary(at) {
                at -= 1;
            }
            at
        };
        let (start, end) = (floor(start), floor(end.max(start)));
        let units = |s: &str| s.encode_utf16().count() as u32;
        Utf16Range {
            start: units(&text[..start]),
            length: units(&text[start..end]),
        }
    }
}

/// Raw path bytes from the core, kept exact so a non-UTF-8 name on a
/// server still round-trips.
pub(crate) fn path_bytes(path: &tl_model::VPath) -> Vec<u8> {
    path.as_bytes().to_vec()
}

pub(crate) fn vpath(bytes: &[u8]) -> tl_model::VPath {
    tl_model::VPath::new(bytes)
}

pub(crate) fn usize32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_ranges_become_utf16_ranges() {
        // "é" is two bytes and one UTF-16 unit; "𝄞" is four and two.
        let text = "é𝄞ab.txt";
        let (start, end) = tl_app::dialog::stem_range(text);
        let range = Utf16Range::from_bytes(text, start, end);
        assert_eq!(range, Utf16Range { start: 0, length: 5 });
        // Out of range and mid-character ends are clamped.
        assert_eq!(Utf16Range::from_bytes(text, 1, 100), Utf16Range { start: 0, length: 9 });
    }
}
