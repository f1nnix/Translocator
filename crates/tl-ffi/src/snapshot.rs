//! What the main window draws, as one small record.
//!
//! [`Snapshot`] stays the same size whatever the folders hold: a pane
//! carries its row count, cursor and a `listing_version`, never its rows
//! (those are fetched by range, see [`crate::rows`]). Long lists elsewhere
//! follow the same pattern: a count and a version here, a fetch on `Core`.
//! One-shot requests (focus, the locations menu, the settings window,
//! clearing the command line) are counters: the shell acts when one moves.

use std::collections::{BTreeMap, HashMap};

use tl_app::conn::LOCAL_PLUGIN;
use tl_app::dialog::{DialogKind, Found};
use tl_app::jobs::JobStatus;
use tl_app::model::FocusTarget as CoreFocus;
use tl_app::pane::{PARENT, is_archive};
use tl_app::settings::DateFormat as CoreDateFormat;
use tl_app::tl_model::transfer::{Conflict, ConflictChoice as CoreChoice};
use tl_app::tl_model::util::human_size;
use tl_app::tl_model::Entry;
use tl_app::view::{ColumnView, JobView, LocationKind as CoreKind, PaneView, ViewModel};

use crate::types::{Caps, ConflictChoice, LocationKind, Side, SortKey, Utf16Range, ViewMode, path_bytes, usize32};
use crate::versions::{Tracker, addr};
use crate::{bookmarks, editor, properties, quick_look, rename, settings, sync};

#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// The panes are open (startup has finished).
    pub ready: bool,
    /// The main window's title.
    pub title: String,
    /// The pane the keyboard commands act on.
    pub active: Side,
    pub left: PaneSnapshot,
    pub right: PaneSnapshot,
    /// The preview, drawn in place of the pane opposite `active`.
    pub quick_look: Option<quick_look::QuickLookSnapshot>,
    /// Open dialogs, the last one on top.
    pub dialogs: Vec<DialogSnapshot>,
    /// Listed jobs, oldest first.
    pub jobs: Vec<JobSnapshot>,
    /// Running jobs that are listed.
    pub running: u32,
    /// The jobs list is open.
    pub jobs_open: bool,
    pub console_shown: bool,
    /// Moves whenever the console's text changed (`Core::console_text`).
    pub console_version: u64,
    /// Left of the command line: `local:~/x $`.
    pub prompt: String,
    pub prompt_elevated: bool,
    /// Moves when the command line should be cleared.
    pub cmdline_clears: u64,
    pub settings: settings::SettingsSnapshot,
    /// Bookmarks, in their order, for the locations menu.
    pub bookmarks: Vec<BookmarkItem>,
    /// "This Mac" entries of the locations menu.
    pub local_folders: Vec<LocalFolderItem>,
    /// Viewer and editor windows that should be open.
    pub editors: Vec<editor::EditorSnapshot>,
    /// Moves when the settings window should come up.
    pub settings_requests: u64,
    /// A side's locations menu should open (keyboard: ⌘K, ⌥F1, ⌥F2).
    pub locations_request: Option<LocationsRequest>,
    /// Where the keyboard should go.
    pub focus_request: Option<FocusRequest>,
    /// Files are on the internal clipboard (Paste does something).
    pub has_clipboard: bool,
}

impl Snapshot {
    pub fn pane(&self, side: Side) -> &PaneSnapshot {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct LocationsRequest {
    pub side: Side,
    pub serial: u64,
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FocusTarget {
    Pane { side: Side },
    CommandLine,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FocusRequest {
    pub target: FocusTarget,
    pub serial: u64,
}

/// One component of the path bar.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Crumb {
    pub name: String,
    /// Raw path, for `PaneInput::Navigate`.
    pub path: Vec<u8>,
}

/// A folder saved on the pane's row of places.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct PlaceItem {
    pub id: String,
    pub name: String,
    /// Raw path, for `PaneInput::Navigate`.
    pub path: Vec<u8>,
    /// The path as text, for the tooltip.
    pub display: String,
    /// The pane is in this folder.
    pub current: bool,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct PaneSnapshot {
    pub side: Side,
    pub tab_id: u64,
    /// One per tab; the strip shows only when there are two or more.
    pub tab_titles: Vec<String>,
    pub active_tab: u32,
    pub conn: u64,
    /// The folder, raw.
    pub path: Vec<u8>,
    pub elevated: bool,
    pub caps: Caps,
    pub kind: LocationKind,
    /// "Local" or the server's label.
    pub location_label: String,
    /// The file system holding this folder (or its archive) is this machine.
    pub host_local: bool,
    /// `~/Projects`, `deploy@web-1:/etc`.
    pub display: String,
    /// From the root, each with its full path; the root itself is not one.
    pub crumbs: Vec<Crumb>,
    pub loading: bool,
    pub error: Option<String>,
    /// The connection behind the pane is gone.
    pub disconnected: bool,
    pub can_reconnect: bool,
    /// Rows in the list, ".." included.
    pub row_count: u32,
    /// The cursor row.
    pub cursor: u32,
    /// Moves exactly when the rows (order, listing, marks, folder sizes or
    /// the date format) changed: drop cached rows and fetch again.
    pub listing_version: u64,
    /// The type-ahead filter; empty for none.
    pub filter: String,
    /// Rows other than "..".
    pub shown: u32,
    /// Rows before the filter.
    pub unfiltered: u32,
    pub sort_key: SortKey,
    pub sort_descending: bool,
    /// How many items an operation would act on: the marked ones, or the
    /// cursor row (0 on "..").
    pub selection_count: u32,
    pub marked_count: u32,
    /// The cursor is on an archive that can be opened as a folder.
    pub cursor_on_archive: bool,
    /// The left part of the status line.
    pub status: String,
    /// The right part: `12 GB free`.
    pub free: Option<String>,
    pub places: Vec<PlaceItem>,
    /// This folder is on the row of places.
    pub place_saved: bool,
    pub view_mode: ViewMode,
    /// In the column view: the folders left of the pane's own, leftmost
    /// first. Their rows come from `Core::column_rows`.
    pub columns: Vec<ColumnSnapshot>,
    /// In the column view: the folder under the cursor, right of the
    /// pane's own; `Core::column_rows` knows it as one past `columns`.
    pub preview: Option<ColumnSnapshot>,
}

/// A column other than the pane's own, in the column view.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ColumnSnapshot {
    /// The folder, raw.
    pub path: Vec<u8>,
    /// Its name, for the column's accessibility label.
    pub name: String,
    pub row_count: u32,
    /// The folder the path goes on into, drawn selected.
    pub selected: Option<u32>,
    /// Moves exactly when the rows changed: fetch them again.
    pub version: u64,
    pub loading: bool,
    pub error: Option<String>,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct BookmarkItem {
    pub id: String,
    pub name: String,
    /// The provider plugin's id ("ssh", "s3", "local", ...).
    pub plugin: String,
    /// A local-folder bookmark rather than a server.
    pub local: bool,
    /// It has a live connection (a checkmark in the menu).
    pub connected: bool,
    /// One-line description from its plugin.
    pub summary: Option<String>,
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FolderIcon {
    Home,
    Desktop,
    Documents,
    Downloads,
    Volume,
    Folder,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct LocalFolderItem {
    pub label: String,
    /// Raw path, for `PaneInput::OpenLocal`.
    pub path: Vec<u8>,
    pub display: String,
    /// A mounted volume rather than a folder of the home directory.
    pub volume: bool,
    pub icon: FolderIcon,
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobState {
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct JobSnapshot {
    pub id: u64,
    pub title: String,
    pub state: JobState,
    /// `0.0..=1.0`, or `None` when the job cannot tell.
    pub fraction: Option<f32>,
    /// Phase, amounts, rate and time while running; the outcome after.
    pub detail: String,
    /// The strip's detail: `1 GB / 3 GB`, `4 / 12` or the phase.
    pub short: String,
    /// The item being worked on.
    pub current: String,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct DialogSnapshot {
    /// What `Input::Dialog` (and the area inputs) name it by.
    pub id: u64,
    pub kind: DialogKindSnapshot,
}

#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum DialogKindSnapshot {
    /// A line of text: a name, a path, a mask.
    Input { dialog: InputDialogSnapshot },
    Confirm { dialog: ConfirmSnapshot },
    /// An error that needs reading (monospaced text, one OK button).
    Message { title: String, message: String },
    /// A provider asks for a password. Bring the app forward.
    Secret { title: String, message: String, text: String },
    /// A transfer asks how to settle a name clash.
    Conflict { dialog: ConflictSnapshot },
    /// Search hits: `count` of them, fetched with `Core::find_results`;
    /// `version` moves when they change.
    FindResults { title: String, count: u32, version: u64 },
    About { dialog: AboutSnapshot },
    Rename { dialog: rename::RenameSnapshot },
    Sync { dialog: sync::SyncSnapshot },
    Properties { dialog: properties::PropertiesSnapshot },
    BookmarkEdit { dialog: bookmarks::BookmarkEditSnapshot },
    Bookmarks { dialog: bookmarks::BookmarksManagerSnapshot },
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct InputDialogSnapshot {
    pub title: String,
    /// Shown above the field when not empty.
    pub message: String,
    pub text: String,
    /// What to preselect when the dialog opens; `None` selects everything.
    pub select: Option<Utf16Range>,
    /// The OK button's label.
    pub ok: String,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConfirmSnapshot {
    pub title: String,
    pub message: String,
    pub ok: String,
    /// The OK button is destructive.
    pub danger: bool,
    /// A provider's question: bring the app forward.
    pub from_provider: bool,
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Newer {
    Existing,
    Incoming,
    Unknown,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConflictButton {
    pub label: String,
    pub choice: ConflictChoice,
    /// The default button (Enter).
    pub primary: bool,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConflictSnapshot {
    /// "“a.txt” already exists" / "Folder “x” already exists".
    pub title: String,
    /// "in /dst".
    pub location: String,
    /// "file, 3.0 KB, modified today 14:02".
    pub existing: String,
    /// "Replace with" for files, "Incoming" for folders.
    pub incoming_label: String,
    pub incoming: String,
    /// Which side is newer: drawn in the success colour.
    pub newer: Newer,
    pub both_dirs: bool,
    /// "Apply to all remaining conflicts".
    pub apply_all: bool,
    /// Left to right; the last one is the primary.
    pub choices: Vec<ConflictButton>,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct AboutSnapshot {
    pub title: String,
    /// "Version 0.2.3".
    pub version: String,
    pub description: String,
    /// "MIT licensed · https://…".
    pub license: String,
}

/// One search hit.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FoundRow {
    /// Below the search root.
    pub rel: String,
    pub is_dir: bool,
}

/// What the Swift app's About box says (the GPUI shell's crate description).
pub const ABOUT_DESCRIPTION: &str = "Dual-pane file manager with an SSH command-protocol provider";

type ListingKey = (u64, usize, usize, usize, usize, CoreDateFormat);

/// The version counters, kept by `Core` between snapshots.
#[derive(Default)]
pub(crate) struct Trackers {
    listing: [Tracker<ListingKey>; 2],
    settings: Tracker<usize>,
    keys: Tracker<BTreeMap<String, Vec<String>>>,
    finds: HashMap<u64, Tracker<usize>>,
    rename: rename::Previews,
    sync: sync::Lists,
}

pub(crate) fn build(view: &ViewModel, t: &mut Trackers) -> Snapshot {
    let date_format = view.settings.general.date_format;
    let summaries = &view.summaries;
    let mut finds = std::mem::take(&mut t.finds);
    let mut kept = HashMap::new();
    let dialogs = view
        .dialogs
        .iter()
        .map(|d| {
            let kind = match &d.kind {
                DialogKind::FindResults { title, rows, .. } => {
                    // A dialog seen before keeps its counter.
                    let mut tracker = finds.remove(&d.id.0).unwrap_or_default();
                    let version = tracker.version(addr(rows));
                    kept.insert(d.id.0, tracker);
                    DialogKindSnapshot::FindResults {
                        title: title.clone(),
                        count: usize32(rows.len()),
                        version,
                    }
                }
                DialogKind::Rename(r) => DialogKindSnapshot::Rename {
                    dialog: t.rename.snapshot(d.id.0, r),
                },
                DialogKind::Sync(s) => DialogKindSnapshot::Sync {
                    dialog: t.sync.snapshot(d.id.0, s, date_format),
                },
                other => dialog_kind(other, view),
            };
            DialogSnapshot { id: d.id.0, kind }
        })
        .collect();
    t.finds = kept;
    t.rename.retain(&view.dialogs);
    t.sync.retain(&view.dialogs);

    let [left, right] = &view.panes;
    let left = pane(left, Side::Left, t.listing[0].version(listing_key(left, date_format)), date_format);
    let right = pane(right, Side::Right, t.listing[1].version(listing_key(right, date_format)), date_format);
    let settings_version = t.settings.version(addr(&view.settings));
    let keys_version = t.keys.version(view.settings.keys.clone());

    Snapshot {
        ready: view.ready,
        title: view.title.clone(),
        active: view.active.into(),
        left,
        right,
        quick_look: view.quick_look.as_ref().map(quick_look::snapshot),
        dialogs,
        jobs: view.jobs.iter().map(job).collect(),
        running: usize32(view.running),
        jobs_open: view.jobs_open,
        console_shown: view.console.shown,
        console_version: view.console.version,
        prompt: view.prompt.clone(),
        prompt_elevated: view.prompt_elevated,
        cmdline_clears: view.cmdline_clears,
        settings: settings::snapshot(&view.settings, settings_version, keys_version),
        bookmarks: view
            .bookmarks
            .bookmarks
            .iter()
            .map(|b| BookmarkItem {
                id: b.id.clone(),
                name: b.name.clone(),
                plugin: b.plugin.clone(),
                local: b.plugin == LOCAL_PLUGIN,
                connected: view.connected.contains(&b.id),
                summary: summaries.get(&b.id).cloned(),
            })
            .collect(),
        local_folders: view
            .local_folders
            .iter()
            .map(|f| LocalFolderItem {
                label: f.label.clone(),
                path: path_bytes(&f.path),
                display: f.path.display(),
                volume: f.volume,
                icon: match f.label.as_str() {
                    _ if f.volume => FolderIcon::Volume,
                    "Home" => FolderIcon::Home,
                    "Desktop" => FolderIcon::Desktop,
                    "Documents" => FolderIcon::Documents,
                    "Downloads" => FolderIcon::Downloads,
                    _ => FolderIcon::Folder,
                },
            })
            .collect(),
        editors: view.editors.iter().map(|e| editor::snapshot(e, &view.settings.keys)).collect(),
        settings_requests: view.settings_requests,
        locations_request: view.locations_request.map(|(side, serial)| LocationsRequest {
            side: side.into(),
            serial,
        }),
        focus_request: view.focus_request.map(|(target, serial)| FocusRequest {
            target: match target {
                CoreFocus::Pane(side) => FocusTarget::Pane { side: side.into() },
                CoreFocus::CommandLine => FocusTarget::CommandLine,
            },
            serial,
        }),
        has_clipboard: view.has_clipboard,
    }
}

fn listing_key(p: &PaneView, date_format: CoreDateFormat) -> ListingKey {
    (
        p.tab_id.0,
        addr(&p.listing.entries),
        addr(&p.order),
        addr(&p.marked),
        addr(&p.dir_sizes),
        date_format,
    )
}

fn pane(p: &PaneView, side: Side, listing_version: u64, date_format: CoreDateFormat) -> PaneSnapshot {
    let has_parent = p.order.first() == Some(&PARENT);
    let shown = p.order.len() - usize::from(has_parent);
    let cursor_entry = match p.order.get(p.cursor) {
        Some(&PARENT) | None => None,
        Some(&ix) => p.listing.entries.get(ix as usize),
    };
    let marked_count = p.marked_totals.dirs + p.marked_totals.files;
    let selection_count = if p.marked.is_empty() {
        u64::from(cursor_entry.is_some())
    } else {
        marked_count
    };
    PaneSnapshot {
        side,
        tab_id: p.tab_id.0,
        tab_titles: p.tab_titles.clone(),
        active_tab: usize32(p.active_tab),
        conn: p.conn.0,
        path: path_bytes(&p.path),
        elevated: p.elevated,
        caps: p.caps.into(),
        kind: p.kind.into(),
        location_label: p.location_label.clone(),
        host_local: p.host_local,
        display: p.display.clone(),
        crumbs: p
            .crumbs
            .iter()
            .map(|(name, path)| Crumb {
                name: name.clone(),
                path: path_bytes(path),
            })
            .collect(),
        loading: p.loading,
        error: p.error.clone(),
        disconnected: p.disconnected,
        can_reconnect: p.can_reconnect,
        row_count: usize32(p.order.len()),
        cursor: usize32(p.cursor),
        listing_version,
        filter: p.filter.clone(),
        shown: usize32(shown),
        unfiltered: usize32(p.unfiltered),
        sort_key: p.sort.key.into(),
        sort_descending: p.sort.desc,
        selection_count: u32::try_from(selection_count).unwrap_or(u32::MAX),
        marked_count: u32::try_from(marked_count).unwrap_or(u32::MAX),
        cursor_on_archive: cursor_entry.is_some_and(|e| is_archive(e) && p.kind != CoreKind::Archive),
        status: p.status.clone(),
        free: p.free.clone(),
        places: p
            .places
            .iter()
            .map(|pl| PlaceItem {
                id: pl.id.clone(),
                name: pl.name.clone(),
                path: path_bytes(&pl.path),
                display: pl.path.display(),
                current: pl.current,
            })
            .collect(),
        place_saved: p.place_saved,
        view_mode: p.view_mode.into(),
        columns: p.columns.iter().map(|c| column(c, date_format)).collect(),
        preview: p.preview.as_ref().map(|c| column(c, date_format)),
    }
}

fn column(c: &ColumnView, date_format: CoreDateFormat) -> ColumnSnapshot {
    ColumnSnapshot {
        path: path_bytes(&c.path),
        name: if c.path.is_root() { "/".into() } else { c.path.file_name_lossy() },
        row_count: usize32(c.order.len()),
        selected: c.selected.map(usize32),
        // Revisions are unique for good; the date format changes the text.
        version: c.revision * 4 + date_format as u64,
        loading: c.loading,
        error: c.error.clone(),
    }
}

fn job(j: &JobView) -> JobSnapshot {
    JobSnapshot {
        id: j.id.0,
        title: j.title.clone(),
        state: match j.status {
            JobStatus::Running => JobState::Running,
            JobStatus::Done(_) => JobState::Done,
            JobStatus::Failed(_) => JobState::Failed,
            JobStatus::Cancelled => JobState::Cancelled,
        },
        fraction: j.fraction,
        detail: j.detail.clone(),
        short: j.short.clone(),
        current: j.current.clone(),
    }
}

fn dialog_kind(kind: &DialogKind, view: &ViewModel) -> DialogKindSnapshot {
    match kind {
        DialogKind::Input(d) => DialogKindSnapshot::Input {
            dialog: InputDialogSnapshot {
                title: d.title.clone(),
                message: d.message.clone(),
                text: d.text.clone(),
                select: d.select.map(|(start, end)| Utf16Range::from_bytes(&d.text, start, end)),
                ok: d.ok.clone(),
            },
        },
        DialogKind::Confirm(d) => DialogKindSnapshot::Confirm {
            dialog: ConfirmSnapshot {
                title: d.title.clone(),
                message: d.message.clone(),
                ok: d.ok.clone(),
                danger: d.danger,
                from_provider: matches!(d.purpose, tl_app::dialog::ConfirmPurpose::Prompt { .. }),
            },
        },
        DialogKind::Message { title, message } => DialogKindSnapshot::Message {
            title: title.clone(),
            message: message.clone(),
        },
        DialogKind::Secret { title, message, text, .. } => DialogKindSnapshot::Secret {
            title: title.clone(),
            message: message.clone(),
            text: text.clone(),
        },
        DialogKind::Conflict { conflict, apply_all, .. } => DialogKindSnapshot::Conflict {
            dialog: conflict_snapshot(conflict, *apply_all),
        },
        DialogKind::FindResults { title, rows, .. } => DialogKindSnapshot::FindResults {
            title: title.clone(),
            count: usize32(rows.len()),
            version: 0,
        },
        DialogKind::About => DialogKindSnapshot::About {
            dialog: AboutSnapshot {
                title: "Translocator".into(),
                version: format!("Version {}", env!("CARGO_PKG_VERSION")),
                description: ABOUT_DESCRIPTION.into(),
                license: format!("MIT licensed · {}", tl_app::dialog::HELP_URL),
            },
        },
        DialogKind::Rename(d) => DialogKindSnapshot::Rename {
            dialog: rename::snapshot(d, 0),
        },
        DialogKind::Sync(d) => DialogKindSnapshot::Sync { dialog: sync::snapshot(d, 0) },
        DialogKind::Properties(d) => DialogKindSnapshot::Properties {
            dialog: properties::snapshot(d),
        },
        DialogKind::BookmarkEdit(d) => DialogKindSnapshot::BookmarkEdit {
            dialog: bookmarks::edit_snapshot(d, view),
        },
        DialogKind::Bookmarks(d) => DialogKindSnapshot::Bookmarks {
            dialog: bookmarks::manager_snapshot(d, view),
        },
    }
}

/// "file, 3.0 KB, modified today 14:02".
fn describe(entry: &Entry) -> String {
    let kind = if entry.is_dir() { "folder" } else { "file" };
    let size = if entry.is_dir() {
        String::new()
    } else {
        format!("{}, ", human_size(entry.size))
    };
    format!("{kind}, {size}modified {}", crate::format::date_long(entry.mtime))
}

fn conflict_snapshot(c: &Conflict, apply_all: bool) -> ConflictSnapshot {
    let name = c.source.name_lossy();
    let newer = match (c.source.mtime, c.existing.mtime) {
        (Some(a), Some(b)) if a > b => Newer::Incoming,
        (Some(a), Some(b)) if a < b => Newer::Existing,
        _ => Newer::Unknown,
    };
    let button = |label: &str, choice: CoreChoice, primary: bool| ConflictButton {
        label: label.into(),
        choice: match choice {
            CoreChoice::Overwrite => ConflictChoice::Overwrite,
            CoreChoice::OverwriteOlder => ConflictChoice::OverwriteOlder,
            CoreChoice::SkipExisting => ConflictChoice::SkipExisting,
            CoreChoice::Skip => ConflictChoice::Skip,
            CoreChoice::KeepBoth => ConflictChoice::KeepBoth,
            CoreChoice::Cancel => ConflictChoice::Cancel,
        },
        primary,
    };
    let choices = if c.both_dirs {
        vec![
            button("Cancel", CoreChoice::Cancel, false),
            button("Skip", CoreChoice::Skip, false),
            button("Keep Both", CoreChoice::KeepBoth, false),
            button("Merge, Keep Existing", CoreChoice::SkipExisting, false),
            button("Merge, Replace Older", CoreChoice::OverwriteOlder, false),
            button("Merge, Replace", CoreChoice::Overwrite, true),
        ]
    } else {
        vec![
            button("Cancel", CoreChoice::Cancel, false),
            button("Skip", CoreChoice::Skip, false),
            button("Keep Both", CoreChoice::KeepBoth, false),
            button("Replace If Older", CoreChoice::OverwriteOlder, false),
            button("Replace", CoreChoice::Overwrite, true),
        ]
    };
    ConflictSnapshot {
        title: if c.both_dirs {
            format!("Folder “{name}” already exists")
        } else {
            format!("“{name}” already exists")
        },
        location: format!("in {}", c.dst_dir),
        existing: describe(&c.existing),
        incoming_label: if c.both_dirs { "Incoming" } else { "Replace with" }.into(),
        incoming: describe(&c.source),
        newer,
        both_dirs: c.both_dirs,
        apply_all,
        choices,
    }
}

/// Search hits `start..start + count` of a find dialog.
pub(crate) fn find_rows(view: &ViewModel, dialog: u64, start: u32, count: u32) -> Vec<FoundRow> {
    let Some(rows) = view.dialogs.iter().find_map(|d| match &d.kind {
        DialogKind::FindResults { rows, .. } if d.id.0 == dialog => Some(rows),
        _ => None,
    }) else {
        return Vec::new();
    };
    let start = (start as usize).min(rows.len());
    let end = start.saturating_add(count as usize).min(rows.len());
    rows[start..end]
        .iter()
        .map(|f: &Found| FoundRow {
            rel: f.rel.clone(),
            is_dir: f.is_dir,
        })
        .collect()
}

