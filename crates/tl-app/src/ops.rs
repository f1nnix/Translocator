//! What spans both panes: copying and moving between them, file operations
//! with their sudo retries, archives, connections, the command line, and
//! startup.

use std::sync::Arc;

use crux_core::Command;
use tl_model::archive::ArchiveFormat;
use tl_model::compare::compare_listings;
use tl_model::entry::split_extension;
use tl_model::multirename::Rename;
use tl_model::sync::SyncPlan;
use tl_model::transfer::TransferMode;
use tl_model::util::{human_size, search_masks};
use tl_model::{Bookmark, Entry, EntryKind, VPath, VfsError, VfsResult};

use crate::capabilities::*;
use crate::conn::{ConnInfo, tilde};
use crate::dialog::{self, ConfirmPurpose, DialogKind, Found, InputPurpose};
use crate::ids::{ConnId, LOCAL, TabId};
use crate::jobs::{self, Job, Spec};
use crate::model::{Command as Cmd, FocusTarget, Model, Reply, SettingsEdit, shell};
use crate::pane::{self, Dragged, Loc, Selection, Side, Sort, Tab, describe_names, is_archive};
use crate::settings::Settings;
use crate::{Out, bookmarks, editor, properties, quick_look, rename, sync};

/// How far startup has got.
#[derive(Clone, Debug, Default)]
pub struct Boot {
    settings: bool,
    plugins: bool,
    /// Saved folders checked so far, per side (`None` = home).
    checked: [Option<Option<VPath>>; 2],
}

/// What runs when a job of this module ends.
#[derive(Clone, Debug, PartialEq)]
pub enum After {
    Transfer { req: Box<TransferReq> },
    OpenedCopy,
    Deleted { key: String, dir: VPath },
    /// A folder was made or something renamed: put the cursor on `name`.
    Select { key: String, dir: VPath, name: Vec<u8> },
    FileCreated { key: String, dir: VPath, name: Vec<u8>, conn: ConnId, path: VPath },
    Attributes { key: String, dir: VPath },
    Sizes { side: Side, tab: TabId, count: usize },
    Found { side: Side, conn: ConnId, root: VPath, query: String },
    Exec { key: String, dir: VPath },
    Connected { side: Side, name: String, sudo: bool, bookmark: String },
    Synced,
    MultiRenamed { key: String, dir: VPath, select: Vec<u8> },
    ExtractDirs { sel: Selection, archives: Vec<Entry>, dst: Loc, dst_elevated: bool, target: VPath },
    PackCreated { sel: Selection, dst: Loc, dst_elevated: bool, file: VPath },
}

// A message that lives for one update; boxing would only move the bytes.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum OpsReply {
    Restored { side: Side, path: VPath, out: FsOutput },
    TransferTarget {
        mode: TransferMode,
        sel: Selection,
        dst: Loc,
        dst_elevated: bool,
        target: VPath,
        out: FsOutput,
    },
    ExtractReady {
        sel: Selection,
        dst: Loc,
        dst_elevated: bool,
        target: VPath,
        file: VPath,
        opened: FsOutput,
        listed: Option<FsOutput>,
    },
    PackTarget { sel: Selection, dst: Loc, dst_elevated: bool, file: VPath, format: ArchiveFormat, out: FsOutput },
    PackOpened { sel: Selection, dst: Loc, dst_elevated: bool, file: VPath, out: FsOutput },
    ExistingConnection { side: Side, sudo: bool, conn: ConnId, out: FsOutput },
    Terminal(PlatformOutput),
}

// ----- startup ----------------------------------------------------------------------

pub fn start(model: &mut Model, seed: u64, out: &mut Out) {
    model.ids.reseed(seed);
    out.push(Command::request_from_shell(StoreOp::Load).then_send(|loaded| shell(Reply::Loaded(loaded))));
    out.push(Command::request_from_shell(PluginOp::List).then_send(|plugins| shell(Reply::Plugins(plugins))));
    out.push(Command::stream_from_shell(PromptOp::Subscribe).then_send(|prompt| shell(Reply::Prompt(prompt))));
    out.push(Command::stream_from_shell(HealthOp::Subscribe).then_send(|health| shell(Reply::Health(health))));
    refresh_local_folders(out);
}

pub fn loaded(model: &mut Model, loaded: StoreOutput, out: &mut Out) {
    let StoreOutput::Loaded { settings, bookmarks } = loaded else { return };
    model.settings = Arc::new(*settings);
    model.bookmarks = Arc::new(bookmarks);
    model.boot.settings = true;
    bookmarks::refresh_summaries(model, out);
    boot(model, out);
}

pub fn plugins(model: &mut Model, plugins: PluginOutput, out: &mut Out) {
    let PluginOutput::Plugins { plugins, local } = plugins else { return };
    model.plugins = Arc::new(plugins);
    model.conns.insert(local);
    model.boot.plugins = true;
    bookmarks::refresh_summaries(model, out);
    boot(model, out);
}

/// Once settings and plugins are in: check the folders the panes were last
/// in, then open the panes there (or at home).
fn boot(model: &mut Model, out: &mut Out) {
    if !model.boot.settings || !model.boot.plugins || model.ready {
        return;
    }
    let session = model.settings.session.clone();
    let restore = model.settings.general.restore_folders;
    for (side, saved) in [(Side::Left, session.left_path), (Side::Right, session.right_path)] {
        match saved.filter(|_| restore) {
            Some(path) => {
                let path = VPath::parse(&path);
                let op = FsOp::Stat {
                    conn: LOCAL,
                    path: path.clone(),
                    elevated: false,
                };
                out.fs(op, move |out| Reply::Ops(OpsReply::Restored { side, path, out }));
            }
            None => model.boot.checked[side.index()] = Some(None),
        }
    }
    open_panes(model, out);
}

fn open_panes(model: &mut Model, out: &mut Out) {
    let [Some(left), Some(right)] = model.boot.checked.clone() else { return };
    let home = model.conns.local().map(|c| c.home.clone()).unwrap_or_else(VPath::root);
    let sort = Sort {
        key: model.settings.general.sort,
        desc: model.settings.general.sort_descending,
    };
    for (side, path) in [(Side::Left, left), (Side::Right, right)] {
        let loc = Loc {
            conn: LOCAL,
            path: path.unwrap_or_else(|| home.clone()),
        };
        let tab = Tab::new(&mut model.ids, loc, false, sort);
        model.pane_mut(side).tabs = vec![tab];
        model.pane_mut(side).active = 0;
        pane::load(model, side, 0, None, out);
    }
    model.active = Side::Left;
    model.ready = true;
    model.focus(FocusTarget::Pane(Side::Left));
}

fn restored(model: &mut Model, side: Side, path: VPath, result: FsOutput, out: &mut Out) {
    let ok = matches!(result, FsOutput::Stat(Ok(Some(ref e))) if e.is_dir() || e.target_kind == Some(EntryKind::Dir));
    model.boot.checked[side.index()] = Some(ok.then_some(path));
    open_panes(model, out);
}

pub fn health(model: &mut Model, health: Health, _out: &mut Out) {
    model.conns.set_connected(health.conn, health.connected);
}

pub fn refresh_local_folders(out: &mut Out) {
    out.push(Command::request_from_shell(PlatformOp::LocalFolders).then_send(|folders| shell(Reply::LocalFolders(folders))));
}

/// After any pane moved: remember local folders for the next start, and
/// forget archives no tab shows any more.
pub fn location_changed(model: &mut Model, out: &mut Out) {
    for side in Side::BOTH {
        let loc = model.pane(side).loc().clone();
        if model.conns.is_local(loc.conn) {
            let session = &mut Arc::make_mut(&mut model.settings).session;
            let path = Some(loc.path.display());
            match side {
                Side::Left if session.left_path != path => session.left_path = path,
                Side::Right if session.right_path != path => session.right_path = path,
                _ => {}
            }
        }
    }
    release_unused(model, out);
}

/// Archives opened as folders that no tab and no clipboard refers to.
pub fn release_unused(model: &mut Model, out: &mut Out) {
    let used: Vec<ConnId> = model
        .panes
        .iter()
        .flat_map(|p| p.tabs.iter())
        .flat_map(|t| std::iter::once(t.loc.conn).chain(t.back.iter().chain(t.forward.iter()).map(|(l, _)| l.conn)))
        .chain(model.clipboard.iter().map(|c| c.conn))
        .collect();
    let unused: Vec<ConnId> = model
        .conns
        .all()
        .iter()
        .filter(|c| c.is_archive() && !used.contains(&c.id))
        .map(|c| c.id)
        .collect();
    for conn in unused {
        model.conns.remove(conn);
        out.push(Command::notify_shell(FsOp::Release { conn }).into());
    }
}

// ----- settings --------------------------------------------------------------------------

pub fn edit_settings(model: &mut Model, edit: SettingsEdit, out: &mut Out) {
    let before = (model.settings.general.show_hidden, model.settings.general.folders_first);
    {
        let s: &mut Settings = Arc::make_mut(&mut model.settings);
        let g = &mut s.general;
        match edit {
            SettingsEdit::RestoreFolders(on) => g.restore_folders = on,
            SettingsEdit::ShowHidden(on) => g.show_hidden = on,
            SettingsEdit::FoldersFirst(on) => g.folders_first = on,
            SettingsEdit::Sort(key) => g.sort = key,
            SettingsEdit::SortDescending(on) => g.sort_descending = on,
            SettingsEdit::ShowIcons(on) => g.show_icons = on,
            SettingsEdit::ConfirmTrash(on) => g.confirm_trash = on,
            SettingsEdit::FontSize(size) => {
                g.font_size = size;
                g.font_size = g.font_size();
            }
            SettingsEdit::RowHeight(height) => {
                g.row_height = height;
                g.row_height = g.row_height();
            }
            SettingsEdit::DateFormat(format) => g.date_format = format,
            SettingsEdit::Terminal(text) => g.terminal = text.trim().to_string(),
            SettingsEdit::AppearanceMode(mode) => s.appearance.mode = mode,
            SettingsEdit::LightTheme(name) => s.appearance.light = name,
            SettingsEdit::DarkTheme(name) => s.appearance.dark = name,
            SettingsEdit::Keys { command, keys } => match keys {
                Some(keys) => {
                    s.keys.insert(command, keys);
                }
                None => {
                    s.keys.remove(&command);
                }
            },
            SettingsEdit::ResetGeneral => s.general = Default::default(),
        }
    }
    let after = (model.settings.general.show_hidden, model.settings.general.folders_first);
    if before != after {
        pane::settings_changed(model);
    }
    model.save_settings(out);
}

// ----- workspace commands ----------------------------------------------------------------

pub fn command(model: &mut Model, cmd: Cmd, out: &mut Out) {
    let side = model.active;
    match cmd {
        Cmd::ToggleHidden => {
            let show = !model.settings.general.show_hidden;
            edit_settings(model, SettingsEdit::ShowHidden(show), out);
        }
        Cmd::SwitchPane => {
            model.active = side.other();
            model.focus(FocusTarget::Pane(side.other()));
        }
        Cmd::FocusPane => model.focus(FocusTarget::Pane(side)),
        Cmd::FocusCommandLine => model.focus(FocusTarget::CommandLine),
        Cmd::SwapPanes => {
            let (l, le) = (model.pane(Side::Left).loc().clone(), model.pane(Side::Left).elevated());
            let (r, re) = (model.pane(Side::Right).loc().clone(), model.pane(Side::Right).elevated());
            pane::open_location(model, Side::Left, r, re, out);
            pane::open_location(model, Side::Right, l, le, out);
        }
        Cmd::MirrorPane => {
            let (loc, elevated) = (model.pane(side).loc().clone(), model.pane(side).elevated());
            pane::open_location(model, side.other(), loc, elevated, out);
        }
        Cmd::ToggleQuickLook => quick_look::toggle(model, out),
        Cmd::Locations(target) => {
            let target = target.unwrap_or(side);
            model.active = target;
            model.focus(FocusTarget::Pane(target));
            let n = model.locations_request.map(|(_, n)| n + 1).unwrap_or(1);
            model.locations_request = Some((target, n));
            refresh_local_folders(out);
        }
        Cmd::ViewFile => open_editor(model, true, out),
        Cmd::EditFile => open_editor(model, false, out),
        Cmd::NewFile => {
            let (loc, elevated) = (model.pane(side).loc().clone(), model.pane(side).elevated());
            dialog::ask_input(model, "New File", "Name:", "", None, "Create", InputPurpose::NewFile { loc, elevated });
        }
        Cmd::NewFolder => {
            let (loc, elevated) = (model.pane(side).loc().clone(), model.pane(side).elevated());
            dialog::ask_input(
                model,
                "New Folder",
                "Name (use / to create nested folders):",
                "",
                None,
                "Create",
                InputPurpose::NewFolder { loc, elevated },
            );
        }
        Cmd::CopyFiles => transfer_prompt(model, TransferMode::Copy),
        Cmd::MoveFiles => transfer_prompt(model, TransferMode::Move),
        Cmd::TrashFiles => delete(model, false, out),
        Cmd::DeleteFiles => delete(model, true, out),
        Cmd::RenameFile => rename_prompt(model),
        Cmd::Properties => {
            if let Some(sel) = model.pane(side).selection() {
                properties::open(model, sel, out);
            }
        }
        Cmd::CopyPath => {
            let pane = model.pane(side);
            let loc = pane.loc().clone();
            let paths: Vec<String> = match pane.selection() {
                Some(sel) => sel.paths().iter().map(|p| model.conns.full_path(loc.conn, p)).collect(),
                None => vec![model.conns.full_path(loc.conn, &loc.path)],
            };
            let text = paths.join("\n");
            out.platform(PlatformOp::WriteClipboard(text.clone()));
            out.info(format!("Copied {text}"));
        }
        Cmd::OpenTerminal => open_terminal(model, out),
        Cmd::OpenExternally => open_externally(model, out),
        Cmd::RevealInFinder => reveal_in_finder(model, out),
        Cmd::CalcSize => calc_size(model, out),
        Cmd::FindFiles => find_prompt(model),
        Cmd::CompareDirs => compare_dirs(model, out),
        Cmd::SyncDirs => {
            let end = |side: Side| {
                let p = model.pane(side);
                SyncEnd {
                    conn: p.loc().conn,
                    dir: p.loc().path.clone(),
                    elevated: p.elevated(),
                }
            };
            let (left, right) = (end(Side::Left), end(Side::Right));
            sync::open(model, left, right, out);
        }
        Cmd::MultiRename => {
            let pane = model.pane(side);
            let Some(sel) = pane.selection() else { return };
            if model.conns.get(sel.conn).is_some_and(ConnInfo::is_archive) {
                out.error("Files inside archives cannot be renamed. Extract them first.");
                return;
            }
            let existing = pane.tab().listed().map(|e| e.name.clone()).collect();
            rename::open(model, sel, existing, out);
        }
        Cmd::ExtractArchive => extract_prompt(model, false, out),
        Cmd::Decompress => extract_prompt(model, true, out),
        Cmd::PackFiles => pack_prompt(model, false, out),
        Cmd::Compress => pack_prompt(model, true, out),
        Cmd::ClipboardCopy => clip(model, false, out),
        Cmd::ClipboardCut => clip(model, true, out),
        Cmd::ClipboardPaste => paste(model, out),
        Cmd::ToggleSudo => toggle_sudo(model, out),
        Cmd::Disconnect => {
            let conn = model.conns.host_of(model.pane(side).loc().conn);
            disconnect(model, conn, out);
        }
        Cmd::BookmarkConnection => bookmark_connection(model, out),
        Cmd::ManageBookmarks => bookmarks::manage(model, side, out),
        Cmd::NewBookmark => bookmarks::edit(model, None, Some(side), out),
        Cmd::ToggleJobs => model.jobs_open = !model.jobs_open,
        Cmd::ToggleConsole => model.console.shown = !model.console.shown,
        Cmd::OpenSettings => model.settings_requests += 1,
        Cmd::About => {
            dialog::open(model, DialogKind::About);
        }
        Cmd::OpenHelp => out.platform(PlatformOp::OpenUrl(dialog::HELP_URL.into())),
        Cmd::Quit => {
            model.save_settings(out);
            out.push(Command::notify_shell(StoreOp::Flush).into());
            out.platform(PlatformOp::Quit);
        }
        _ => {}
    }
}

fn open_editor(model: &mut Model, readonly: bool, out: &mut Out) {
    let pane = model.pane(model.active);
    let Some(entry) = pane.tab().cursor_entry().filter(|e| !e.is_dir()) else { return };
    let (conn, path, elevated) = (pane.loc().conn, pane.loc().path.child(&entry.name), pane.elevated());
    editor::open(model, conn, path, elevated, readonly, out);
}

/// A path typed into a dialog: `~`, `~/x`, absolute, or relative to `base`.
fn resolve_input(model: &Model, conn: ConnId, base: &VPath, text: &str) -> VPath {
    let home = || model.conns.get(conn).map(|c| c.home.clone()).unwrap_or_else(VPath::root);
    if text == "~" {
        home()
    } else if let Some(rest) = text.strip_prefix("~/") {
        home().join(rest)
    } else {
        base.join(text)
    }
}

/// "Go to Folder" answered.
pub fn go_to(model: &mut Model, side: Side, text: &str, out: &mut Out) {
    let loc = model.pane(side).loc().clone();
    let target = resolve_input(model, loc.conn, &loc.path, text.trim());
    pane::navigate(model, side, target, None, out);
}

/// Shows `path` in a pane: opens its folder with the cursor on it.
pub fn reveal(model: &mut Model, side: Side, conn: ConnId, path: VPath, out: &mut Out) {
    let parent = path.parent().unwrap_or_else(VPath::root);
    let name = path.file_name().map(<[u8]>::to_vec);
    if model.pane(side).loc().conn != conn {
        let elevated = model.pane(side).elevated();
        pane::open_location(model, side, Loc { conn, path: parent.clone() }, elevated, out);
    }
    pane::navigate(model, side, parent, name, out);
    model.active = side;
    model.focus(FocusTarget::Pane(side));
}

pub fn offer_sudo_listing(model: &mut Model, tab: TabId, path: VPath, _out: &mut Out) {
    let Some((side, ix)) = pane::find_tab(model, tab) else { return };
    let label = model.conns.label(model.pane(side).tabs[ix].loc.conn);
    dialog::ask_confirm(
        model,
        "Permission denied",
        format!("You cannot read {path} as {label}. Open it with sudo?"),
        "Use sudo",
        false,
        ConfirmPurpose::SudoListing { tab },
    );
}

fn toggle_sudo(model: &mut Model, out: &mut Out) {
    let side = model.active;
    let (conn, elevated, tab) = {
        let p = model.pane(side);
        (p.loc().conn, p.elevated(), p.tab().id)
    };
    if !model.conns.caps(conn).elevation {
        out.error("This location does not support sudo.");
        return;
    }
    if elevated {
        pane::set_elevated(model, side, false, out);
    } else {
        pane::elevate(model, tab, out);
    }
}

// ----- copy & move ------------------------------------------------------------------------

/// How the data will travel, shown before a copy or move starts.
fn route_hint(model: &Model, src: ConnId, dst: ConnId, mode: TransferMode) -> String {
    let conns = &model.conns;
    let place = |host: ConnId| {
        if conns.is_local(host) {
            format!("on {}", conns.machine())
        } else {
            format!("on {} with its own tar/zip", conns.label(host))
        }
    };
    if let Some((host, file)) = conns.get(dst).and_then(|c| c.host.clone()) {
        return format!("Adds to {}; the archive is rewritten {}.", file.file_name_lossy(), place(host));
    }
    if let Some((host, file)) = conns.get(src).and_then(|c| c.host.clone()) {
        return format!("Extracts from {} {}.", file.file_name_lossy(), place(host));
    }
    let verb = match mode {
        TransferMode::Copy => "cp -a",
        TransferMode::Move => "mv",
    };
    if conns.location_key(src) == conns.location_key(dst) {
        if conns.is_local(src) {
            format!("Runs on {}.", conns.machine())
        } else {
            format!("Runs `{verb}` on {} — no data passes through {}.", conns.label(src), conns.machine())
        }
    } else {
        match (conns.is_local(src), conns.is_local(dst)) {
            (true, false) => format!("Uploads to {} through ssh (tar stream).", conns.label(dst)),
            (false, true) => format!("Downloads from {} through ssh (tar stream).", conns.label(src)),
            _ => format!("Streams {} → {} through {}.", conns.label(src), conns.label(dst), conns.machine()),
        }
    }
}

fn dir_text(path: &VPath) -> String {
    if path.is_root() { "/".to_string() } else { format!("{path}/") }
}

fn transfer_prompt(model: &mut Model, mode: TransferMode) {
    let side = model.active;
    let Some(sel) = model.pane(side).selection() else { return };
    let dst_pane = model.pane(side.other());
    let (dst, dst_elevated) = (dst_pane.loc().clone(), dst_pane.elevated());
    let verb = match mode {
        TransferMode::Copy => "Copy",
        TransferMode::Move => "Move",
    };
    let target = dir_text(&dst.path);
    let hint = route_hint(model, sel.conn, dst.conn, mode);
    let message = format!("{verb} {} to {}:\n{hint}", sel.describe(), model.conns.place_name(dst.conn));
    let end = target.len();
    dialog::ask_input(
        model,
        verb,
        message,
        target,
        Some((end, end)),
        verb,
        InputPurpose::Transfer { mode, sel, dst, dst_elevated },
    );
}

pub fn transfer_answered(
    model: &mut Model,
    mode: TransferMode,
    sel: Selection,
    dst: Loc,
    dst_elevated: bool,
    text: String,
    out: &mut Out,
) {
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }
    let target = resolve_input(model, dst.conn, &dst.path, &text);
    if text.ends_with('/') {
        start_transfer(model, transfer_req(mode, &sel, dst.conn, target, dst_elevated, None), out);
        return;
    }
    let op = FsOp::Stat {
        conn: dst.conn,
        path: target.clone(),
        elevated: dst_elevated,
    };
    out.fs(op, move |out| {
        Reply::Ops(OpsReply::TransferTarget {
            mode,
            sel,
            dst,
            dst_elevated,
            target,
            out,
        })
    });
}

fn transfer_req(
    mode: TransferMode,
    sel: &Selection,
    dst: ConnId,
    dst_dir: VPath,
    dst_elevated: bool,
    target_name: Option<Vec<u8>>,
) -> TransferReq {
    TransferReq {
        mode,
        src: sel.conn,
        src_dir: sel.dir.clone(),
        names: sel.names(),
        src_elevated: sel.elevated,
        dst,
        dst_dir,
        dst_elevated,
        target_name,
        conflicts: Conflicts::Ask,
    }
}

#[allow(clippy::too_many_arguments)]
fn transfer_target(
    model: &mut Model,
    mode: TransferMode,
    sel: Selection,
    dst: Loc,
    dst_elevated: bool,
    target: VPath,
    result: FsOutput,
    out: &mut Out,
) {
    let FsOutput::Stat(result) = result else { return };
    let single = sel.entries.len() == 1;
    match result {
        Ok(Some(e)) if e.is_dir() => start_transfer(model, transfer_req(mode, &sel, dst.conn, target, dst_elevated, None), out),
        Ok(_) if single => {
            let name = target.file_name().map(<[u8]>::to_vec);
            let dir = target.parent().unwrap_or_else(VPath::root);
            start_transfer(model, transfer_req(mode, &sel, dst.conn, dir, dst_elevated, name), out);
        }
        Ok(_) => out.error(VfsError::NotFound(format!("{target} is not a folder")).to_string()),
        Err(err) => out.error(err.to_string()),
    }
}

/// Files from the other pane dropped into `to`.
pub fn transfer_drop(model: &mut Model, drag: Dragged, to: Side, mode: TransferMode, out: &mut Out) {
    let dst = model.pane(to);
    let req = TransferReq {
        mode,
        src: drag.conn,
        src_dir: drag.dir,
        names: drag.names,
        src_elevated: drag.elevated,
        dst: dst.loc().conn,
        dst_dir: dst.loc().path.clone(),
        dst_elevated: dst.elevated(),
        target_name: None,
        conflicts: Conflicts::Ask,
    };
    start_transfer(model, req, out);
}

/// Files dropped from the desktop: copied into the pane's folder, one
/// transfer per folder they came from.
pub fn copy_external(model: &mut Model, side: Side, paths: Vec<VPath>, out: &mut Out) {
    let mut groups: Vec<(VPath, Vec<Vec<u8>>)> = Vec::new();
    for path in paths {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else { continue };
        match groups.iter_mut().find(|(dir, _)| *dir == parent) {
            Some((_, names)) => names.push(name.to_vec()),
            None => groups.push((parent, vec![name.to_vec()])),
        }
    }
    let dst = model.pane(side);
    let (dst_loc, dst_elevated) = (dst.loc().clone(), dst.elevated());
    for (dir, names) in groups {
        let req = TransferReq {
            mode: TransferMode::Copy,
            src: LOCAL,
            src_dir: dir,
            names,
            src_elevated: false,
            dst: dst_loc.conn,
            dst_dir: dst_loc.path.clone(),
            dst_elevated,
            target_name: None,
            conflicts: Conflicts::Ask,
        };
        start_transfer(model, req, out);
    }
}

pub fn start_transfer(model: &mut Model, req: TransferReq, out: &mut Out) {
    let verb = match req.mode {
        TransferMode::Copy => "Copy",
        TransferMode::Move => "Move",
    };
    let title = format!(
        "{verb} {} → {}",
        describe_names(&req.names),
        model.conns.display(req.dst, &req.dst_dir)
    );
    let after = jobs::After::Ops(After::Transfer { req: Box::new(req.clone()) });
    jobs::start(model, Spec::listed(title, Work::Transfer(req), after).no_retry(), out);
}

pub fn retry_transfer(model: &mut Model, mut req: TransferReq, out: &mut Out) {
    req.src_elevated = req.src_elevated || model.conns.caps(req.src).elevation;
    req.dst_elevated = req.dst_elevated || model.conns.caps(req.dst).elevation;
    start_transfer(model, req, out);
}

fn refresh_archive_host(model: &mut Model, conn: ConnId, out: &mut Out) {
    if let Some((host, file)) = model.conns.get(conn).and_then(|c| c.host.clone())
        && let Some(dir) = file.parent()
    {
        let key = model.conns.location_key(host);
        pane::refresh_dir(model, &key, &dir, out);
    }
}

fn transfer_done(model: &mut Model, job: &Job, req: TransferReq, result: VfsResult<JobOutput>, out: &mut Out) {
    let (src_key, dst_key) = (model.conns.location_key(req.src), model.conns.location_key(req.dst));
    pane::refresh_dir(model, &src_key, &req.src_dir, out);
    pane::refresh_dir(model, &dst_key, &req.dst_dir, out);
    // An archive that was written to changed size in its folder.
    refresh_archive_host(model, req.src, out);
    refresh_archive_host(model, req.dst, out);
    match result {
        Ok(JobOutput::Transfer(report)) => {
            let mut msg = match report.transferred {
                0 => "Nothing to do".to_string(),
                1 => "1 item".to_string(),
                n => format!("{n} items"),
            };
            if report.skipped > 0 {
                msg.push_str(&format!(", {} skipped", report.skipped));
            }
            if report.server_side && !model.conns.is_local(req.src) {
                msg.push_str(" · on server");
            }
            jobs::set_done_message(model, job.id, msg);
            if let Some(first) = report.names.first().cloned() {
                pane::select_in_panes(model, &dst_key, &req.dst_dir, first, out);
            }
        }
        Ok(_) | Err(VfsError::Cancelled) => {}
        Err(err) => {
            let can_src = !req.src_elevated && model.conns.caps(req.src).elevation;
            let can_dst = !req.dst_elevated && model.conns.caps(req.dst).elevation;
            if err.is_permission_denied() && (can_src || can_dst) {
                dialog::ask_confirm(
                    model,
                    "Permission denied",
                    format!("{err}\n\nRetry with sudo?"),
                    "Retry with sudo",
                    false,
                    ConfirmPurpose::RetryTransfer { req: Box::new(req) },
                );
            } else {
                dialog::error(model, "Transfer failed", err.to_string());
            }
        }
    }
}

// ----- clipboard ---------------------------------------------------------------------------

fn clip(model: &mut Model, cut: bool, out: &mut Out) {
    let Some(sel) = model.pane(model.active).selection() else { return };
    let text = sel.paths().iter().map(|p| model.conns.full_path(sel.conn, p)).collect::<Vec<_>>().join("\n");
    out.platform(PlatformOp::WriteClipboard(text));
    let count = sel.entries.len();
    model.clipboard = Some(crate::model::FileClip {
        conn: sel.conn,
        dir: sel.dir.clone(),
        names: sel.names(),
        elevated: sel.elevated,
        cut,
    });
    let verb = if cut { "Cut" } else { "Copied" };
    out.info(format!("{verb} {count} item(s). Paste with ⌘V in any pane."));
}

fn paste(model: &mut Model, out: &mut Out) {
    let Some(clip) = model.clipboard.clone() else { return };
    let dst = model.pane(model.active);
    let req = TransferReq {
        mode: if clip.cut { TransferMode::Move } else { TransferMode::Copy },
        src: clip.conn,
        src_dir: clip.dir.clone(),
        names: clip.names.clone(),
        src_elevated: clip.elevated,
        dst: dst.loc().conn,
        dst_dir: dst.loc().path.clone(),
        dst_elevated: dst.elevated(),
        target_name: None,
        conflicts: Conflicts::Ask,
    };
    if clip.cut {
        model.clipboard = None;
    }
    start_transfer(model, req, out);
}

// ----- delete -----------------------------------------------------------------------------------

fn delete(model: &mut Model, permanent: bool, out: &mut Out) {
    let Some(sel) = model.pane(model.active).selection() else { return };
    let trash = !permanent && model.conns.caps(sel.conn).trash;
    let mut message = sel
        .entries
        .iter()
        .take(6)
        .map(|e| format!("• {}", e.name_lossy()))
        .collect::<Vec<_>>()
        .join("\n");
    let more = sel.entries.len().saturating_sub(6);
    if more > 0 {
        message.push_str(&format!("\n… and {more} more"));
    }
    let place = if model.conns.is_local(sel.conn) {
        String::new()
    } else {
        format!(" on {}", model.conns.label(sel.conn))
    };
    if !trash {
        message.push_str("\n\nThis cannot be undone.");
        if sel.elevated {
            message.push_str(" Runs as root (sudo).");
        }
    }
    let title = if trash {
        format!("Move {} to Trash?", sel.describe())
    } else {
        format!("Delete {}{place} permanently?", sel.describe())
    };
    // Permanent deletion always asks: a setting that turns off the last
    // question before an unrecoverable delete is not one worth having.
    if trash && !model.settings.general.confirm_trash {
        delete_confirmed(model, sel, trash, out);
        return;
    }
    let ok = if trash { "Move to Trash" } else { "Delete" };
    dialog::ask_confirm(model, title, message, ok, !trash, ConfirmPurpose::Delete { sel, trash });
}

pub fn delete_confirmed(model: &mut Model, sel: Selection, trash: bool, out: &mut Out) {
    let title = format!("{} {}", if trash { "Trash" } else { "Delete" }, sel.describe());
    let key = model.conns.location_key(sel.conn);
    let work = Work::Delete {
        conn: sel.conn,
        paths: sel.paths(),
        trash,
        elevated: sel.elevated,
    };
    let after = jobs::After::Ops(After::Deleted { key, dir: sel.dir.clone() });
    jobs::start(model, Spec::listed(title, work, after), out);
}

// ----- create & rename ---------------------------------------------------------------------------

pub fn new_folder_answered(model: &mut Model, loc: Loc, elevated: bool, text: String, out: &mut Out) {
    let name = text.trim().to_string();
    if name.is_empty() {
        return;
    }
    let parts: Vec<Vec<u8>> = name.split('/').filter(|s| !s.is_empty()).map(|s| s.as_bytes().to_vec()).collect();
    let Some(first) = parts.first().cloned() else { return };
    let key = model.conns.location_key(loc.conn);
    let work = Work::MakeDirs {
        conn: loc.conn,
        base: loc.path.clone(),
        parts,
        elevated,
    };
    let after = jobs::After::Ops(After::Select { key, dir: loc.path, name: first });
    jobs::start(model, Spec::quiet(work, after), out);
}

pub fn new_file_answered(model: &mut Model, loc: Loc, elevated: bool, text: String, out: &mut Out) {
    let name = text.trim().to_string();
    if name.is_empty() || name.contains('/') {
        return;
    }
    let path = loc.path.child(name.as_bytes());
    let key = model.conns.location_key(loc.conn);
    let work = Work::CreateFile {
        conn: loc.conn,
        path: path.clone(),
        elevated,
    };
    let after = jobs::After::Ops(After::FileCreated {
        key,
        dir: loc.path,
        name: name.into_bytes(),
        conn: loc.conn,
        path,
    });
    jobs::start(model, Spec::quiet(work, after), out);
}

fn rename_prompt(model: &mut Model) {
    let pane = model.pane(model.active);
    let Some(entry) = pane.tab().cursor_entry().cloned() else { return };
    let (loc, elevated) = (pane.loc().clone(), pane.elevated());
    let old = entry.name_lossy();
    let select = dialog::stem_range(&old);
    dialog::ask_input(
        model,
        "Rename",
        "",
        old.clone(),
        Some(select),
        "Rename",
        InputPurpose::Rename {
            loc,
            elevated,
            name: entry.name,
            old,
        },
    );
}

pub fn rename_answered(model: &mut Model, loc: Loc, elevated: bool, name: Vec<u8>, old: String, text: String, out: &mut Out) {
    let new = text.trim().to_string();
    if new.is_empty() || new == old {
        return;
    }
    if new.contains('/') {
        out.error("A name cannot contain “/”. Use Move (F6) to move items.");
        return;
    }
    let key = model.conns.location_key(loc.conn);
    let work = Work::Rename {
        conn: loc.conn,
        from: loc.path.child(&name),
        to: loc.path.child(new.as_bytes()),
        elevated,
    };
    let after = jobs::After::Ops(After::Select {
        key,
        dir: loc.path,
        name: new.into_bytes(),
    });
    jobs::start(model, Spec::quiet(work, after), out);
}

// ----- open ------------------------------------------------------------------------------------------

/// Enter on a file: its application opens it (a server's file is fetched
/// into the cache first).
pub fn open_file(model: &mut Model, side: Side, entry: Entry, out: &mut Out) {
    let pane = model.pane(side);
    let (loc, elevated) = (pane.loc().clone(), pane.elevated());
    if model.conns.is_local(loc.conn) {
        out.platform(PlatformOp::Open(loc.path.child(&entry.name)));
        return;
    }
    let title = format!("Open “{}” from {}", entry.name_lossy(), model.conns.label(loc.conn));
    let work = Work::OpenCopy {
        conn: loc.conn,
        dir: loc.path,
        name: entry.name,
        elevated,
    };
    jobs::start(model, Spec::listed(title, work, jobs::After::Ops(After::OpenedCopy)).no_retry(), out);
}

fn open_externally(model: &mut Model, out: &mut Out) {
    let side = model.active;
    let pane = model.pane(side);
    let loc = pane.loc().clone();
    let local = model.conns.is_local(loc.conn);
    match pane.tab().cursor_entry().cloned() {
        Some(entry) if !entry.is_dir() => open_file(model, side, entry, out),
        Some(entry) if local => out.platform(PlatformOp::Open(loc.path.child(&entry.name))),
        _ if local => out.platform(PlatformOp::Open(loc.path)),
        _ => {}
    }
}

fn reveal_in_finder(model: &mut Model, out: &mut Out) {
    let pane = model.pane(model.active);
    let loc = pane.loc().clone();
    if let Some((host, file)) = model.conns.get(loc.conn).and_then(|c| c.host.clone()) {
        // Inside an archive: show the archive file.
        if model.conns.is_local(host) {
            out.platform(PlatformOp::Reveal(file));
        }
        return;
    }
    if !model.conns.is_local(loc.conn) {
        return;
    }
    let target = match pane.tab().cursor_entry() {
        Some(entry) => loc.path.child(&entry.name),
        None => loc.path,
    };
    out.platform(PlatformOp::Reveal(target));
}

fn open_terminal(model: &mut Model, out: &mut Out) {
    let pane = model.pane(model.active);
    let loc = pane.loc().clone();
    let elevated = pane.elevated();
    // Inside an archive: the folder that holds it.
    let (conn, path) = match model.conns.get(loc.conn).and_then(|c| c.host.clone()) {
        Some((host, file)) => (host, file.parent().unwrap_or_else(VPath::root)),
        None => (loc.conn, loc.path),
    };
    let op = PlatformOp::OpenTerminal {
        conn,
        path,
        elevated,
        preferred: model.settings.general.terminal.clone(),
    };
    out.push(Command::request_from_shell(op).then_send(|done| shell(Reply::Ops(OpsReply::Terminal(done)))));
}

// ----- size, find, compare -------------------------------------------------------------------------

fn calc_size(model: &mut Model, out: &mut Out) {
    let side = model.active;
    let Some(sel) = model.pane(side).selection() else { return };
    let names: Vec<Vec<u8>> = sel.entries.iter().filter(|e| e.is_dir()).map(|e| e.name.clone()).collect();
    if names.is_empty() {
        return;
    }
    let tab = model.pane(side).tab().id;
    let title = format!("Calculate size of {}", sel.describe());
    let work = Work::Scan {
        conn: sel.conn,
        dir: sel.dir.clone(),
        names: names.clone(),
        elevated: sel.elevated,
    };
    let after = jobs::After::Ops(After::Sizes { side, tab, count: names.len() });
    jobs::start(model, Spec::listed(title, work, after), out);
}

fn find_prompt(model: &mut Model) {
    let side = model.active;
    let pane = model.pane(side);
    let (loc, elevated) = (pane.loc().clone(), pane.elevated());
    let place = model.conns.display(loc.conn, &loc.path);
    let remote = !model.conns.is_local(model.conns.host_of(loc.conn));
    let message = format!(
        "Search below {place}. Use wildcards like *.conf, or plain text to match anywhere in a name.{}",
        if remote { " Runs `find` on the server." } else { "" }
    );
    dialog::ask_input(model, "Find Files", message, "", None, "Find", InputPurpose::Find { side, loc, elevated, place });
}

pub fn find_answered(model: &mut Model, side: Side, loc: Loc, elevated: bool, place: String, text: String, out: &mut Out) {
    let query = text.trim().to_string();
    if query.is_empty() {
        return;
    }
    let title = format!("Find “{query}” in {place}");
    let work = Work::Find {
        conn: loc.conn,
        root: loc.path.clone(),
        masks: search_masks(&query),
        limit: 5000,
        elevated,
    };
    let after = jobs::After::Ops(After::Found {
        side,
        conn: loc.conn,
        root: loc.path,
        query,
    });
    jobs::start(model, Spec::listed(title, work, after), out);
}

fn compare_dirs(model: &mut Model, out: &mut Out) {
    let (left, right) = (model.pane(Side::Left), model.pane(Side::Right));
    if left.is_loading() || right.is_loading() {
        out.error("Wait until both panes have loaded.");
        return;
    }
    let ci = model.conns.caps(left.loc().conn).case_insensitive && model.conns.caps(right.loc().conn).case_insensitive;
    let l: Vec<Entry> = left.tab().listed().cloned().collect();
    let r: Vec<Entry> = right.tab().listed().cloned().collect();
    let result = compare_listings(&l, &r, ci);
    let stats = result.stats;
    pane::set_marks(model, Side::Left, result.left);
    pane::set_marks(model, Side::Right, result.right);
    let message = if stats.is_identical() {
        format!("No differences: {} items match.", stats.same)
    } else {
        let mut parts = vec![
            format!("Left: {} only there, {} newer", stats.only_left, stats.newer_left),
            format!("Right: {} only there, {} newer", stats.only_right, stats.newer_right),
        ];
        if stats.different > 0 {
            parts.push(format!("{} differ in size only (marked on both sides)", stats.different));
        }
        format!("{}. Marked items can be copied with F5.", parts.join(" · "))
    };
    out.info(message);
}

/// Runs a reviewed sync plan as a job.
pub fn start_sync(model: &mut Model, plan: SyncPlan, left: SyncEnd, right: SyncEnd, out: &mut Out) {
    let title = format!(
        "Synchronize {} ⇄ {}",
        model.conns.display(left.conn, &left.dir),
        model.conns.display(right.conn, &right.dir)
    );
    let work = Work::SyncApply { plan, left, right };
    jobs::start(model, Spec::listed(title, work, jobs::After::Ops(After::Synced)).no_retry(), out);
}

/// Runs the renames planned by the multi-rename dialog.
pub fn apply_renames(model: &mut Model, sel: Selection, steps: Vec<Rename>, count: usize, select: Vec<u8>, out: &mut Out) {
    let key = model.conns.location_key(sel.conn);
    let title = format!("Rename {count} item{}", if count == 1 { "" } else { "s" });
    let work = Work::Renames {
        conn: sel.conn,
        dir: sel.dir.clone(),
        steps,
        elevated: sel.elevated,
    };
    let after = jobs::After::Ops(After::MultiRenamed { key, dir: sel.dir, select });
    jobs::start(model, Spec::listed(title, work, after), out);
}

/// Applies permissions and ownership from the properties dialog.
pub fn apply_attributes(
    model: &mut Model,
    sel: Selection,
    mode: Option<u32>,
    owner: Option<String>,
    group: Option<String>,
    recursive: bool,
    out: &mut Out,
) {
    let key = model.conns.location_key(sel.conn);
    let title = format!("Change attributes of {}", sel.describe());
    let work = Work::Attributes {
        conn: sel.conn,
        paths: sel.paths(),
        mode,
        owner,
        group,
        recursive,
        elevated: sel.elevated,
    };
    let after = jobs::After::Ops(After::Attributes { key, dir: sel.dir });
    jobs::start(model, Spec::listed(title, work, after), out);
}

// ----- archives ------------------------------------------------------------------------------------------

fn extract_prompt(model: &mut Model, here: bool, out: &mut Out) {
    let side = model.active;
    let Some(sel) = model.pane(side).selection() else { return };
    let archives: Vec<Entry> = sel.entries.iter().filter(|e| is_archive(e)).cloned().collect();
    if archives.is_empty() || model.conns.get(sel.conn).is_some_and(ConnInfo::is_archive) {
        out.error("Select one or more .zip, .tar or .tar.gz files to extract.");
        return;
    }
    let dst_side = if here { side } else { side.other() };
    let (dst, dst_elevated) = (model.pane(dst_side).loc().clone(), model.pane(dst_side).elevated());
    let what = match archives.as_slice() {
        [one] => format!("“{}”", one.name_lossy()),
        many => format!("{} archives", many.len()),
    };
    let dir = dir_text(&dst.path);
    // Unpacking beside the archive would strew its contents over the folder
    // it is in, so a single archive gets a folder of its own as the proposal.
    let (target, select) = match (here, archives.as_slice()) {
        (true, [one]) => {
            let stem = String::from_utf8_lossy(split_extension(&one.name).0).into_owned();
            let stem = stem.strip_suffix(".tar").unwrap_or(&stem).to_string();
            let range = (dir.len(), dir.len() + stem.len());
            (format!("{dir}{stem}"), range)
        }
        _ => {
            let end = dir.len();
            (dir, (end, end))
        }
    };
    let message = format!(
        "Extract {what} into this folder on {} (created if missing):",
        model.conns.place_name(dst.conn)
    );
    let title = if here { "Decompress" } else { "Extract" };
    dialog::ask_input(
        model,
        title,
        message,
        target,
        Some(select),
        title,
        InputPurpose::Extract { sel, archives, dst, dst_elevated },
    );
}

pub fn extract_answered(
    model: &mut Model,
    sel: Selection,
    archives: Vec<Entry>,
    dst: Loc,
    dst_elevated: bool,
    text: String,
    out: &mut Out,
) {
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }
    let target = resolve_input(model, dst.conn, &dst.path, &text);
    let parts: Vec<Vec<u8>> = target.components().map(<[u8]>::to_vec).collect();
    let work = Work::MakeDirs {
        conn: dst.conn,
        base: VPath::root(),
        parts,
        elevated: dst_elevated,
    };
    let after = jobs::After::Ops(After::ExtractDirs {
        sel,
        archives,
        dst,
        dst_elevated,
        target,
    });
    jobs::start(model, Spec::quiet(work, after).no_retry(), out);
}

fn extract_dirs_made(model: &mut Model, sel: Selection, archives: Vec<Entry>, dst: Loc, dst_elevated: bool, target: VPath, out: &mut Out) {
    for entry in archives {
        let file = sel.dir.child(&entry.name);
        let new = model.ids.conn();
        let (sel, dst, target) = (sel.clone(), dst.clone(), target.clone());
        let elevated = sel.elevated;
        out.push(Command::new(move |ctx| async move {
            let opened = ctx
                .request_from_shell(FsOp::OpenArchive {
                    conn: sel.conn,
                    file: file.clone(),
                    elevated,
                    new,
                })
                .await;
            let listed = match &opened {
                FsOutput::Opened(Ok(info)) => Some(
                    ctx.request_from_shell(FsOp::List {
                        conn: info.id,
                        path: file.clone(),
                        elevated,
                    })
                    .await,
                ),
                _ => None,
            };
            ctx.send_event(shell(Reply::Ops(OpsReply::ExtractReady {
                sel,
                dst,
                dst_elevated,
                target,
                file,
                opened,
                listed,
            })));
        }));
    }
}

#[allow(clippy::too_many_arguments)]
fn extract_ready(
    model: &mut Model,
    sel: Selection,
    dst: Loc,
    dst_elevated: bool,
    target: VPath,
    file: VPath,
    opened: FsOutput,
    listed: Option<FsOutput>,
    out: &mut Out,
) {
    let info = match opened {
        FsOutput::Opened(Ok(info)) => info,
        FsOutput::Opened(Err(err)) => return out.error(err.to_string()),
        _ => return,
    };
    let conn = info.id;
    model.conns.insert(info);
    let names: Vec<Vec<u8>> = match listed {
        Some(FsOutput::Listing { entries: Ok(entries), .. }) => entries.into_iter().map(|e| e.name).collect(),
        Some(FsOutput::Listing { entries: Err(err), .. }) => return out.error(err.to_string()),
        _ => Vec::new(),
    };
    if names.is_empty() {
        return;
    }
    let req = TransferReq {
        mode: TransferMode::Copy,
        src: conn,
        src_dir: file,
        names,
        src_elevated: sel.elevated,
        dst: dst.conn,
        dst_dir: target,
        dst_elevated,
        target_name: None,
        conflicts: Conflicts::Ask,
    };
    start_transfer(model, req, out);
}

fn pack_prompt(model: &mut Model, here: bool, out: &mut Out) {
    let side = model.active;
    let Some(sel) = model.pane(side).selection() else { return };
    let dst_side = if here { side } else { side.other() };
    let (dst, dst_elevated) = (model.pane(dst_side).loc().clone(), model.pane(dst_side).elevated());
    if model.conns.get(dst.conn).is_some_and(ConnInfo::is_archive) {
        out.error(if here {
            "This pane is inside an archive. Copy (F5) adds files to it."
        } else {
            "The other pane is inside an archive. Copy (F5) adds files to it."
        });
        return;
    }
    let stem = match sel.entries.as_slice() {
        [one] if one.is_dir() => one.name_lossy(),
        [one] => String::from_utf8_lossy(split_extension(&one.name).0).into_owned(),
        _ => sel.dir.file_name().map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_else(|| "archive".into()),
    };
    let prefix = dir_text(&dst.path);
    let initial = format!("{prefix}{stem}.tar.gz");
    let select = (prefix.len(), prefix.len() + stem.len());
    let message = format!(
        "Pack {} into an archive on {}. The extension picks the format: .tar.gz, .tgz, .tar or .zip. An existing archive gets the files added.",
        sel.describe(),
        model.conns.place_name(dst.conn)
    );
    let title = if here { "Compress" } else { "Pack" };
    dialog::ask_input(model, title, message, initial, Some(select), title, InputPurpose::Pack { sel, dst, dst_elevated });
}

pub fn pack_answered(model: &mut Model, sel: Selection, dst: Loc, dst_elevated: bool, text: String, out: &mut Out) {
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }
    let file = resolve_input(model, dst.conn, &dst.path, &text);
    let name = file.file_name().unwrap_or_default().to_vec();
    let Some(format) = ArchiveFormat::detect(&name) else {
        out.error(format!(
            "“{}” does not end in .tar.gz, .tgz, .tar or .zip",
            String::from_utf8_lossy(&name)
        ));
        return;
    };
    let op = FsOp::Stat {
        conn: dst.conn,
        path: file.clone(),
        elevated: dst_elevated,
    };
    out.fs(op, move |out| {
        Reply::Ops(OpsReply::PackTarget {
            sel,
            dst,
            dst_elevated,
            file,
            format,
            out,
        })
    });
}

fn pack_open(model: &mut Model, sel: Selection, dst: Loc, dst_elevated: bool, file: VPath, out: &mut Out) {
    let new = model.ids.conn();
    let op = FsOp::OpenArchive {
        conn: dst.conn,
        file: file.clone(),
        elevated: dst_elevated,
        new,
    };
    out.fs(op, move |out| Reply::Ops(OpsReply::PackOpened { sel, dst, dst_elevated, file, out }));
}

// ----- connections -----------------------------------------------------------------------------------------

pub fn open_bookmark(model: &mut Model, id: String, side: Side, out: &mut Out) {
    let Some(bookmark) = model.bookmarks.get(&id).cloned() else { return };
    if !model.plugins.iter().any(|p| p.id == bookmark.plugin) {
        out.error(format!("Unknown connection type “{}”", bookmark.plugin));
        return;
    }
    let sudo = bookmark.flag("sudo");
    model.active = side;
    if let Some(conn) = model.conns.of_bookmark(&id).map(|c| c.id) {
        let op = FsOp::InitialPath { conn, bookmark };
        out.fs(op, move |out| Reply::Ops(OpsReply::ExistingConnection { side, sudo, conn, out }));
        return;
    }
    if bookmark.plugin == crate::conn::LOCAL_PLUGIN {
        // The local disk is always connected.
        let op = FsOp::InitialPath { conn: LOCAL, bookmark };
        out.fs(op, move |out| Reply::Ops(OpsReply::ExistingConnection { side, sudo, conn: LOCAL, out }));
        return;
    }
    let conn = model.ids.conn();
    let name = bookmark.name.clone();
    let title = format!("Connect to {name}");
    let work = Work::Connect { conn, bookmark, sudo };
    let after = jobs::After::Ops(After::Connected { side, name, sudo, bookmark: id });
    jobs::start(model, Spec::listed(title, work, after).no_retry(), out);
}

pub fn disconnect(model: &mut Model, conn: ConnId, out: &mut Out) {
    if conn == LOCAL || model.conns.is_local(conn) {
        return;
    }
    pane::drop_connection(model, conn, out);
    let hosted: Vec<ConnId> = model
        .conns
        .all()
        .iter()
        .filter(|c| c.id != conn && model.conns.host_of(c.id) == conn)
        .map(|c| c.id)
        .collect();
    for archive in hosted {
        model.conns.remove(archive);
        out.push(Command::notify_shell(FsOp::Release { conn: archive }).into());
    }
    if model.clipboard.as_ref().is_some_and(|c| model.conns.host_of(c.conn) == conn) {
        model.clipboard = None;
    }
    model.conns.remove(conn);
    out.push(Command::notify_shell(FsOp::Release { conn }).into());
}

/// A bookmark that opens straight in this folder, which is what reaches a
/// server's folder from cold; a place on the row needs the connection up.
fn bookmark_connection(model: &mut Model, out: &mut Out) {
    let loc = model.pane(model.active).loc().clone();
    let host = model.conns.host_of(loc.conn);
    let path = loc.path.clone();
    let id = model.ids.uuid();
    let template = if model.conns.is_local(host) {
        let mut b = Bookmark::with_id(id, path.file_name_lossy(), crate::conn::LOCAL_PLUGIN);
        let home = model.conns.get(host).and_then(|c| c.local_home.clone());
        b.set("path", tilde(&path, home.as_deref()));
        b
    } else {
        let origin = model
            .conns
            .get(host)
            .and_then(|c| c.bookmark.clone())
            .and_then(|b| model.bookmarks.get(&b).cloned());
        let Some(origin) = origin else { return };
        let mut b = origin.clone();
        b.id = id;
        b.name = format!("{} {}", origin.name, path.file_name_lossy());
        b.set("path", path.display());
        b
    };
    bookmarks::edit(model, Some(template), None, out);
}

// ----- command line ---------------------------------------------------------------------------------------------

pub fn run_command(model: &mut Model, text: String, out: &mut Out) {
    let command = text.trim().to_string();
    if command.is_empty() {
        return;
    }
    model.cmdline_clears += 1;
    let side = model.active;
    let pane = model.pane(side);
    let (loc, elevated) = (pane.loc().clone(), pane.elevated());
    // `cd` changes the pane's folder instead of running anything.
    if command == "cd" || command.starts_with("cd ") {
        let arg = command[2..].trim();
        let target = if arg.is_empty() {
            resolve_input(model, loc.conn, &loc.path, "~")
        } else {
            resolve_input(model, loc.conn, &loc.path, arg)
        };
        pane::navigate(model, side, target, None, out);
        return;
    }
    if !model.conns.caps(loc.conn).exec {
        out.error("This location cannot run commands.");
        return;
    }
    let home = model.conns.local().and_then(|c| c.local_home.clone());
    let prompt = format!(
        "{}{} {} $ {command}\n",
        if elevated { "[sudo] " } else { "" },
        model.conns.label(loc.conn),
        tilde(&loc.path, home.as_deref())
    );
    model.console.append(&prompt);
    model.console.shown = true;
    let key = model.conns.location_key(loc.conn);
    let work = Work::Exec {
        conn: loc.conn,
        cwd: loc.path.clone(),
        command: command.clone(),
        elevated,
    };
    let after = jobs::After::Ops(After::Exec { key, dir: loc.path });
    jobs::start(model, Spec::listed(format!("$ {command}"), work, after), out);
}

// ----- job follow-ups ---------------------------------------------------------------------------------------------

pub fn finished(model: &mut Model, job: &Job, then: After, result: VfsResult<JobOutput>, out: &mut Out) {
    let cancelled = matches!(result, Err(VfsError::Cancelled));
    match then {
        After::Transfer { req } => transfer_done(model, job, *req, result, out),
        After::OpenedCopy => match result {
            Ok(_) => jobs::set_done_message(model, job.id, "Opened a local copy"),
            Err(err) if !cancelled => out.error(err.to_string()),
            Err(_) => {}
        },
        After::Deleted { key, dir } | After::Attributes { key, dir } => {
            pane::refresh_dir(model, &key, &dir, out);
            if let Err(err) = result
                && !cancelled
            {
                out.error(err.to_string());
            }
        }
        After::Select { key, dir, name } => match result {
            Ok(_) => pane::select_in_panes(model, &key, &dir, name, out),
            Err(err) => out.error(err.to_string()),
        },
        After::FileCreated { key, dir, name, conn, path } => match result {
            Ok(_) => {
                pane::select_in_panes(model, &key, &dir, name, out);
                let elevated = job.work.elevation_target().is_some_and(|(_, e)| e);
                editor::open(model, conn, path, elevated, false, out);
            }
            Err(err) => out.error(err.to_string()),
        },
        After::Sizes { side, tab, count } => match result {
            Ok(JobOutput::Sizes(sizes)) => {
                let total: u64 = sizes.iter().map(|(_, t)| t.bytes).sum();
                let files: u64 = sizes.iter().map(|(_, t)| t.files).sum();
                pane::set_dir_sizes(model, side, tab, sizes.into_iter().map(|(n, t)| (n, t.bytes)).collect());
                if count > 1 {
                    out.info(format!("{} in {} files", human_size(total), files));
                }
            }
            Err(err) if !cancelled => out.error(err.to_string()),
            _ => {}
        },
        After::Found { side, conn, root, query } => match result {
            Ok(JobOutput::Found(hits)) => {
                if hits.is_empty() {
                    out.info(format!("Nothing matches “{query}”."));
                    return;
                }
                let rows: Vec<Found> = hits
                    .into_iter()
                    .map(|(path, kind)| Found {
                        rel: path
                            .relative_to(&root)
                            .map(|r| String::from_utf8_lossy(r).into_owned())
                            .unwrap_or_else(|| path.display()),
                        path,
                        is_dir: kind == EntryKind::Dir,
                    })
                    .collect();
                let title = format!("{} match{} for “{query}”", rows.len(), if rows.len() == 1 { "" } else { "es" });
                dialog::open(
                    model,
                    DialogKind::FindResults {
                        title,
                        side,
                        conn,
                        rows: Arc::new(rows),
                    },
                );
            }
            Err(err) if !cancelled => out.error(err.to_string()),
            _ => {}
        },
        After::Exec { key, dir } => {
            match result {
                Ok(JobOutput::Exec(output)) => {
                    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                    text.push_str(&String::from_utf8_lossy(&output.stderr));
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    if output.status != 0 {
                        text.push_str(&format!("[exit status {}]\n", output.status));
                    }
                    model.console.append(&text);
                }
                Ok(_) => {}
                Err(err) => model.console.append(&format!("{err}\n")),
            }
            pane::refresh_dir(model, &key, &dir, out);
        }
        After::Connected { side, name, sudo, bookmark } => match result {
            Ok(JobOutput::Connected { info, initial }) => {
                let message = format!("Connected · {}", info.server_info.clone().unwrap_or_default());
                jobs::set_done_message(model, job.id, message);
                // One live connection per bookmark.
                let old: Vec<ConnId> = model
                    .conns
                    .all()
                    .iter()
                    .filter(|c| c.host.is_none() && c.bookmark.as_deref() == Some(bookmark.as_str()) && c.id != info.id)
                    .map(|c| c.id)
                    .collect();
                for conn in old {
                    disconnect(model, conn, out);
                }
                let conn = info.id;
                model.conns.insert(info);
                pane::open_location(model, side, Loc { conn, path: initial }, sudo, out);
                model.active = side;
                model.focus(FocusTarget::Pane(side));
            }
            Err(err) if !cancelled => dialog::error(model, format!("Cannot connect to {name}"), err.to_string()),
            _ => {}
        },
        After::Synced => {
            for side in Side::BOTH {
                pane::reload_active(model, side, out);
            }
            match result {
                Ok(JobOutput::Synced(report)) => jobs::set_done_message(
                    model,
                    job.id,
                    format!(
                        "{} → right, {} ← left{}",
                        report.to_right,
                        report.to_left,
                        if report.server_side { " · on server" } else { "" }
                    ),
                ),
                Err(err) if !cancelled => dialog::error(model, "Synchronization failed", err.to_string()),
                _ => {}
            }
        }
        After::MultiRenamed { key, dir, select } => match result {
            Ok(_) => pane::select_in_panes(model, &key, &dir, select, out),
            Err(err) => {
                pane::refresh_dir(model, &key, &dir, out);
                if !cancelled {
                    let message = err.to_string();
                    let title = if message.contains("Undoing") {
                        "Rename failed"
                    } else {
                        "Rename failed; the folder was left as it was"
                    };
                    dialog::error(model, title, message);
                }
            }
        },
        After::ExtractDirs { sel, archives, dst, dst_elevated, target } => match result {
            Ok(_) => extract_dirs_made(model, sel, archives, dst, dst_elevated, target, out),
            Err(err) => out.error(err.to_string()),
        },
        After::PackCreated { sel, dst, dst_elevated, file } => match result {
            Ok(_) => pack_open(model, sel, dst, dst_elevated, file, out),
            Err(err) => out.error(err.to_string()),
        },
    }
}

pub fn reply(model: &mut Model, reply: OpsReply, out: &mut Out) {
    match reply {
        OpsReply::Restored { side, path, out: result } => restored(model, side, path, result, out),
        OpsReply::TransferTarget {
            mode,
            sel,
            dst,
            dst_elevated,
            target,
            out: result,
        } => transfer_target(model, mode, sel, dst, dst_elevated, target, result, out),
        OpsReply::ExtractReady {
            sel,
            dst,
            dst_elevated,
            target,
            file,
            opened,
            listed,
        } => extract_ready(model, sel, dst, dst_elevated, target, file, opened, listed, out),
        OpsReply::PackTarget {
            sel,
            dst,
            dst_elevated,
            file,
            format,
            out: result,
        } => match result {
            FsOutput::Stat(Ok(Some(_))) => pack_open(model, sel, dst, dst_elevated, file, out),
            FsOutput::Stat(Ok(None)) => {
                let work = Work::WriteFile {
                    conn: dst.conn,
                    path: file.clone(),
                    data: format.empty_archive(),
                    elevated: dst_elevated,
                };
                let after = jobs::After::Ops(After::PackCreated { sel, dst, dst_elevated, file });
                jobs::start(model, Spec::quiet(work, after).no_retry(), out);
            }
            FsOutput::Stat(Err(err)) => out.error(err.to_string()),
            _ => {}
        },
        OpsReply::PackOpened {
            sel,
            dst,
            dst_elevated,
            file,
            out: result,
        } => match result {
            FsOutput::Opened(Ok(info)) => {
                let conn = info.id;
                model.conns.insert(info);
                if let Some(dir) = file.parent() {
                    let key = model.conns.location_key(dst.conn);
                    pane::refresh_dir(model, &key, &dir, out);
                }
                let req = TransferReq {
                    mode: TransferMode::Copy,
                    src: sel.conn,
                    src_dir: sel.dir.clone(),
                    names: sel.names(),
                    src_elevated: sel.elevated,
                    dst: conn,
                    dst_dir: file,
                    dst_elevated,
                    target_name: None,
                    conflicts: Conflicts::Ask,
                };
                start_transfer(model, req, out);
            }
            FsOutput::Opened(Err(err)) => out.error(err.to_string()),
            _ => {}
        },
        OpsReply::ExistingConnection { side, sudo, conn, out: result } => {
            if let FsOutput::Path(path) = result {
                pane::open_location(model, side, Loc { conn, path }, sudo, out);
                model.active = side;
                model.focus(FocusTarget::Pane(side));
            }
        }
        OpsReply::Terminal(result) => {
            if let PlatformOutput::Failed(err) = result {
                out.error(format!("Cannot open Terminal: {err}"));
            }
        }
    }
}

