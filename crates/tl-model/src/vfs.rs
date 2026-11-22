//! The data a provider call takes and returns, shared by the providers and
//! the core. The provider API itself (`Vfs`) is `tl_core::vfs`.

use crate::entry::EntryKind;
use crate::path::VPath;

/// Who should own files created by an elevated copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OwnerHint {
    /// Whatever the operation naturally produces (`cp -a` keeps the source
    /// owner when run as root).
    #[default]
    Natural,
    /// The copy ran elevated only because the *source* needed it; hand the
    /// new files to the login user, as if they had copied them themselves.
    LoginUser,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Capabilities {
    /// Operations can run as another user (sudo).
    pub elevation: bool,
    /// Deleting can move items to a recoverable trash.
    pub trash: bool,
    /// Arbitrary shell commands can run in a directory (command line).
    pub exec: bool,
    pub chmod: bool,
    pub chown: bool,
    pub symlink: bool,
    /// Names differing only in case refer to the same file.
    pub case_insensitive: bool,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct SpaceInfo {
    pub total: u64,
    pub available: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: i32,
}

/// Totals of a recursive scan, for progress reporting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScanTotals {
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
}

/// One node of a recursive listing; `rel` is relative to the walk root.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct WalkEntry {
    pub rel: Vec<u8>,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime: Option<i64>,
}

/// How a transfer item treats what already exists at its destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Resolution {
    /// Nothing exists at the destination.
    Create,
    /// Remove the destination, then put the source in its place.
    Replace,
    /// Both are directories: merge the source into the destination.
    Merge(MergePolicy),
}

/// What a merge does with files that exist on both sides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MergePolicy {
    Overwrite,
    /// Overwrite only when the source file is newer.
    OverwriteOlder,
    /// Keep existing files, add only new ones.
    SkipExisting,
}

/// One top-level item of a copy/move, already resolved against the
/// destination by the transfer engine.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct TransferItem {
    pub src: VPath,
    /// Full destination path (may carry a new name).
    pub dst: VPath,
    pub kind: EntryKind,
    pub resolution: Resolution,
}

