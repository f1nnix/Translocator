//! The effects the core asks the shell to perform: everything that touches a
//! disk, a server, a clock, the clipboard or another process.
//!
//! Each kind of effect is an [`Operation`]: a request the core builds as
//! plain data, and the output the shell resolves it with. The core never
//! holds a file system; it holds [`ConnId`]s, and the shell keeps the live
//! connection behind each one.

use crux_core::capability::Operation;
use serde::{Deserialize, Serialize};
use tl_model::bookmark::Bookmarks;
use tl_model::multirename::Rename;
use tl_model::sync::{SyncOptions, SyncPlan, SyncReport};
use tl_model::transfer::{Conflict, ConflictAnswer, TransferMode, TransferReport};
use tl_model::{
    Bookmark, BookmarkConfig, Entry, EntryKind, ExecOutput, PluginInfo, ProgressState, ScanTotals, SpaceInfo, VPath,
    VfsResult,
};

use crate::conn::ConnInfo;
use crate::ids::{ConnId, JobId, PromptId, TabId};
use crate::settings::Settings;

// ----- file system queries ---------------------------------------------

/// A quick question to a file system: one call, answered with [`FsOutput`].
/// Anything that changes files, or can take long, is a [`JobOp`] instead.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FsOp {
    /// The folder's entries and the free space on its file system.
    List { conn: ConnId, path: VPath, elevated: bool },
    /// `lstat`: `None` if nothing is there.
    Stat { conn: ConnId, path: VPath, elevated: bool },
    /// Up to `limit` bytes from the start of a file, and its size.
    Read { conn: ConnId, path: VPath, limit: u64, elevated: bool },
    /// Asks for and checks what sudo needs (a password, once).
    PrepareElevation { conn: ConnId },
    /// Mounts an archive file of `conn` as the file system `new`.
    OpenArchive { conn: ConnId, file: VPath, elevated: bool, new: ConnId },
    /// Where a pane opens on an existing connection of `bookmark`.
    InitialPath { conn: ConnId, bookmark: Bookmark },
    /// The connection is no longer used: the shell forgets it, and closes
    /// it if it is a server.
    Release { conn: ConnId },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FsOutput {
    Listing {
        entries: VfsResult<Vec<Entry>>,
        space: Option<SpaceInfo>,
    },
    Stat(VfsResult<Option<Entry>>),
    Read(VfsResult<(Vec<u8>, u64)>),
    Done(VfsResult<()>),
    Opened(VfsResult<ConnInfo>),
    Path(VPath),
}

impl Operation for FsOp {
    type Output = FsOutput;
}

// ----- jobs ----------------------------------------------------------------

/// One end of a synchronization.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncEnd {
    pub conn: ConnId,
    pub dir: VPath,
    pub elevated: bool,
}

/// How a transfer settles name clashes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Conflicts {
    /// Each one is a question to the user ([`JobUpdate::Conflict`]).
    Ask,
    /// Overwrite without asking (refreshing a cached copy).
    Overwrite,
}

/// A copy or move.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransferReq {
    pub mode: TransferMode,
    pub src: ConnId,
    pub src_dir: VPath,
    pub names: Vec<Vec<u8>>,
    pub src_elevated: bool,
    pub dst: ConnId,
    pub dst_dir: VPath,
    pub dst_elevated: bool,
    /// New name for a single item ("copy as").
    pub target_name: Option<Vec<u8>>,
    pub conflicts: Conflicts,
}

/// The work of a job. Every variant that acts on one file system says
/// whether it runs elevated; [`Work::with_elevation`] flips that for the
/// "retry with sudo" offer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Work {
    Transfer(TransferReq),
    Delete { conn: ConnId, paths: Vec<VPath>, trash: bool, elevated: bool },
    /// Creates `base/parts[0]/parts[1]/...`, each folder only if missing.
    MakeDirs { conn: ConnId, base: VPath, parts: Vec<Vec<u8>>, elevated: bool },
    CreateFile { conn: ConnId, path: VPath, elevated: bool },
    Rename { conn: ConnId, from: VPath, to: VPath, elevated: bool },
    /// Planned multi-rename steps; a failure undoes the ones done.
    Renames { conn: ConnId, dir: VPath, steps: Vec<Rename>, elevated: bool },
    WriteFile { conn: ConnId, path: VPath, data: Vec<u8>, elevated: bool },
    Attributes {
        conn: ConnId,
        paths: Vec<VPath>,
        mode: Option<u32>,
        owner: Option<String>,
        group: Option<String>,
        recursive: bool,
        elevated: bool,
    },
    /// Recursive totals of each named folder in `dir`.
    Scan { conn: ConnId, dir: VPath, names: Vec<Vec<u8>>, elevated: bool },
    Find { conn: ConnId, root: VPath, masks: Vec<String>, limit: usize, elevated: bool },
    Exec { conn: ConnId, cwd: VPath, command: String, elevated: bool },
    /// Connects `bookmark` as the file system `conn`, preparing sudo when
    /// `sudo` is set.
    Connect { conn: ConnId, bookmark: Bookmark, sudo: bool },
    SyncCompare { left: SyncEnd, right: SyncEnd, options: SyncOptions },
    SyncApply { plan: SyncPlan, left: SyncEnd, right: SyncEnd },
    /// Downloads a file of a server into the local cache and opens the
    /// copy with its application.
    OpenCopy { conn: ConnId, dir: VPath, name: Vec<u8>, elevated: bool },
}

impl Work {
    /// The file system whose sudo a "permission denied" can be retried with,
    /// and whether the work already uses it.
    pub fn elevation_target(&self) -> Option<(ConnId, bool)> {
        match self {
            Work::Delete { conn, elevated, .. }
            | Work::MakeDirs { conn, elevated, .. }
            | Work::CreateFile { conn, elevated, .. }
            | Work::Rename { conn, elevated, .. }
            | Work::Renames { conn, elevated, .. }
            | Work::WriteFile { conn, elevated, .. }
            | Work::Attributes { conn, elevated, .. }
            | Work::Scan { conn, elevated, .. }
            | Work::Find { conn, elevated, .. }
            | Work::Exec { conn, elevated, .. }
            | Work::OpenCopy { conn, elevated, .. } => Some((*conn, *elevated)),
            _ => None,
        }
    }

    /// The same work, elevated.
    pub fn with_elevation(&self) -> Work {
        let mut work = self.clone();
        match &mut work {
            Work::Delete { elevated, .. }
            | Work::MakeDirs { elevated, .. }
            | Work::CreateFile { elevated, .. }
            | Work::Rename { elevated, .. }
            | Work::Renames { elevated, .. }
            | Work::WriteFile { elevated, .. }
            | Work::Attributes { elevated, .. }
            | Work::Scan { elevated, .. }
            | Work::Find { elevated, .. }
            | Work::Exec { elevated, .. }
            | Work::OpenCopy { elevated, .. } => *elevated = true,
            _ => {}
        }
        work
    }
}

/// What a finished job hands back.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum JobOutput {
    Done,
    Transfer(TransferReport),
    Sizes(Vec<(Vec<u8>, ScanTotals)>),
    Found(Vec<(VPath, EntryKind)>),
    Exec(ExecOutput),
    /// The connection is up; `server_info` describes it.
    Connected { info: ConnInfo, initial: VPath },
    Plan(SyncPlan),
    Synced(SyncReport),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum JobUpdate {
    /// A progress snapshot, sent while the job runs (a few times a second
    /// at most). `elapsed_ms` counts from the job's start.
    Progress { state: ProgressState, elapsed_ms: u64 },
    /// The job waits for the answer to this clash ([`JobOp::Answer`]).
    Conflict { ask: u64, conflict: Conflict },
    /// The last update of a job.
    Finished(VfsResult<JobOutput>),
}

/// Background work: copies, deletions, searches, connections. A job streams
/// [`JobUpdate`]s and always ends with [`JobUpdate::Finished`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum JobOp {
    Start { id: JobId, work: Work },
    /// Asks a running job to stop; it still finishes (as cancelled).
    Cancel { id: JobId },
    Answer { id: JobId, ask: u64, answer: ConflictAnswer },
}

impl Operation for JobOp {
    type Output = JobUpdate;
}

// ----- prompts from the providers -------------------------------------------

/// A question a provider asks while it works: a password, a host key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptKind {
    Secret { title: String, message: String },
    Confirm { title: String, message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptRequest {
    pub id: PromptId,
    pub kind: PromptKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptAnswer {
    Secret(Option<String>),
    Confirm(bool),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptOp {
    /// Every question the providers will ask, for as long as the app runs.
    Subscribe,
    Answer { id: PromptId, answer: PromptAnswer },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptOutput {
    Request(PromptRequest),
    Answered,
}

impl Operation for PromptOp {
    type Output = PromptOutput;
}

// ----- what is kept between runs --------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum StoreOp {
    Load,
    /// Writes the settings (the shell skips a write that changes nothing).
    SaveSettings(Box<Settings>),
    SaveBookmarks(Bookmarks),
    /// Writes whatever is still pending, before the app quits.
    Flush,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StoreOutput {
    Loaded { settings: Box<Settings>, bookmarks: Bookmarks },
    Saved(Result<(), String>),
}

impl Operation for StoreOp {
    type Output = StoreOutput;
}

// ----- watching local folders -------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WatchOp {
    /// Streams a `Changed` whenever the local folder changes, already
    /// debounced by the shell.
    Start { tab: TabId, path: VPath },
    Stop { tab: TabId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WatchOutput {
    Changed,
    Stopped,
}

impl Operation for WatchOp {
    type Output = WatchOutput;
}

// ----- connection health ------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthOp {
    /// Streams a change whenever a connection drops (or comes back).
    Subscribe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    pub conn: ConnId,
    pub connected: bool,
}

impl Operation for HealthOp {
    type Output = Health;
}

// ----- the desktop ----------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

/// A local folder worth offering in the locations menu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalFolder {
    pub label: String,
    pub path: VPath,
    /// A mounted volume rather than a folder of the home directory.
    pub volume: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PlatformOp {
    /// Opens a local file or folder with its application.
    Open(VPath),
    /// Shows a local file in the system file manager.
    Reveal(VPath),
    WriteClipboard(String),
    OpenUrl(String),
    /// A transient notification in the main window.
    Toast { kind: ToastKind, message: String },
    /// A terminal in `path` of `conn` (a local folder, or a login on the
    /// server); `preferred` is the setting.
    OpenTerminal { conn: ConnId, path: VPath, elevated: bool, preferred: String },
    /// Local calendar fields of Unix times, in the platform's time zone.
    LocalTimes(Vec<Option<i64>>),
    /// Home, Desktop, Documents, Downloads (those that exist) and volumes.
    LocalFolders,
    Quit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PlatformOutput {
    Done,
    Failed(String),
    Times(Vec<Option<[i64; 6]>>),
    Folders(Vec<LocalFolder>),
}

impl Operation for PlatformOp {
    type Output = PlatformOutput;
}

// ----- provider plugins -----------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PluginOp {
    /// The plugins compiled in, and the local file system.
    List,
    /// The plugin's own checks of a bookmark configuration.
    Validate { plugin: String, config: BookmarkConfig },
    /// One-line descriptions of bookmarks ("deploy@web-1:2222").
    Summaries(Vec<Bookmark>),
}

// A message that lives for one update; boxing would only move the bytes.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PluginOutput {
    Plugins { plugins: Vec<PluginInfo>, local: ConnInfo },
    Valid(Result<(), String>),
    Summaries(Vec<String>),
}

impl Operation for PluginOp {
    type Output = PluginOutput;
}

// ----- time -------------------------------------------------------------------------

/// Resolves after the given number of milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SleepOp(pub u64);

impl Operation for SleepOp {
    type Output = ();
}
