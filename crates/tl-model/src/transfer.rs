//! What a copy or move is about, as data: the mode, the conflicts the
//! engine asks about and the answers it takes, and the report it ends with.
//! The engine is `tl_core::transfer`.

use crate::entry::Entry;
use crate::path::VPath;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TransferMode {
    Copy,
    Move,
}

/// An item about to land on something that already exists.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Conflict {
    pub source: Entry,
    pub existing: Entry,
    pub dst_dir: VPath,
    /// Both sides are real directories, so the overwrite choices merge.
    pub both_dirs: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ConflictChoice {
    /// File: replace it. Folder: merge, overwriting files inside.
    Overwrite,
    /// File: replace it if the source is newer. Folder: merge, overwriting
    /// only older files.
    OverwriteOlder,
    /// Folder: merge, keeping existing files. File: same as `Skip`.
    SkipExisting,
    Skip,
    /// Transfer under a new, unused name.
    KeepBoth,
    Cancel,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ConflictAnswer {
    pub choice: ConflictChoice,
    pub apply_to_all: bool,
    /// Explicit name for `KeepBoth`; generated when `None`.
    pub rename_to: Option<Vec<u8>>,
}

impl ConflictAnswer {
    pub fn new(choice: ConflictChoice) -> Self {
        ConflictAnswer {
            choice,
            apply_to_all: false,
            rename_to: None,
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct TransferReport {
    pub transferred: usize,
    pub skipped: usize,
    pub server_side: bool,
    /// Final names of transferred items (to select them afterwards).
    pub names: Vec<Vec<u8>>,
}

