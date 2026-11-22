//! What the shell draws. Built after every update that asked for a render;
//! the big parts (listings, row orders, marks, plans) are shared `Arc`s, so
//! building it costs the size of the window's text, not of the folders.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tl_model::util::human_size;
use tl_model::{Bookmarks, Capabilities, PluginInfo, SpaceInfo, VPath};

use crate::capabilities::LocalFolder;
use crate::conn::tilde;
use crate::dialog::Dialog;
use crate::ids::{ConnId, JobId, TabId};
use crate::jobs::{Job, JobStatus};
use crate::model::{Console, FocusTarget, Model};
use crate::pane::{Listing, Side, Sort, Tab, Totals};
use crate::settings::Settings;
use crate::{editor, quick_look};

#[derive(Clone, Debug, Default)]
pub struct ViewModel {
    /// The panes are open (startup has finished).
    pub ready: bool,
    /// The main window's title.
    pub title: String,
    pub active: Side,
    pub panes: [PaneView; 2],
    /// The preview, drawn in place of the pane opposite `active`.
    pub quick_look: Option<quick_look::QuickLookView>,
    /// Modal dialogs, the last one on top.
    pub dialogs: Vec<Dialog>,
    pub jobs: Vec<JobView>,
    /// Running jobs that are listed.
    pub running: usize,
    pub jobs_open: bool,
    pub console: Console,
    /// Left of the command line: `local:~/x $`.
    pub prompt: String,
    pub prompt_elevated: bool,
    /// Bumped when the command line should be cleared.
    pub cmdline_clears: u64,
    pub settings: Arc<Settings>,
    pub bookmarks: Arc<Bookmarks>,
    pub summaries: Arc<HashMap<String, String>>,
    pub plugins: Arc<Vec<PluginInfo>>,
    /// Bookmarks with a live connection.
    pub connected: Vec<String>,
    pub local_folders: Arc<Vec<LocalFolder>>,
    pub editors: Vec<editor::EditorView>,
    pub settings_requests: u64,
    pub locations_request: Option<(Side, u64)>,
    pub focus_request: Option<(FocusTarget, u64)>,
    pub has_clipboard: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LocationKind {
    #[default]
    Local,
    Remote,
    Archive,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaceView {
    pub id: String,
    pub name: String,
    pub path: VPath,
    /// The pane is in this folder.
    pub current: bool,
}

#[derive(Clone, Debug, Default)]
pub struct PaneView {
    pub side: Side,
    pub tab_id: TabId,
    pub tab_titles: Vec<String>,
    pub active_tab: usize,
    pub conn: ConnId,
    pub path: VPath,
    pub elevated: bool,
    pub caps: Capabilities,
    pub kind: LocationKind,
    /// "Local" or the server's label.
    pub location_label: String,
    /// The file system holding this folder (or its archive) is this machine.
    pub host_local: bool,
    /// `~/Projects`, `deploy@web-1:/etc`.
    pub display: String,
    /// The path's components from the root, each with its full path.
    pub crumbs: Vec<(String, VPath)>,
    pub loading: bool,
    pub error: Option<String>,
    /// The connection behind the pane is gone.
    pub disconnected: bool,
    /// A bookmark to reconnect with.
    pub can_reconnect: bool,
    pub listing: Listing,
    pub order: Arc<Vec<u32>>,
    pub cursor: usize,
    pub marked: Arc<HashSet<Vec<u8>>>,
    pub dir_sizes: Arc<HashMap<Vec<u8>, u64>>,
    pub filter: String,
    /// Rows before the filter.
    pub unfiltered: usize,
    pub sort: Sort,
    pub totals: Totals,
    pub marked_totals: Totals,
    pub space: Option<SpaceInfo>,
    /// The left part of the status line.
    pub status: String,
    /// The right part: `12 GB free`.
    pub free: Option<String>,
    pub places: Vec<PlaceView>,
    /// This folder is on the row.
    pub place_saved: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JobView {
    pub id: JobId,
    pub title: String,
    pub status: JobStatus,
    /// `0.0..=1.0`, or `None` when the job cannot tell.
    pub fraction: Option<f32>,
    /// Phase, amounts, rate and time for a running job; the outcome after.
    pub detail: String,
    /// Strip detail: `1 GB / 3 GB`, `4 / 12` or the phase.
    pub short: String,
    /// The item being worked on.
    pub current: String,
}

impl JobView {
    pub fn is_running(&self) -> bool {
        self.status == JobStatus::Running
    }
}

pub fn view(model: &Model) -> ViewModel {
    let active = model.pane(model.active);
    let mut title = format!("{} — Translocator", model.conns.display(active.loc().conn, &active.loc().path));
    if active.elevated() {
        title.push_str(" (sudo)");
    }
    let home = model.conns.local().and_then(|c| c.local_home.clone());
    let loc = active.loc();
    let prompt = format!(
        "{}:{} {}",
        if model.conns.is_local(loc.conn) {
            "local".to_string()
        } else {
            model.conns.label(loc.conn)
        },
        tilde(&loc.path, home.as_deref()),
        if active.elevated() { "#" } else { "$" }
    );
    let jobs: Vec<JobView> = model.jobs.visible().map(job_view).collect();
    ViewModel {
        ready: model.ready,
        title,
        active: model.active,
        panes: [pane_view(model, Side::Left), pane_view(model, Side::Right)],
        quick_look: quick_look::view(model),
        dialogs: model.dialogs.clone(),
        running: model.jobs.running(),
        jobs,
        jobs_open: model.jobs_open,
        console: model.console.clone(),
        prompt,
        prompt_elevated: active.elevated(),
        cmdline_clears: model.cmdline_clears,
        settings: model.settings.clone(),
        bookmarks: model.bookmarks.clone(),
        summaries: model.summaries.clone(),
        plugins: model.plugins.clone(),
        connected: model
            .bookmarks
            .bookmarks
            .iter()
            .filter(|b| model.conns.of_bookmark(&b.id).is_some())
            .map(|b| b.id.clone())
            .collect(),
        local_folders: model.local_folders.clone(),
        editors: editor::views(model),
        settings_requests: model.settings_requests,
        locations_request: model.locations_request,
        focus_request: model.focus_request,
        has_clipboard: model.clipboard.is_some(),
    }
}

fn pane_view(model: &Model, side: Side) -> PaneView {
    let pane = model.pane(side);
    let tab = pane.tab();
    let conns = &model.conns;
    let conn = tab.loc.conn;
    let host = conns.host_of(conn);
    let info = conns.get(conn);
    let kind = match info {
        Some(c) if c.is_archive() => LocationKind::Archive,
        Some(c) if c.is_local() => LocationKind::Local,
        _ => LocationKind::Remote,
    };
    let location_label = if conns.is_local(host) { "Local".to_string() } else { conns.label(host) };
    let mut crumbs = Vec::new();
    let mut at = VPath::root();
    for part in tab.loc.path.components() {
        at = at.child(part);
        crumbs.push((String::from_utf8_lossy(part).into_owned(), at.clone()));
    }
    let host_key = conns.location_key(host);
    let in_archive = kind == LocationKind::Archive;
    let places: Vec<PlaceView> = model
        .bookmarks
        .places
        .iter()
        .filter(|p| p.host == host_key)
        .map(|p| PlaceView {
            id: p.id.clone(),
            name: p.name.clone(),
            path: p.path.clone(),
            current: !in_archive && p.path == tab.loc.path,
        })
        .collect();
    let place_saved = places.iter().any(|p| p.current);
    let disconnected = !conns.is_connected(host) || (conn != host && !conns.is_connected(conn));
    let can_reconnect = conns.get(host).and_then(|c| c.bookmark.as_ref()).is_some();
    PaneView {
        side,
        tab_id: tab.id,
        tab_titles: pane.tabs.iter().map(|t| tab_title(model, t)).collect(),
        active_tab: pane.active,
        conn,
        path: tab.loc.path.clone(),
        elevated: tab.elevated,
        caps: conns.caps(conn),
        kind,
        location_label,
        host_local: conns.is_local(host),
        display: conns.display(conn, &tab.loc.path),
        crumbs,
        loading: tab.loading,
        error: tab.error.clone(),
        disconnected,
        can_reconnect,
        listing: tab.listing.clone(),
        order: tab.order.clone(),
        cursor: tab.cursor,
        marked: tab.marked.clone(),
        dir_sizes: tab.dir_sizes.clone(),
        filter: tab.filter.clone(),
        unfiltered: tab.unfiltered,
        sort: tab.sort,
        totals: tab.totals,
        marked_totals: tab.marked_totals,
        space: tab.space,
        status: status_line(tab),
        free: tab.space.map(|s| format!("{} free", human_size(s.available))),
        places,
        place_saved,
    }
}

/// The last path component; on a server, with its label in front.
fn tab_title(model: &Model, tab: &Tab) -> String {
    let host = model.conns.host_of(tab.loc.conn);
    let name = tab.loc.path.file_name_lossy();
    if model.conns.is_local(host) {
        name
    } else {
        format!("{} · {}", model.conns.label(host), name)
    }
}

fn counts(t: &Totals) -> String {
    format!("{} dirs, {} files, {}", t.dirs, t.files, human_size(t.bytes))
}

fn status_line(tab: &Tab) -> String {
    if tab.marked.is_empty() {
        let all = counts(&tab.totals);
        match tab.cursor_entry() {
            Some(entry) => {
                let size = if entry.is_dir() {
                    match tab.dir_sizes.get(&entry.name) {
                        Some(size) => human_size(*size),
                        None => "folder".into(),
                    }
                } else {
                    human_size(entry.size)
                };
                let owner = match (&entry.user, &entry.group) {
                    (Some(u), Some(g)) => format!(" · {u}:{g}"),
                    (Some(u), None) => format!(" · {u}"),
                    _ => String::new(),
                };
                format!("{} · {size}{owner} · {all}", entry.name_lossy())
            }
            None => all,
        }
    } else {
        let m = &tab.marked_totals;
        format!(
            "{} of {} marked · {}",
            m.dirs + m.files,
            tab.totals.dirs + tab.totals.files,
            human_size(m.bytes)
        )
    }
}

fn job_view(job: &Job) -> JobView {
    let snap = &job.progress;
    let detail = match &job.status {
        JobStatus::Running => {
            let mut parts = Vec::new();
            if !snap.phase.is_empty() {
                parts.push(snap.phase.clone());
            }
            if snap.bytes_total > 0 {
                parts.push(format!("{} of {}", human_size(snap.bytes_done), human_size(snap.bytes_total)));
            } else if snap.items_total > 0 {
                parts.push(format!("{} of {} items", snap.items_done, snap.items_total));
            }
            if job.rate > 1024.0 {
                parts.push(format!("{}/s", human_size(job.rate as u64)));
            }
            let elapsed = job.elapsed_ms / 1000;
            if elapsed >= 2 {
                parts.push(format!("{}:{:02}", elapsed / 60, elapsed % 60));
            }
            parts.join(" · ")
        }
        JobStatus::Done(msg) | JobStatus::Failed(msg) => msg.clone(),
        JobStatus::Cancelled => "Cancelled".into(),
    };
    let short = if snap.bytes_total > 0 {
        format!("{} / {}", human_size(snap.bytes_done), human_size(snap.bytes_total))
    } else if snap.items_total > 0 {
        format!("{} / {}", snap.items_done, snap.items_total)
    } else {
        snap.phase.clone()
    };
    JobView {
        id: job.id,
        title: job.title.clone(),
        status: job.status.clone(),
        fraction: snap.fraction(),
        detail,
        short,
        current: snap.current.clone(),
    }
}
