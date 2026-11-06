//! Core of Translocator: the provider-neutral virtual file system API that
//! panes talk to, the plugin registry that turns bookmarks into live
//! connections, and the transfer engine that moves data between providers —
//! plus what is built on them: directory compare and sync, multi-rename and
//! archives shown as folders.

pub mod archive;
pub mod askpass;
pub mod bookmark;
pub mod cancel;
pub mod compare;
pub mod entry;
pub mod error;
pub mod multirename;
pub mod path;
pub mod plugin;
pub mod progress;
pub mod prompt;
pub mod shell;
pub mod sync;
pub mod transfer;
pub mod util;
pub mod vfs;

pub use archive::{ArchiveBackend, ArchiveFormat, ArchiveMember};
pub use bookmark::{Bookmark, BookmarkConfig, BookmarkStore, Place};
pub use cancel::{CancelGuard, CancelToken};
pub use entry::{Entry, EntryKind};
pub use error::{VfsError, VfsResult};
pub use path::VPath;
pub use plugin::{FieldKind, FieldSpec, PluginEnv, PluginRegistry, ProviderPlugin};
pub use progress::{Progress, ProgressState};
pub use prompt::Prompter;
pub use vfs::{
    Capabilities, ExecOutput, MergePolicy, OpCtx, OwnerHint, Resolution, ScanTotals, SpaceInfo,
    TarSink, TarSource, TransferItem, Vfs, WalkEntry,
};
