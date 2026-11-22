//! Translocator's shared model: the plain data the providers, the Crux core
//! and the shell all speak, and the logic that needs nothing but that data.
//!
//! Nothing in this crate touches a file, a socket, a clock or a random
//! number generator (bar [`bookmark::Bookmark::new`] behind the `uuid`
//! feature, which only the I/O side turns on). That is what lets the Crux
//! core in `tl-app` build on it and stay side-effect-free: listing a folder,
//! copying a file or reading the time are all effects the shell performs.

pub mod archive;
pub mod bookmark;
pub mod compare;
pub mod entry;
pub mod error;
pub mod multirename;
pub mod path;
pub mod plugin;
pub mod progress;
pub mod sync;
pub mod transfer;
pub mod util;
pub mod vfs;

pub use archive::{ArchiveFormat, ArchiveMember};
pub use bookmark::{Bookmark, BookmarkConfig, Bookmarks, Place};
pub use entry::{Entry, EntryKind};
pub use error::{VfsError, VfsResult};
pub use path::VPath;
pub use plugin::{FieldInfo, FieldKind, PluginInfo};
pub use progress::ProgressState;
pub use vfs::{Capabilities, ExecOutput, MergePolicy, OwnerHint, Resolution, ScanTotals, SpaceInfo, TransferItem, WalkEntry};
