//! The main window: two panes plus everything that spans both of them —
//! copy/move between panes, file operations with sudo retries, the command
//! line, jobs and connections.

use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::progress::Progress as ProgressBar;
use gpui_kit::component::{
    ActiveTheme as _, IconName, ResizableState, Selectable as _, Sizable as _, TitleBar,
    WindowExt as _, h_flex, h_resizable, resizable_panel, v_flex,
};
use gpui_kit::{prelude::*, *};
use tl_core::archive::{self, ArchiveFormat};
use tl_core::transfer::{self, TransferMode, TransferReport, TransferSpec};
use tl_core::util::human_size;
use tl_core::{Entry, OpCtx, Progress, VPath, Vfs, VfsError, VfsResult};

use crate::actions::*;
use crate::app_state::{AppState, Connection, FileClip};
use crate::bookmarks;
use crate::dialogs;
use crate::editor;
use crate::jobs::{JobStatus, Jobs};
use crate::keymap::shortcut;
use crate::pane::{DraggedFiles, FilePane, Location, PaneEvent, Side, tilde};
use crate::properties;
use crate::quick_look::QuickLook;
use crate::settings::AppearanceMode;
use crate::util::blocking;

type Work<T> = Arc<dyn Fn(&dyn Vfs, &OpCtx, &Progress) -> VfsResult<T> + Send + Sync>;
type Done<T> = Box<dyn FnOnce(&mut Workspace, VfsResult<T>, &mut Window, &mut Context<Workspace>)>;

pub struct Workspace {
    left: Entity<FilePane>,
    right: Entity<FilePane>,
    active: Side,
    split: Entity<ResizableState>,
    pub jobs: Entity<Jobs>,
    cmdline: Entity<InputState>,
    console: Entity<EditorState>,
    console_text: String,
    show_console: bool,
    /// Quick look, while it is on: it takes the place of the pane opposite
    /// the one with the keyboard.
    quick_look: Option<Entity<QuickLook>>,
    _subs: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = cx.global::<AppState>().settings.session.clone();
        let restore = cx.global::<AppState>().settings.general.restore_folders;
        let home = tl_local::local_vfs().home();
        let start = |saved: &Option<String>| {
            saved
                .as_ref()
                .filter(|_| restore)
                .map(|p| VPath::parse(p))
                .filter(|p| p.to_local().is_dir())
                .unwrap_or_else(|| home.clone())
        };
        let left_loc = Location::local(start(&settings.left_path));
        let right_loc = Location::local(start(&settings.right_path));
        let left = cx.new(|cx| FilePane::new(Side::Left, left_loc, false, window, cx));
        let right = cx.new(|cx| FilePane::new(Side::Right, right_loc, false, window, cx));
        let cmdline = cx.new(|cx| InputState::new(window, cx).placeholder("Run a command in the active folder…"));
        let console = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("text")
                .line_number(false)
                .soft_wrap(true)
        });
        let jobs = cx.new(|_| Jobs::new());

        let mut subs = vec![
            cx.subscribe_in(&left, window, Self::on_pane_event),
            cx.subscribe_in(&right, window, Self::on_pane_event),
            cx.subscribe_in(&cmdline, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.run_command(window, cx);
                }
            }),
            cx.observe(&jobs, |_, _, cx| cx.notify()),
        ];
        subs.push(cx.observe_window_appearance(window, |_, window, cx| {
            crate::themes::apply(Some(window), cx);
        }));

        let focus = left.read(cx).focus.clone();
        window.focus(&focus, cx);
        Workspace {
            left,
            right,
            active: Side::Left,
            split: cx.new(|_| ResizableState::default()),
            jobs,
            cmdline,
            console,
            console_text: String::new(),
            show_console: false,
            quick_look: None,
            _subs: subs,
        }
    }

    pub fn pane(&self, side: Side) -> &Entity<FilePane> {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    /// Which pane an action acts on. The tests read it to check that
    /// focusing a pane really hands it the keyboard commands.
    #[cfg(test)]
    pub(crate) fn active_side(&self) -> Side {
        self.active
    }

    fn active_pane(&self) -> &Entity<FilePane> {
        self.pane(self.active)
    }

    fn other_pane(&self) -> &Entity<FilePane> {
        self.pane(self.active.other())
    }

    fn update_title(&self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().read(cx);
        let mut title = format!("{} — Translocator", pane.location().display());
        if pane.elevated() {
            title.push_str(" (sudo)");
        }
        window.set_window_title(&title);
    }

    fn notify_error(&self, message: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
        window.push_notification(Notification::error(message.into()), cx);
    }

    /// Reloads every tab showing `dir` on the file system `key`, in both panes.
    pub fn refresh_dir(&self, key: &str, dir: &VPath, cx: &mut Context<Self>) {
        for pane in [&self.left, &self.right] {
            pane.update(cx, |p, cx| p.refresh_dir(key, dir, cx));
        }
    }

    // ----- pane events ----------------------------------------------------

    fn on_pane_event(&mut self, pane: &Entity<FilePane>, event: &PaneEvent, window: &mut Window, cx: &mut Context<Self>) {
        let side = pane.read(cx).side;
        match event {
            PaneEvent::Focused => {
                self.active = side;
                self.sync_quick_look(window, cx);
                self.update_title(window, cx);
                cx.notify();
            }
            PaneEvent::LocationChanged => {
                self.update_title(window, cx);
                self.remember_paths(cx);
                cx.notify();
            }
            PaneEvent::Error(message) => self.notify_error(message.clone(), window, cx),
            PaneEvent::PermissionDenied(path) => self.offer_sudo_listing(side, path.clone(), window, cx),
            PaneEvent::OpenFile(entry) => self.open_file(side, entry.clone(), window, cx),
            PaneEvent::OpenBookmark(id) => self.open_bookmark(id.clone(), side, window, cx),
            PaneEvent::NewBookmark => bookmarks::edit(None, Some(side), cx.weak_entity(), window, cx),
            PaneEvent::ManageBookmarks => bookmarks::manage(cx.weak_entity(), side, window, cx),
            PaneEvent::PlacesChanged => {
                // Both panes may be on this host, and the row is drawn
                // from the store rather than from the pane.
                for pane in [&self.left, &self.right] {
                    pane.update(cx, |_, cx| cx.notify());
                }
            }
            PaneEvent::Disconnect => {
                let vfs = pane.read(cx).location().on_host().vfs;
                self.disconnect(vfs, cx);
            }
            PaneEvent::Dropped(drag) => {
                let mode = if window.modifiers().shift || window.modifiers().platform {
                    TransferMode::Move
                } else {
                    TransferMode::Copy
                };
                self.transfer_drop(drag.clone(), side, mode, window, cx);
            }
            PaneEvent::DroppedExternal(paths) => self.copy_external(paths.clone(), side, window, cx),
        }
    }

    fn remember_paths(&self, cx: &mut Context<Self>) {
        let left = self.left.read(cx).location().clone();
        let right = self.right.read(cx).location().clone();
        let state = cx.global_mut::<AppState>();
        if left.is_local() {
            state.settings.session.left_path = Some(left.path.display());
        }
        if right.is_local() {
            state.settings.session.right_path = Some(right.path.display());
        }
    }

    fn offer_sudo_listing(&mut self, side: Side, path: VPath, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.pane(side).clone();
        let vfs = pane.read(cx).location().vfs.clone();
        let answer = dialogs::ask_confirm(
            window,
            cx,
            "Permission denied",
            format!("You cannot read {path} as {}. Open it with sudo?", vfs.label()),
            "Use sudo",
            false,
        );
        cx.spawn_in(window, async move |this, cx| {
            if !answer.await {
                return;
            }
            let prep = vfs.clone();
            let result = blocking(move || prep.prepare_elevation(&OpCtx::new())).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(()) => pane.update(cx, |p, cx| p.set_elevated(true, cx)),
                Err(VfsError::Cancelled) => {}
                Err(err) => this.notify_error(err.to_string(), window, cx),
            });
        })
        .detach();
    }

    // ----- generic operation runner ---------------------------------------

    /// Runs provider work on a worker thread. A "permission denied" on a
    /// provider that supports sudo offers to retry elevated.
    #[allow(clippy::too_many_arguments)]
    fn run_op<T: Send + 'static>(
        &mut self,
        title: Option<String>,
        vfs: Arc<dyn Vfs>,
        elevated: bool,
        work: Work<T>,
        done: Done<T>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let job = title.as_ref().map(|t| self.jobs.update(cx, |j, cx| j.start(t.clone(), cx)));
        let (progress, cancel) = match &job {
            Some((_, p, c)) => (p.clone(), c.clone()),
            None => (Progress::new(), tl_core::CancelToken::new()),
        };
        let job_id = job.map(|(id, _, _)| id);
        let label = vfs.label();
        cx.spawn_in(window, async move |this, cx| {
            let ctx = OpCtx {
                elevated,
                cancel,
                ..Default::default()
            };
            let (w, v) = (work.clone(), vfs.clone());
            let result = blocking(move || w(&*v, &ctx, &progress)).await;
            let _ = this.update_in(cx, move |this, window, cx| {
                if let Some(id) = job_id {
                    let status = match &result {
                        Ok(_) => JobStatus::Done("Done".into()),
                        Err(VfsError::Cancelled) => JobStatus::Cancelled,
                        Err(err) => JobStatus::Failed(err.to_string().into()),
                    };
                    this.jobs.update(cx, |j, cx| j.finish(id, status, cx));
                }
                match result {
                    Err(VfsError::PermissionDenied(msg)) if !elevated && vfs.capabilities().elevation => {
                        let answer = dialogs::ask_confirm(
                            window,
                            cx,
                            "Permission denied",
                            format!("{msg}\n\nRetry as root with sudo on {label}?"),
                            "Retry with sudo",
                            false,
                        );
                        cx.spawn_in(window, async move |this, cx| {
                            let retry = answer.await;
                            let _ = this.update_in(cx, move |this, window, cx| {
                                if retry {
                                    this.run_op(title, vfs, true, work, done, window, cx);
                                } else {
                                    done(this, Err(VfsError::PermissionDenied(msg)), window, cx);
                                }
                            });
                        })
                        .detach();
                    }
                    other => done(this, other, window, cx),
                }
            });
        })
        .detach();
    }

    // ----- copy & move ----------------------------------------------------

    fn copy_files(&mut self, _: &CopyFiles, window: &mut Window, cx: &mut Context<Self>) {
        self.transfer_prompt(TransferMode::Copy, window, cx);
    }

    fn move_files(&mut self, _: &MoveFiles, window: &mut Window, cx: &mut Context<Self>) {
        self.transfer_prompt(TransferMode::Move, window, cx);
    }

    /// How data will travel, shown before a copy/move starts.
    fn route_hint(src: &Arc<dyn Vfs>, dst: &Arc<dyn Vfs>, mode: TransferMode) -> String {
        let place = |host: &Arc<dyn Vfs>| {
            if host.plugin_id() == tl_local::PLUGIN_ID {
                "on this Mac".to_string()
            } else {
                format!("on {} with its own tar/zip", host.label())
            }
        };
        if let Some((host, file)) = dst.host() {
            return format!("Adds to {}; the archive is rewritten {}.", file.file_name_lossy(), place(&host));
        }
        if let Some((host, file)) = src.host() {
            return format!("Extracts from {} {}.", file.file_name_lossy(), place(&host));
        }
        let verb = match mode {
            TransferMode::Copy => "cp -a",
            TransferMode::Move => "mv",
        };
        if src.location_key() == dst.location_key() {
            if src.plugin_id() == tl_local::PLUGIN_ID {
                "Runs on this Mac.".into()
            } else {
                format!("Runs `{verb}` on {} — no data passes through this Mac.", src.label())
            }
        } else {
            let local = |v: &Arc<dyn Vfs>| v.plugin_id() == tl_local::PLUGIN_ID;
            match (local(src), local(dst)) {
                (true, false) => format!("Uploads to {} through ssh (tar stream).", dst.label()),
                (false, true) => format!("Downloads from {} through ssh (tar stream).", src.label()),
                _ => format!("Streams {} → {} through this Mac.", src.label(), dst.label()),
            }
        }
    }

    fn transfer_prompt(&mut self, mode: TransferMode, window: &mut Window, cx: &mut Context<Self>) {
        let src = self.active_pane().read(cx);
        let Some(sel) = src.selection() else { return };
        let dst = self.other_pane().read(cx);
        let dst_loc = dst.location().clone();
        let dst_elevated = dst.elevated();
        let verb = match mode {
            TransferMode::Copy => "Copy",
            TransferMode::Move => "Move",
        };
        let target = if dst_loc.path.is_root() {
            "/".to_string()
        } else {
            format!("{}/", dst_loc.path)
        };
        let hint = Self::route_hint(&sel.vfs, &dst_loc.vfs, mode);
        let dst_place = dst_loc.on_host();
        let message = format!(
            "{verb} {} to {}:\n{hint}",
            sel.describe(),
            if dst_place.is_local() { "this Mac".to_string() } else { dst_place.vfs.label() }
        );
        let select = 0..target.len();
        let answer = dialogs::ask_input(window, cx, verb, message, target, Some(select.end..select.end), if mode == TransferMode::Copy { "Copy" } else { "Move" });
        cx.spawn_in(window, async move |this, cx| {
            let Some(text) = answer.await else { return };
            let text = text.trim().to_string();
            if text.is_empty() {
                return;
            }
            let dst_vfs = dst_loc.vfs.clone();
            let base = dst_loc.path.clone();
            let single = sel.entries.len() == 1;
            // Work out "into this folder" vs "as this name".
            let resolved = blocking(move || {
                let target = if text == "~" {
                    dst_vfs.home()
                } else if let Some(rest) = text.strip_prefix("~/") {
                    dst_vfs.home().join(rest)
                } else {
                    base.join(&text)
                };
                let ctx = OpCtx::new().elevated(dst_elevated);
                if text.ends_with('/') {
                    return Ok((target, None));
                }
                match dst_vfs.stat(&target, &ctx)? {
                    Some(e) if e.is_dir() => Ok((target, None)),
                    _ if single => {
                        let name = target.file_name().map(<[u8]>::to_vec);
                        Ok((target.parent().unwrap_or_else(VPath::root), name))
                    }
                    _ => Err(VfsError::NotFound(format!("{target} is not a folder"))),
                }
            })
            .await;
            let _ = this.update_in(cx, move |this, window, cx| match resolved {
                Ok((dst_dir, target_name)) => {
                    let spec = TransferSpec {
                        mode,
                        src: sel.vfs.clone(),
                        src_dir: sel.dir.clone(),
                        names: sel.names(),
                        src_elevated: sel.elevated,
                        dst: dst_loc.vfs.clone(),
                        dst_dir,
                        dst_elevated,
                        target_name,
                    };
                    this.start_transfer(spec, window, cx);
                }
                Err(err) => this.notify_error(err.to_string(), window, cx),
            });
        })
        .detach();
    }

    fn transfer_drop(&mut self, drag: DraggedFiles, to: Side, mode: TransferMode, window: &mut Window, cx: &mut Context<Self>) {
        let dst = self.pane(to).read(cx);
        let spec = TransferSpec {
            mode,
            src: drag.vfs.clone(),
            src_dir: drag.dir.clone(),
            names: drag.names.as_ref().clone(),
            src_elevated: drag.elevated,
            dst: dst.location().vfs.clone(),
            dst_dir: dst.location().path.clone(),
            dst_elevated: dst.elevated(),
            target_name: None,
        };
        self.start_transfer(spec, window, cx);
    }

    /// Files dropped from Finder: copy them into the pane's folder.
    fn copy_external(&mut self, paths: Vec<std::path::PathBuf>, to: Side, window: &mut Window, cx: &mut Context<Self>) {
        let dst = self.pane(to).read(cx);
        let (dst_loc, dst_elevated) = (dst.location().clone(), dst.elevated());
        let mut groups: Vec<(VPath, Vec<Vec<u8>>)> = Vec::new();
        for path in paths {
            let p = VPath::from_local(&path);
            let (Some(parent), Some(name)) = (p.parent(), p.file_name()) else { continue };
            match groups.iter_mut().find(|(dir, _)| *dir == parent) {
                Some((_, names)) => names.push(name.to_vec()),
                None => groups.push((parent, vec![name.to_vec()])),
            }
        }
        for (dir, names) in groups {
            let spec = TransferSpec {
                mode: TransferMode::Copy,
                src: tl_local::local_vfs(),
                src_dir: dir,
                names,
                src_elevated: false,
                dst: dst_loc.vfs.clone(),
                dst_dir: dst_loc.path.clone(),
                dst_elevated,
                target_name: None,
            };
            self.start_transfer(spec, window, cx);
        }
    }

    pub fn start_transfer(&mut self, spec: TransferSpec, window: &mut Window, cx: &mut Context<Self>) {
        let verb = match spec.mode {
            TransferMode::Copy => "Copy",
            TransferMode::Move => "Move",
        };
        let what = match spec.names.as_slice() {
            [one] => format!("“{}”", String::from_utf8_lossy(one)),
            many => format!("{} items", many.len()),
        };
        let dst = Location {
            vfs: spec.dst.clone(),
            path: spec.dst_dir.clone(),
        };
        let title = format!("{verb} {what} → {}", dst.display());
        let (id, progress, cancel) = self.jobs.update(cx, |j, cx| j.start(title, cx));
        let resolver = cx.global::<AppState>().bridge.clone();
        let spec = Arc::new(spec);
        cx.spawn_in(window, async move |this, cx| {
            let s = spec.clone();
            let result = blocking(move || transfer::run(&s, &resolver, &cancel, &progress)).await;
            let _ = this.update_in(cx, |this, window, cx| this.transfer_done(id, spec, result, window, cx));
        })
        .detach();
    }

    fn transfer_done(
        &mut self,
        id: usize,
        spec: Arc<TransferSpec>,
        result: VfsResult<TransferReport>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.refresh_dir(spec.src.location_key(), &spec.src_dir, cx);
        self.refresh_dir(spec.dst.location_key(), &spec.dst_dir, cx);
        // An archive that was written to changed size in its folder.
        for vfs in [&spec.src, &spec.dst] {
            if let Some((host, file)) = vfs.host()
                && let Some(dir) = file.parent()
            {
                self.refresh_dir(host.location_key(), &dir, cx);
            }
        }
        match result {
            Ok(report) => {
                let mut msg = match report.transferred {
                    0 => "Nothing to do".to_string(),
                    1 => "1 item".to_string(),
                    n => format!("{n} items"),
                };
                if report.skipped > 0 {
                    msg.push_str(&format!(", {} skipped", report.skipped));
                }
                if report.server_side && spec.src.plugin_id() != tl_local::PLUGIN_ID {
                    msg.push_str(" · on server");
                }
                self.jobs.update(cx, |j, cx| j.finish(id, JobStatus::Done(msg.into()), cx));
                if let Some(first) = report.names.first().cloned() {
                    for pane in [&self.left, &self.right] {
                        let shows = pane.read(cx).location().vfs.location_key() == spec.dst.location_key()
                            && pane.read(cx).location().path == spec.dst_dir;
                        if shows {
                            pane.update(cx, |p, cx| p.reload_select(first.clone(), cx));
                        }
                    }
                }
            }
            Err(VfsError::Cancelled) => {
                self.jobs.update(cx, |j, cx| j.finish(id, JobStatus::Cancelled, cx));
            }
            Err(err) => {
                self.jobs.update(cx, |j, cx| j.finish(id, JobStatus::Failed(err.to_string().into()), cx));
                let can_src = !spec.src_elevated && spec.src.capabilities().elevation;
                let can_dst = !spec.dst_elevated && spec.dst.capabilities().elevation;
                if err.is_permission_denied() && (can_src || can_dst) {
                    let answer = dialogs::ask_confirm(
                        window,
                        cx,
                        "Permission denied",
                        format!("{err}\n\nRetry with sudo?"),
                        "Retry with sudo",
                        false,
                    );
                    cx.spawn_in(window, async move |this, cx| {
                        if !answer.await {
                            return;
                        }
                        let _ = this.update_in(cx, |this, window, cx| {
                            let retry = TransferSpec {
                                mode: spec.mode,
                                src: spec.src.clone(),
                                src_dir: spec.src_dir.clone(),
                                names: spec.names.clone(),
                                src_elevated: spec.src_elevated || can_src,
                                dst: spec.dst.clone(),
                                dst_dir: spec.dst_dir.clone(),
                                dst_elevated: spec.dst_elevated || can_dst,
                                target_name: spec.target_name.clone(),
                            };
                            this.start_transfer(retry, window, cx);
                        });
                    })
                    .detach();
                } else {
                    dialogs::error(window, cx, "Transfer failed", err.to_string());
                }
            }
        }
    }

    // ----- clipboard ------------------------------------------------------

    fn clipboard_copy(&mut self, _: &ClipboardCopy, window: &mut Window, cx: &mut Context<Self>) {
        self.clip(false, window, cx);
    }

    fn clipboard_cut(&mut self, _: &ClipboardCut, window: &mut Window, cx: &mut Context<Self>) {
        self.clip(true, window, cx);
    }

    fn clip(&mut self, cut: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sel) = self.active_pane().read(cx).selection() else { return };
        let text = sel
            .paths()
            .iter()
            .map(|p| full_path(&sel.vfs, p))
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        let count = sel.entries.len();
        cx.global_mut::<AppState>().clipboard = Some(FileClip {
            vfs: sel.vfs.clone(),
            dir: sel.dir.clone(),
            names: sel.names(),
            elevated: sel.elevated,
            cut,
        });
        let verb = if cut { "Cut" } else { "Copied" };
        window.push_notification(
            Notification::info(format!("{verb} {count} item(s). Paste with ⌘V in any pane.")),
            cx,
        );
    }

    fn clipboard_paste(&mut self, _: &ClipboardPaste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(clip) = cx.global::<AppState>().clipboard.clone() else { return };
        let dst = self.active_pane().read(cx);
        let spec = TransferSpec {
            mode: if clip.cut { TransferMode::Move } else { TransferMode::Copy },
            src: clip.vfs.clone(),
            src_dir: clip.dir.clone(),
            names: clip.names.clone(),
            src_elevated: clip.elevated,
            dst: dst.location().vfs.clone(),
            dst_dir: dst.location().path.clone(),
            dst_elevated: dst.elevated(),
            target_name: None,
        };
        if clip.cut {
            cx.global_mut::<AppState>().clipboard = None;
        }
        self.start_transfer(spec, window, cx);
    }

    // ----- delete ---------------------------------------------------------

    fn trash_files(&mut self, _: &TrashFiles, window: &mut Window, cx: &mut Context<Self>) {
        self.delete(false, window, cx);
    }

    fn delete_files(&mut self, _: &DeleteFiles, window: &mut Window, cx: &mut Context<Self>) {
        self.delete(true, window, cx);
    }

    fn delete(&mut self, permanent: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sel) = self.active_pane().read(cx).selection() else { return };
        let trash = !permanent && sel.vfs.capabilities().trash;
        let names: Vec<String> = sel.entries.iter().take(6).map(|e| format!("• {}", e.name_lossy())).collect();
        let more = sel.entries.len().saturating_sub(6);
        let mut message = names.join("\n");
        if more > 0 {
            message.push_str(&format!("\n… and {more} more"));
        }
        let place = if sel.vfs.plugin_id() == tl_local::PLUGIN_ID {
            String::new()
        } else {
            format!(" on {}", sel.vfs.label())
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
        let ask = !trash || cx.global::<AppState>().settings.general.confirm_trash;
        let answer = ask.then(|| {
            dialogs::ask_confirm(window, cx, title, message, if trash { "Move to Trash" } else { "Delete" }, !trash)
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Some(answer) = answer
                && !answer.await
            {
                return;
            }
            let _ = this.update_in(cx, move |this, window, cx| {
                let paths = sel.paths();
                let dir = sel.dir.clone();
                let key = sel.vfs.location_key().to_string();
                let job = format!(
                    "{} {}",
                    if trash { "Trash" } else { "Delete" },
                    sel.describe()
                );
                this.run_op(
                    Some(job),
                    sel.vfs.clone(),
                    sel.elevated,
                    Arc::new(move |vfs, ctx, progress| {
                        if trash {
                            vfs.trash(&paths, ctx, progress)
                        } else {
                            vfs.delete(&paths, ctx, progress)
                        }
                    }),
                    Box::new(move |this, result, window, cx| {
                        this.refresh_dir(&key, &dir, cx);
                        if let Err(err) = result
                            && !err.is_cancelled() {
                                this.notify_error(err.to_string(), window, cx);
                            }
                    }),
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    // ----- create & rename ------------------------------------------------

    fn new_folder(&mut self, _: &NewFolder, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().read(cx);
        let (loc, elevated) = (pane.location().clone(), pane.elevated());
        let answer = dialogs::ask_input(window, cx, "New Folder", "Name (use / to create nested folders):", "", None, "Create");
        cx.spawn_in(window, async move |this, cx| {
            let Some(name) = answer.await.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) else { return };
            let _ = this.update_in(cx, move |this, window, cx| {
                let first = name.split('/').find(|s| !s.is_empty()).unwrap_or("").as_bytes().to_vec();
                let base = loc.path.clone();
                let key = loc.vfs.location_key().to_string();
                this.run_op(
                    None,
                    loc.vfs.clone(),
                    elevated,
                    Arc::new(move |vfs, ctx, _| {
                        let mut path = base.clone();
                        for part in name.split('/').filter(|s| !s.is_empty()) {
                            path = path.join(part);
                            if vfs.stat(&path, ctx)?.is_none() {
                                vfs.mkdir(&path, ctx)?;
                            }
                        }
                        Ok(())
                    }),
                    Box::new(move |this, result, window, cx| match result {
                        Ok(()) => this.select_in_panes(&key, &loc.path, first, cx),
                        Err(err) => this.notify_error(err.to_string(), window, cx),
                    }),
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    fn new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().read(cx);
        let (loc, elevated) = (pane.location().clone(), pane.elevated());
        let answer = dialogs::ask_input(window, cx, "New File", "Name:", "", None, "Create");
        cx.spawn_in(window, async move |this, cx| {
            let Some(name) = answer.await.map(|n| n.trim().to_string()).filter(|n| !n.is_empty() && !n.contains('/')) else { return };
            let _ = this.update_in(cx, move |this, window, cx| {
                let path = loc.path.child(name.as_bytes());
                let target = path.clone();
                let key = loc.vfs.location_key().to_string();
                this.run_op(
                    None,
                    loc.vfs.clone(),
                    elevated,
                    Arc::new(move |vfs, ctx, _| vfs.create_file(&target, ctx).map(|_| ctx.elevated)),
                    Box::new(move |this, result, window, cx| match result {
                        Ok(used_sudo) => {
                            this.select_in_panes(&key, &loc.path, name.as_bytes().to_vec(), cx);
                            editor::open(loc.vfs.clone(), path, used_sudo, false, cx.weak_entity(), window, cx);
                        }
                        Err(err) => this.notify_error(err.to_string(), window, cx),
                    }),
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    fn select_in_panes(&self, key: &str, dir: &VPath, name: Vec<u8>, cx: &mut Context<Self>) {
        for pane in [&self.left, &self.right] {
            let loc = pane.read(cx).location().clone();
            if loc.vfs.location_key() == key && &loc.path == dir {
                pane.update(cx, |p, cx| p.reload_select(name.clone(), cx));
            }
        }
    }

    fn rename_file(&mut self, _: &RenameFile, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().read(cx);
        let Some(row) = pane.cursor_row().filter(|r| !r.parent).cloned() else { return };
        let (loc, elevated) = (pane.location().clone(), pane.elevated());
        let old = row.name.to_string();
        let select = dialogs::stem_range(&old);
        let answer = dialogs::ask_input(window, cx, "Rename", "", old.clone(), Some(select), "Rename");
        cx.spawn_in(window, async move |this, cx| {
            let Some(new) = answer.await.map(|n| n.trim().to_string()).filter(|n| !n.is_empty() && *n != old) else { return };
            let _ = this.update_in(cx, move |this, window, cx| {
                if new.contains('/') {
                    this.notify_error("A name cannot contain “/”. Use Move (F6) to move items.", window, cx);
                    return;
                }
                let from = loc.path.child(&row.entry.name);
                let to = loc.path.child(new.as_bytes());
                let key = loc.vfs.location_key().to_string();
                this.run_op(
                    None,
                    loc.vfs.clone(),
                    elevated,
                    Arc::new(move |vfs, ctx, _| vfs.rename(&from, &to, ctx)),
                    Box::new(move |this, result, window, cx| match result {
                        Ok(()) => this.select_in_panes(&key, &loc.path, new.into_bytes(), cx),
                        Err(err) => this.notify_error(err.to_string(), window, cx),
                    }),
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    // ----- open, view, edit -----------------------------------------------

    fn view_file(&mut self, _: &ViewFile, window: &mut Window, cx: &mut Context<Self>) {
        self.open_editor(true, window, cx);
    }

    fn edit_file(&mut self, _: &EditFile, window: &mut Window, cx: &mut Context<Self>) {
        self.open_editor(false, window, cx);
    }

    fn open_editor(&mut self, readonly: bool, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().read(cx);
        let Some(row) = pane.cursor_row().filter(|r| !r.parent).cloned() else { return };
        if row.entry.is_dir() {
            return;
        }
        let loc = pane.location().clone();
        let elevated = pane.elevated();
        let path = loc.path.child(&row.entry.name);
        editor::open(loc.vfs, path, elevated, readonly, cx.weak_entity(), window, cx);
    }

    fn open_file(&mut self, side: Side, entry: Entry, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.pane(side).read(cx);
        let loc = pane.location().clone();
        let elevated = pane.elevated();
        if loc.is_local() {
            cx.open_with_system(&loc.path.child(&entry.name).to_local());
            return;
        }
        // Remote: download into the cache, then open with the default app.
        let cache = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("Translocator")
            .join(sanitize(&loc.vfs.label()))
            .join(loc.path.display().trim_start_matches('/'));
        if let Err(err) = std::fs::create_dir_all(&cache) {
            self.notify_error(format!("Cannot create cache folder: {err}"), window, cx);
            return;
        }
        let local_path = cache.join(entry.name_lossy());
        let _ = std::fs::remove_file(&local_path);
        let spec = TransferSpec {
            mode: TransferMode::Copy,
            src: loc.vfs.clone(),
            src_dir: loc.path.clone(),
            names: vec![entry.name.clone()],
            src_elevated: elevated,
            dst: tl_local::local_vfs(),
            dst_dir: VPath::from_local(&cache),
            dst_elevated: false,
            target_name: None,
        };
        let title = format!("Open “{}” from {}", entry.name_lossy(), loc.vfs.label());
        let (id, progress, cancel) = self.jobs.update(cx, |j, cx| j.start(title, cx));
        let resolver = tl_core::transfer::FixedResolver(tl_core::transfer::ConflictChoice::Overwrite);
        cx.spawn_in(window, async move |this, cx| {
            let result = blocking(move || transfer::run(&spec, &resolver, &cancel, &progress)).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(_) => {
                    this.jobs.update(cx, |j, cx| j.finish(id, JobStatus::Done("Opened a local copy".into()), cx));
                    cx.open_with_system(&local_path);
                }
                Err(err) => {
                    this.jobs.update(cx, |j, cx| j.finish(id, JobStatus::Failed(err.to_string().into()), cx));
                    if !err.is_cancelled() {
                        this.notify_error(err.to_string(), window, cx);
                    }
                }
            });
        })
        .detach();
    }

    fn open_externally(&mut self, _: &OpenExternally, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().read(cx);
        let loc = pane.location().clone();
        match pane.cursor_row().filter(|r| !r.parent).cloned() {
            Some(row) if !row.entry.is_dir() => self.open_file(self.active, row.entry, window, cx),
            Some(row) if loc.is_local() => cx.open_with_system(&loc.path.child(&row.entry.name).to_local()),
            _ if loc.is_local() => cx.open_with_system(&loc.path.to_local()),
            _ => {}
        }
    }

    fn reveal_in_finder(&mut self, _: &RevealInFinder, _: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().read(cx);
        let loc = pane.location().clone();
        if let Some((host, file)) = loc.vfs.host() {
            // Inside an archive: show the archive file.
            if host.plugin_id() == tl_local::PLUGIN_ID {
                cx.reveal_path(&file.to_local());
            }
            return;
        }
        if !loc.is_local() {
            return;
        }
        let target = match pane.cursor_row().filter(|r| !r.parent) {
            Some(row) => loc.path.child(&row.entry.name),
            None => loc.path.clone(),
        };
        cx.reveal_path(&target.to_local());
    }

    fn copy_path(&mut self, _: &CopyPath, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().read(cx);
        let loc = pane.location().clone();
        let paths: Vec<String> = match pane.selection() {
            Some(sel) => sel.paths().iter().map(|p| full_path(&loc.vfs, p)).collect(),
            None => vec![full_path(&loc.vfs, &loc.path)],
        };
        let text = paths.join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        window.push_notification(Notification::info(format!("Copied {text}")), cx);
    }

    fn open_terminal(&mut self, _: &OpenTerminal, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().read(cx);
        // Inside an archive: the folder that holds it.
        let loc = pane.location().archive_folder().unwrap_or_else(|| pane.location().clone());
        let elevated = pane.elevated();
        let preferred = cx.global::<AppState>().settings.general.terminal.clone();
        let preferred = Some(preferred.as_str());
        let result = if loc.is_local() {
            crate::terminal::open(Some(&loc.path.to_local()), None, preferred)
        } else {
            match loc.vfs.terminal_command(&loc.path, elevated) {
                Some(argv) => crate::terminal::open(None, Some(&argv), preferred),
                None => Ok(()),
            }
        };
        if let Err(err) = result {
            self.notify_error(format!("Cannot open Terminal: {err}"), window, cx);
        }
    }

    // ----- size, properties -----------------------------------------------

    fn calc_size(&mut self, _: &CalcSize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sel) = self.active_pane().read(cx).selection() else { return };
        let dirs: Vec<Entry> = sel.entries.iter().filter(|e| e.is_dir()).cloned().collect();
        if dirs.is_empty() {
            return;
        }
        let side = self.active;
        let dir = sel.dir.clone();
        let names: Vec<Vec<u8>> = dirs.iter().map(|e| e.name.clone()).collect();
        let job_names = names.clone();
        self.run_op(
            Some(format!("Calculate size of {}", sel.describe())),
            sel.vfs.clone(),
            sel.elevated,
            Arc::new(move |vfs, ctx, progress| {
                let mut sizes = Vec::new();
                for name in &job_names {
                    progress.set_current(String::from_utf8_lossy(name).into_owned());
                    let totals = vfs.scan(&[dir.child(name)], ctx)?;
                    sizes.push((name.clone(), totals));
                }
                Ok(sizes)
            }),
            Box::new(move |this, result, window, cx| match result {
                Ok(sizes) => {
                    let total: u64 = sizes.iter().map(|(_, t)| t.bytes).sum();
                    let files: u64 = sizes.iter().map(|(_, t)| t.files).sum();
                    let pane = this.pane(side).clone();
                    pane.update(cx, |p, cx| {
                        for (name, t) in sizes {
                            p.set_dir_size(name, t.bytes, cx);
                        }
                    });
                    if names.len() > 1 {
                        window.push_notification(
                            Notification::info(format!("{} in {} files", human_size(total), files)),
                            cx,
                        );
                    }
                }
                Err(err) if !err.is_cancelled() => this.notify_error(err.to_string(), window, cx),
                Err(_) => {}
            }),
            window,
            cx,
        );
    }

    // ----- find -----------------------------------------------------------

    fn find_files(&mut self, _: &FindFiles, window: &mut Window, cx: &mut Context<Self>) {
        let side = self.active;
        let pane = self.active_pane().read(cx);
        let (loc, elevated) = (pane.location().clone(), pane.elevated());
        let place = loc.display();
        let answer = dialogs::ask_input(
            window,
            cx,
            "Find Files",
            format!(
                "Search below {place}. Use wildcards like *.conf, or plain text to match anywhere in a name.{}",
                if loc.is_local() { "" } else { " Runs `find` on the server." }
            ),
            "",
            None,
            "Find",
        );
        cx.spawn_in(window, async move |this, cx| {
            let Some(query) = answer.await.map(|q| q.trim().to_string()).filter(|q| !q.is_empty()) else { return };
            let _ = this.update_in(cx, move |this, window, cx| {
                let masks = tl_core::util::search_masks(&query);
                let root = loc.path.clone();
                let vfs = loc.vfs.clone();
                this.run_op(
                    Some(format!("Find “{query}” in {place}")),
                    loc.vfs.clone(),
                    elevated,
                    Arc::new(move |vfs, ctx, progress| {
                        let mut hits = Vec::new();
                        vfs.find(&root, &masks, ctx, &mut |path, kind| {
                            hits.push((path, kind));
                            progress.set_done(0, hits.len() as u64);
                            hits.len() < 5000
                        })?;
                        Ok(hits)
                    }),
                    Box::new(move |this, result, window, cx| match result {
                        Ok(hits) => this.show_find_results(side, vfs, loc.path.clone(), query, hits, window, cx),
                        Err(err) if !err.is_cancelled() => this.notify_error(err.to_string(), window, cx),
                        Err(_) => {}
                    }),
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn show_find_results(
        &mut self,
        side: Side,
        vfs: Arc<dyn Vfs>,
        root: VPath,
        query: String,
        hits: Vec<(VPath, tl_core::EntryKind)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if hits.is_empty() {
            window.push_notification(Notification::info(format!("Nothing matches “{query}”.")), cx);
            return;
        }
        let rows: std::rc::Rc<Vec<(SharedString, VPath, bool)>> = std::rc::Rc::new(
            hits.into_iter()
                .map(|(path, kind)| {
                    let rel = path.relative_to(&root).map(|r| String::from_utf8_lossy(r).into_owned()).unwrap_or_else(|| path.display());
                    (rel.into(), path, kind == tl_core::EntryKind::Dir)
                })
                .collect(),
        );
        let workspace = cx.weak_entity();
        let title: SharedString = format!(
            "{} match{} for “{query}”",
            rows.len(),
            if rows.len() == 1 { "" } else { "es" }
        )
        .into();
        window.open_dialog(cx, move |dialog, _window, cx| {
            let theme = cx.theme().clone();
            let (rows, workspace, vfs) = (rows.clone(), workspace.clone(), vfs.clone());
            let count = rows.len();
            let list = uniform_list("find-results", count, move |range: std::ops::Range<usize>, _window, _cx| {
                range
                    .map(|ix| {
                        let (rel, path, is_dir) = rows[ix].clone();
                        let (workspace, vfs) = (workspace.clone(), vfs.clone());
                        h_flex()
                            .id(("hit", ix))
                            .h(px(24.))
                            .px_2()
                            .gap_2()
                            .rounded_sm()
                            .cursor_pointer()
                            .text_size(px(12.))
                            .hover(|s| s.bg(theme.list_hover))
                            .child(
                                gpui_kit::component::Icon::new(if is_dir { Lucide::Folder } else { Lucide::File })
                                    .size(px(14.))
                                    .text_color(if is_dir { theme.blue } else { theme.muted_foreground }),
                            )
                            .child(div().flex_1().min_w_0().truncate().child(rel))
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                let (vfs, path) = (vfs.clone(), path.clone());
                                let _ = workspace.update(cx, |ws, cx| ws.reveal(side, vfs, path, window, cx));
                            })
                    })
                    .collect::<Vec<_>>()
            })
            .h(px(420.));
            dialog.title(title.clone()).width(px(720.)).child(list)
        });
    }

    /// Shows `path` in a pane: opens its folder and puts the cursor on it.
    fn reveal(&mut self, side: Side, vfs: Arc<dyn Vfs>, path: VPath, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.pane(side).clone();
        let parent = path.parent().unwrap_or_else(VPath::root);
        let name = path.file_name().map(<[u8]>::to_vec);
        pane.update(cx, |p, cx| {
            if !Arc::ptr_eq(&p.location().vfs, &vfs) {
                let elevated = p.elevated();
                p.open_location(Location { vfs, path: parent.clone() }, elevated, cx);
            }
            p.navigate(parent, name, cx);
        });
        window.focus(&pane.read(cx).focus.clone(), cx);
    }

    fn locations_left(&mut self, _: &LocationsLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.locations_for(Side::Left, window, cx);
    }

    fn locations_right(&mut self, _: &LocationsRight, window: &mut Window, cx: &mut Context<Self>) {
        self.locations_for(Side::Right, window, cx);
    }

    fn locations_for(&mut self, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.pane(side).clone();
        window.focus(&pane.read(cx).focus.clone(), cx);
        pane.update(cx, |p, cx| p.open_locations(window, cx));
    }

    fn properties(&mut self, _: &Properties, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sel) = self.active_pane().read(cx).selection() else { return };
        properties::open(sel, cx.weak_entity(), window, cx);
    }

    /// Applies permissions/ownership from the properties dialog.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_attributes(
        &mut self,
        sel: crate::pane::Selection,
        mode: Option<u32>,
        owner: Option<String>,
        group: Option<String>,
        recursive: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths = sel.paths();
        let key = sel.vfs.location_key().to_string();
        let dir = sel.dir.clone();
        self.run_op(
            Some(format!("Change attributes of {}", sel.describe())),
            sel.vfs.clone(),
            sel.elevated,
            Arc::new(move |vfs, ctx, _| {
                if owner.is_some() || group.is_some() {
                    vfs.chown(&paths, owner.as_deref(), group.as_deref(), recursive, ctx)?;
                }
                if let Some(mode) = mode {
                    vfs.chmod(&paths, mode, recursive, ctx)?;
                }
                Ok(())
            }),
            Box::new(move |this, result, window, cx| {
                this.refresh_dir(&key, &dir, cx);
                if let Err(err) = result
                    && !err.is_cancelled() {
                        this.notify_error(err.to_string(), window, cx);
                    }
            }),
            window,
            cx,
        );
    }

    // ----- compare, sync, multi-rename -----------------------------------

    /// Marks what differs between the two panes: entries on one side only,
    /// and the newer of two files that differ.
    fn compare_dirs(&mut self, _: &CompareDirs, window: &mut Window, cx: &mut Context<Self>) {
        let (left, right) = (self.left.read(cx), self.right.read(cx));
        if left.is_loading() || right.is_loading() {
            self.notify_error("Wait until both panes have loaded.", window, cx);
            return;
        }
        let case_insensitive = left.location().vfs.capabilities().case_insensitive
            && right.location().vfs.capabilities().case_insensitive;
        let result = tl_core::compare::compare_listings(&left.listed_entries(), &right.listed_entries(), case_insensitive);
        let stats = result.stats;
        self.left.update(cx, |p, cx| p.set_marks(result.left, cx));
        self.right.update(cx, |p, cx| p.set_marks(result.right, cx));
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
        window.push_notification(Notification::info(message), cx);
    }

    fn sync_dirs(&mut self, _: &SyncDirs, window: &mut Window, cx: &mut Context<Self>) {
        let end = |pane: &Entity<FilePane>, cx: &App| {
            let p = pane.read(cx);
            tl_core::sync::SyncEnd {
                vfs: p.location().vfs.clone(),
                dir: p.location().path.clone(),
                elevated: p.elevated(),
            }
        };
        let (left, right) = (end(&self.left, cx), end(&self.right, cx));
        crate::sync::open(left, right, cx.weak_entity(), window, cx);
    }

    /// Runs a reviewed sync plan as a job.
    pub fn start_sync(
        &mut self,
        plan: tl_core::sync::SyncPlan,
        left: tl_core::sync::SyncEnd,
        right: tl_core::sync::SyncEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let display = |end: &tl_core::sync::SyncEnd| {
            Location {
                vfs: end.vfs.clone(),
                path: end.dir.clone(),
            }
            .display()
        };
        let title = format!("Synchronize {} ⇄ {}", display(&left), display(&right));
        let (id, progress, cancel) = self.jobs.update(cx, |j, cx| j.start(title, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = blocking(move || tl_core::sync::apply(&plan, &left, &right, &cancel, &progress)).await;
            let _ = this.update_in(cx, |this, window, cx| {
                for pane in [&this.left, &this.right] {
                    pane.update(cx, |p, cx| p.reload(cx));
                }
                let status = match result {
                    Ok(report) => JobStatus::Done(
                        format!(
                            "{} → right, {} ← left{}",
                            report.to_right,
                            report.to_left,
                            if report.server_side { " · on server" } else { "" }
                        )
                        .into(),
                    ),
                    Err(VfsError::Cancelled) => JobStatus::Cancelled,
                    Err(err) => {
                        dialogs::error(window, cx, "Synchronization failed", err.to_string());
                        JobStatus::Failed(err.to_string().into())
                    }
                };
                this.jobs.update(cx, |j, cx| j.finish(id, status, cx));
            });
        })
        .detach();
    }

    fn multi_rename(&mut self, _: &MultiRename, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().read(cx);
        let Some(sel) = pane.selection() else { return };
        if sel.vfs.host().is_some() {
            self.notify_error("Files inside archives cannot be renamed. Extract them first.", window, cx);
            return;
        }
        let existing: Vec<Vec<u8>> = pane.listed_entries().into_iter().map(|e| e.name).collect();
        crate::rename::open(sel, existing, cx.weak_entity(), window, cx);
    }

    /// Runs the renames planned by the multi-rename dialog. A failure undoes
    /// the renames already done, so the folder ends up as it was.
    pub fn apply_renames(
        &mut self,
        sel: crate::pane::Selection,
        steps: Vec<tl_core::multirename::Rename>,
        count: usize,
        select: Vec<u8>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dir = sel.dir.clone();
        let key = sel.vfs.location_key().to_string();
        let work_dir = dir.clone();
        self.run_op(
            Some(format!("Rename {count} item{}", if count == 1 { "" } else { "s" })),
            sel.vfs.clone(),
            sel.elevated,
            Arc::new(move |vfs, ctx, progress| tl_core::multirename::apply(vfs, &work_dir, &steps, ctx, progress)),
            Box::new(move |this, result, window, cx| match result {
                Ok(()) => this.select_in_panes(&key, &dir, select, cx),
                Err(err) => {
                    this.refresh_dir(&key, &dir, cx);
                    if !err.is_cancelled() {
                        let message = err.to_string();
                        let title = if message.contains("Undoing") {
                            "Rename failed"
                        } else {
                            "Rename failed; the folder was left as it was"
                        };
                        dialogs::error(window, cx, title, message);
                    }
                }
            }),
            window,
            cx,
        );
    }

    // ----- archives -------------------------------------------------------

    /// Unpacks the selected archives into a folder of the other pane.
    fn extract_archive(&mut self, _: &ExtractArchive, window: &mut Window, cx: &mut Context<Self>) {
        self.extract_into(false, window, cx);
    }

    /// Unpacks them beside themselves instead, into a folder of this pane.
    fn decompress(&mut self, _: &Decompress, window: &mut Window, cx: &mut Context<Self>) {
        self.extract_into(true, window, cx);
    }

    fn extract_into(&mut self, here: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sel) = self.active_pane().read(cx).selection() else { return };
        let archives: Vec<Entry> = sel.entries.iter().filter(|e| FilePane::is_archive(e)).cloned().collect();
        if archives.is_empty() || sel.vfs.host().is_some() {
            self.notify_error("Select one or more .zip, .tar or .tar.gz files to extract.", window, cx);
            return;
        }
        let dst = if here { self.active_pane().read(cx) } else { self.other_pane().read(cx) };
        let (dst_loc, dst_elevated) = (dst.location().clone(), dst.elevated());
        let what = match archives.as_slice() {
            [one] => format!("“{}”", one.name_lossy()),
            many => format!("{} archives", many.len()),
        };
        let dir = if dst_loc.path.is_root() { "/".to_string() } else { format!("{}/", dst_loc.path) };
        // Unpacking beside the archive would otherwise strew its contents
        // over the folder it is in, so a single archive gets a folder of
        // its own, named after it, as the proposal.
        let (target, select) = match (here, archives.as_slice()) {
            (true, [one]) => {
                let stem = tl_core::entry::split_extension(&one.name).0;
                let stem = String::from_utf8_lossy(stem);
                let stem = stem.strip_suffix(".tar").unwrap_or(&stem).to_string();
                let range = dir.len()..dir.len() + stem.len();
                (format!("{dir}{stem}"), range)
            }
            _ => {
                let end = dir.len();
                (dir, end..end)
            }
        };
        let place = dst_loc.on_host();
        let message = format!(
            "Extract {what} into this folder on {} (created if missing):",
            if place.is_local() { "this Mac".to_string() } else { place.vfs.label() }
        );
        let title = if here { "Decompress" } else { "Extract" };
        let answer = dialogs::ask_input(window, cx, title, message, target, Some(select), title);
        cx.spawn_in(window, async move |this, cx| {
            let Some(text) = answer.await.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) else { return };
            let dst_vfs = dst_loc.vfs.clone();
            let base = dst_loc.path.clone();
            let src = sel.clone();
            let opened = blocking(move || {
                let target = resolve_input(&dst_vfs, &base, &text);
                let ctx = OpCtx::new().elevated(dst_elevated);
                let mut path = VPath::root();
                for part in target.components() {
                    path = path.child(part);
                    if dst_vfs.stat(&path, &ctx)?.is_none() {
                        dst_vfs.mkdir(&path, &ctx)?;
                    }
                }
                let src_ctx = OpCtx::new().elevated(src.elevated);
                let mut out = Vec::new();
                for entry in &archives {
                    let file = src.dir.child(&entry.name);
                    let av = archive::open(src.vfs.clone(), file.clone(), &src_ctx)?;
                    let names: Vec<Vec<u8>> = av.list(&file, &src_ctx)?.into_iter().map(|e| e.name).collect();
                    out.push((av as Arc<dyn Vfs>, file, names));
                }
                Ok((target, out))
            })
            .await;
            let _ = this.update_in(cx, move |this, window, cx| match opened {
                Ok((target, archives)) => {
                    for (av, file, names) in archives.into_iter().filter(|(_, _, n)| !n.is_empty()) {
                        let spec = TransferSpec {
                            mode: TransferMode::Copy,
                            src: av,
                            src_dir: file,
                            names,
                            src_elevated: sel.elevated,
                            dst: dst_loc.vfs.clone(),
                            dst_dir: target.clone(),
                            dst_elevated,
                            target_name: None,
                        };
                        this.start_transfer(spec, window, cx);
                    }
                }
                Err(err) => this.notify_error(err.to_string(), window, cx),
            });
        })
        .detach();
    }

    /// Packs the selection into an archive in the other pane's folder;
    /// an existing archive of that name gets the files added.
    fn pack_files(&mut self, _: &PackFiles, window: &mut Window, cx: &mut Context<Self>) {
        self.pack_into(false, window, cx);
    }

    /// Packs it into an archive in this pane's own folder instead.
    fn compress(&mut self, _: &Compress, window: &mut Window, cx: &mut Context<Self>) {
        self.pack_into(true, window, cx);
    }

    fn pack_into(&mut self, here: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sel) = self.active_pane().read(cx).selection() else { return };
        let dst = if here { self.active_pane().read(cx) } else { self.other_pane().read(cx) };
        let (dst_loc, dst_elevated) = (dst.location().clone(), dst.elevated());
        if dst_loc.vfs.host().is_some() {
            let message = if here {
                "This pane is inside an archive. Copy (F5) adds files to it."
            } else {
                "The other pane is inside an archive. Copy (F5) adds files to it."
            };
            self.notify_error(message, window, cx);
            return;
        }
        let stem = match sel.entries.as_slice() {
            [one] if one.is_dir() => one.name_lossy(),
            [one] => String::from_utf8_lossy(tl_core::entry::split_extension(&one.name).0).into_owned(),
            _ => sel.dir.file_name().map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_else(|| "archive".into()),
        };
        let prefix = if dst_loc.path.is_root() { "/".to_string() } else { format!("{}/", dst_loc.path) };
        let initial = format!("{prefix}{stem}.tar.gz");
        let select = prefix.len()..prefix.len() + stem.len();
        let place = dst_loc.on_host();
        let message = format!(
            "Pack {} into an archive on {}. The extension picks the format: .tar.gz, .tgz, .tar or .zip. An existing archive gets the files added.",
            sel.describe(),
            if place.is_local() { "this Mac".to_string() } else { place.vfs.label() }
        );
        let title = if here { "Compress" } else { "Pack" };
        let answer = dialogs::ask_input(window, cx, title, message, initial, Some(select), title);
        cx.spawn_in(window, async move |this, cx| {
            let Some(text) = answer.await.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) else { return };
            let dst_vfs = dst_loc.vfs.clone();
            let base = dst_loc.path.clone();
            let opened = blocking(move || {
                let file = resolve_input(&dst_vfs, &base, &text);
                let name = file.file_name().unwrap_or_default().to_vec();
                let format = ArchiveFormat::detect(&name).ok_or_else(|| {
                    VfsError::other(format!("“{}” does not end in .tar.gz, .tgz, .tar or .zip", String::from_utf8_lossy(&name)))
                })?;
                let ctx = OpCtx::new().elevated(dst_elevated);
                if dst_vfs.stat(&file, &ctx)?.is_none() {
                    dst_vfs.write_file(&file, &format.empty_archive(), &ctx)?;
                }
                let av = archive::open(dst_vfs.clone(), file.clone(), &ctx)?;
                Ok((av as Arc<dyn Vfs>, file))
            })
            .await;
            let _ = this.update_in(cx, move |this, window, cx| match opened {
                Ok((av, file)) => {
                    if let Some(dir) = file.parent() {
                        this.refresh_dir(dst_loc.vfs.location_key(), &dir, cx);
                    }
                    let spec = TransferSpec {
                        mode: TransferMode::Copy,
                        src: sel.vfs.clone(),
                        src_dir: sel.dir.clone(),
                        names: sel.names(),
                        src_elevated: sel.elevated,
                        dst: av,
                        dst_dir: file,
                        dst_elevated,
                        target_name: None,
                    };
                    this.start_transfer(spec, window, cx);
                }
                Err(err) => this.notify_error(err.to_string(), window, cx),
            });
        })
        .detach();
    }

    // ----- panes ----------------------------------------------------------

    fn switch_pane(&mut self, _: &SwitchPane, window: &mut Window, cx: &mut Context<Self>) {
        // The active side is set here rather than left to the focus event:
        // with quick look on, the other pane is only drawn once `active`
        // says so, and a focus handle that is not drawn never reports back.
        let side = self.active.other();
        self.active = side;
        self.sync_quick_look(window, cx);
        let target = self.pane(side).read(cx).focus.clone();
        window.focus(&target, cx);
        cx.notify();
    }

    /// Turns quick look on or off. The file list keeps the keyboard either
    /// way; all that changes is what the other pane draws.
    fn toggle_quick_look(&mut self, _: &ToggleQuickLook, window: &mut Window, cx: &mut Context<Self>) {
        if self.quick_look.take().is_none() {
            self.quick_look = Some(cx.new(|_| QuickLook::new()));
            self.sync_quick_look(window, cx);
        }
        cx.notify();
    }

    /// Points the preview at the pane that has the keyboard.
    fn sync_quick_look(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.quick_look.clone() else { return };
        let pane = self.active_pane().clone();
        view.update(cx, |view, cx| view.watch(pane, window, cx));
    }

    /// Which side shows the preview, for the tests.
    #[cfg(test)]
    pub(crate) fn quick_look_side(&self) -> Option<Side> {
        self.quick_look.as_ref().map(|_| self.active.other())
    }

    #[cfg(test)]
    pub(crate) fn quick_look(&self) -> Option<&Entity<QuickLook>> {
        self.quick_look.as_ref()
    }

    fn focus_pane(&mut self, _: &FocusPane, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.active_pane().read(cx).focus.clone();
        window.focus(&target, cx);
    }

    fn swap_panes(&mut self, _: &SwapPanes, _: &mut Window, cx: &mut Context<Self>) {
        let (l, le) = {
            let p = self.left.read(cx);
            (p.location().clone(), p.elevated())
        };
        let (r, re) = {
            let p = self.right.read(cx);
            (p.location().clone(), p.elevated())
        };
        self.left.update(cx, |p, cx| p.open_location(r, re, cx));
        self.right.update(cx, |p, cx| p.open_location(l, le, cx));
    }

    fn mirror_pane(&mut self, _: &MirrorPane, _: &mut Window, cx: &mut Context<Self>) {
        let (loc, elevated) = {
            let p = self.active_pane().read(cx);
            (p.location().clone(), p.elevated())
        };
        self.other_pane().update(cx, |p, cx| p.open_location(loc, elevated, cx));
    }

    fn toggle_hidden(&mut self, _: &ToggleHidden, _: &mut Window, cx: &mut Context<Self>) {
        // Every pane watches the settings, so this is all it takes.
        let show = !cx.global::<AppState>().settings.general.show_hidden;
        crate::settings::update(cx, |settings| settings.general.show_hidden = show);
    }

    fn toggle_sudo(&mut self, _: &ToggleSudo, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().clone();
        let (vfs, elevated) = {
            let p = pane.read(cx);
            (p.location().vfs.clone(), p.elevated())
        };
        if !vfs.capabilities().elevation {
            self.notify_error("This location does not support sudo.", window, cx);
            return;
        }
        if elevated {
            pane.update(cx, |p, cx| p.set_elevated(false, cx));
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let prep = vfs.clone();
            let result = blocking(move || prep.prepare_elevation(&OpCtx::new())).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(()) => pane.update(cx, |p, cx| p.set_elevated(true, cx)),
                Err(VfsError::Cancelled) => {}
                Err(err) => this.notify_error(err.to_string(), window, cx),
            });
        })
        .detach();
    }

    // ----- connections ----------------------------------------------------

    pub fn open_bookmark(&mut self, id: String, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        let state = cx.global::<AppState>();
        let Some(bookmark) = state.bookmarks.get(&id).cloned() else { return };
        let Some(plugin) = state.registry.get(&bookmark.plugin) else {
            self.notify_error(format!("Unknown connection type “{}”", bookmark.plugin), window, cx);
            return;
        };
        let pane = self.pane(side).clone();
        let sudo = bookmark.flag("sudo");
        if let Some(vfs) = state.connection(&id) {
            let path = plugin.initial_path(&bookmark, &*vfs);
            pane.update(cx, |p, cx| p.open_location(Location { vfs, path }, sudo, cx));
            window.focus(&pane.read(cx).focus.clone(), cx);
            return;
        }
        let env = state.env.clone();
        let (job, progress, cancel) = self
            .jobs
            .update(cx, |j, cx| j.start(format!("Connect to {}", bookmark.name), cx));
        progress.set_phase("Connecting");
        progress.set_indeterminate(true);
        let name = bookmark.name.clone();
        cx.spawn_in(window, async move |this, cx| {
            let b = bookmark.clone();
            let result = blocking(move || {
                let vfs = plugin.connect(&b, &env, &cancel)?;
                let path = plugin.initial_path(&b, &*vfs);
                if sudo {
                    vfs.prepare_elevation(&OpCtx::new())?;
                }
                Ok((vfs, path))
            })
            .await;
            let _ = this.update_in(cx, move |this, window, cx| match result {
                Ok((vfs, path)) => {
                    let info = vfs.server_info().unwrap_or_default();
                    {
                        let state = cx.global_mut::<AppState>();
                        state.connections.retain(|c| c.bookmark_id != id);
                        state.connections.push(Connection {
                            bookmark_id: id.clone(),
                            vfs: vfs.clone(),
                        });
                    }
                    this.jobs.update(cx, |j, cx| j.finish(job, JobStatus::Done(format!("Connected · {info}").into()), cx));
                    pane.update(cx, |p, cx| p.open_location(Location { vfs, path }, sudo, cx));
                    window.focus(&pane.read(cx).focus.clone(), cx);
                }
                Err(VfsError::Cancelled) => {
                    this.jobs.update(cx, |j, cx| j.finish(job, JobStatus::Cancelled, cx));
                }
                Err(err) => {
                    this.jobs.update(cx, |j, cx| j.finish(job, JobStatus::Failed(err.to_string().into()), cx));
                    dialogs::error(window, cx, format!("Cannot connect to {name}"), err.to_string());
                }
            });
        })
        .detach();
    }

    fn disconnect(&mut self, vfs: Arc<dyn Vfs>, cx: &mut Context<Self>) {
        if vfs.plugin_id() == tl_local::PLUGIN_ID {
            return;
        }
        cx.global_mut::<AppState>()
            .connections
            .retain(|c| !Arc::ptr_eq(&c.vfs, &vfs));
        for pane in [&self.left, &self.right] {
            pane.update(cx, |p, cx| p.drop_connection(&vfs, cx));
        }
        std::thread::spawn(move || vfs.disconnect());
    }

    fn disconnect_action(&mut self, _: &Disconnect, _: &mut Window, cx: &mut Context<Self>) {
        let vfs = self.active_pane().read(cx).location().on_host().vfs;
        self.disconnect(vfs, cx);
    }

    /// A connection bookmark that opens straight in this folder, which is
    /// what reaches a server's folder from cold; a place on the row needs
    /// the connection to be up already.
    fn bookmark_connection(&mut self, _: &BookmarkConnection, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().read(cx);
        let loc = pane.location().on_host();
        let state = cx.global::<AppState>();
        let template = if loc.is_local() {
            let mut b = tl_core::Bookmark::new(loc.path.file_name_lossy(), tl_local::PLUGIN_ID);
            b.set("path", tilde(&loc.path));
            b
        } else {
            match state.bookmark_of(&loc.vfs).and_then(|id| state.bookmarks.get(&id).cloned()) {
                Some(origin) => {
                    let mut b = origin.clone();
                    b.id = tl_core::Bookmark::new("", "").id;
                    b.name = format!("{} {}", origin.name, loc.path.file_name_lossy());
                    b.set("path", loc.path.display());
                    b
                }
                None => return,
            }
        };
        bookmarks::edit(Some(template), None, cx.weak_entity(), window, cx);
    }

    fn manage_bookmarks(&mut self, _: &ManageBookmarks, window: &mut Window, cx: &mut Context<Self>) {
        bookmarks::manage(cx.weak_entity(), self.active, window, cx);
    }

    fn new_bookmark(&mut self, _: &NewBookmark, window: &mut Window, cx: &mut Context<Self>) {
        bookmarks::edit(None, Some(self.active), cx.weak_entity(), window, cx);
    }

    // ----- command line ---------------------------------------------------

    fn focus_command_line(&mut self, _: &FocusCommandLine, window: &mut Window, cx: &mut Context<Self>) {
        self.cmdline.update(cx, |s, cx| s.focus(window, cx));
    }

    fn run_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let command = self.cmdline.read(cx).value().trim().to_string();
        if command.is_empty() {
            return;
        }
        self.cmdline.update(cx, |s, cx| s.set_value("", window, cx));
        let pane = self.active_pane().clone();
        let (loc, elevated) = {
            let p = pane.read(cx);
            (p.location().clone(), p.elevated())
        };
        // `cd` changes the pane's folder instead of running anything.
        if command == "cd" || command.starts_with("cd ") {
            let arg = command[2..].trim();
            let target = if arg.is_empty() || arg == "~" {
                loc.vfs.home()
            } else if let Some(rest) = arg.strip_prefix("~/") {
                loc.vfs.home().join(rest)
            } else {
                loc.path.join(arg)
            };
            pane.update(cx, |p, cx| p.navigate(target, None, cx));
            return;
        }
        if !loc.vfs.capabilities().exec {
            self.notify_error("This location cannot run commands.", window, cx);
            return;
        }
        let prompt = format!(
            "{}{} {} $ {command}\n",
            if elevated { "[sudo] " } else { "" },
            loc.vfs.label(),
            tilde(&loc.path)
        );
        self.append_console(&prompt, window, cx);
        self.show_console = true;
        let dir = loc.path.clone();
        let key = loc.vfs.location_key().to_string();
        let cmd = command.clone();
        self.run_op(
            Some(format!("$ {command}")),
            loc.vfs.clone(),
            elevated,
            Arc::new(move |vfs, ctx, _| vfs.exec(&dir, &cmd, ctx)),
            Box::new(move |this, result, window, cx| {
                match result {
                    Ok(out) => {
                        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                        text.push_str(&String::from_utf8_lossy(&out.stderr));
                        if !text.is_empty() && !text.ends_with('\n') {
                            text.push('\n');
                        }
                        if out.status != 0 {
                            text.push_str(&format!("[exit status {}]\n", out.status));
                        }
                        this.append_console(&text, window, cx);
                    }
                    Err(err) => this.append_console(&format!("{err}\n"), window, cx),
                }
                this.refresh_dir(&key, &loc.path, cx);
            }),
            window,
            cx,
        );
    }

    fn append_console(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.console_text.push_str(text);
        // Keep the console bounded.
        if self.console_text.len() > 512 * 1024 {
            let cut = self.console_text.len() - 256 * 1024;
            let cut = self.console_text[cut..].find('\n').map(|i| cut + i + 1).unwrap_or(cut);
            self.console_text.drain(..cut);
        }
        let text = self.console_text.clone();
        let lines = text.lines().count() as u32;
        self.console.update(cx, |s, cx| {
            s.set_value(text, window, cx);
            s.set_cursor_position(tl_cursor(lines), window, cx);
        });
        cx.notify();
    }

    fn toggle_console(&mut self, _: &ToggleConsole, _: &mut Window, cx: &mut Context<Self>) {
        self.show_console = !self.show_console;
        cx.notify();
    }

    fn toggle_jobs(&mut self, _: &ToggleJobs, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_sheet(cx) {
            window.close_sheet(cx);
            return;
        }
        let jobs = self.jobs.clone();
        window.open_sheet(cx, move |sheet, _, _| sheet.title("Jobs").size(px(400.)).child(jobs.clone()));
    }

    fn quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        cx.quit();
    }

    // ----- rendering ------------------------------------------------------

    fn render_function_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let key = |id: &'static str, label: &'static str, action: Box<dyn Action>, k: Option<SharedString>| {
            let action: std::rc::Rc<dyn Action> = action.into();
            h_flex()
                .id(id)
                .flex_1()
                .justify_center()
                .gap_1()
                .h_full()
                .cursor_pointer()
                .rounded_sm()
                .hover(|s| s.bg(theme.list_hover))
                .children(k.map(|k| {
                    div()
                        .text_size(px(10.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground)
                        .child(k)
                }))
                .child(div().text_size(px(12.)).child(label))
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
        };
        h_flex()
            .h(px(26.))
            .flex_shrink_0()
            .px_1()
            .gap_1()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .child(key("fk2", "Rename", Box::new(RenameFile), shortcut("rename-file", cx)))
            .child(key("fk3", "View", Box::new(ViewFile), shortcut("view-file", cx)))
            .child(key("fk4", "Edit", Box::new(EditFile), shortcut("edit-file", cx)))
            .child(key("fk5", "Copy", Box::new(CopyFiles), shortcut("copy-files", cx)))
            .child(key("fk6", "Move", Box::new(MoveFiles), shortcut("move-files", cx)))
            .child(key("fk7", "New Folder", Box::new(NewFolder), shortcut("new-folder", cx)))
            .child(key("fk8", "Delete", Box::new(TrashFiles), shortcut("trash-files", cx)))
            .child(key("fk9", "Command", Box::new(FocusCommandLine), shortcut("focus-command-line", cx)))
    }

    fn render_command_line(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let pane = self.active_pane().read(cx);
        let loc = pane.location();
        let elevated = pane.elevated();
        let prompt: SharedString = format!(
            "{}:{} {}",
            if loc.is_local() { "local".to_string() } else { loc.vfs.label() },
            tilde(&loc.path),
            if elevated { "#" } else { "$" }
        )
        .into();
        h_flex()
            .key_context("CommandLine")
            .h(px(30.))
            .flex_shrink_0()
            .px_2()
            .gap_2()
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .max_w(px(420.))
                    .flex_shrink_0()
                    .truncate()
                    .text_size(px(12.))
                    .font_family(theme.mono_font_family.clone())
                    .text_color(if elevated { theme.danger } else { theme.muted_foreground })
                    .child(prompt),
            )
            .child(div().flex_1().min_w_0().child(Input::new(&self.cmdline).small().appearance(false)))
            .child(
                Button::new("console-toggle")
                    .icon(Lucide::SquareTerminal)
                    .ghost()
                    .xsmall()
                    .tab_stop(false)
                    .selected(self.show_console)
                    .tooltip("Console output (⌘⇧J)")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_console = !this.show_console;
                        cx.notify();
                    })),
            )
    }

    fn render_jobs_strip(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let jobs = self.jobs.read(cx);
        let running: Vec<_> = jobs.list.iter().filter(|j| j.is_running()).collect();
        let job = running.first()?;
        let theme = cx.theme().clone();
        let snap = job.progress.snapshot();
        let fraction = snap.fraction();
        let id = job.id;
        let more = running.len() - 1;
        let detail: SharedString = if snap.bytes_total > 0 {
            format!("{} / {}", human_size(snap.bytes_done), human_size(snap.bytes_total)).into()
        } else if snap.items_total > 0 {
            format!("{} / {}", snap.items_done, snap.items_total).into()
        } else {
            snap.phase.clone().into()
        };
        let title = job.title.clone();
        let jobs_entity = self.jobs.clone();
        Some(
            h_flex()
                .h(px(28.))
                .flex_shrink_0()
                .px_2()
                .gap_3()
                .border_t_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .text_size(px(12.))
                .child(div().w(px(260.)).flex_shrink_0().truncate().child(title))
                .child(
                    div().flex_1().child(
                        ProgressBar::new("strip-progress")
                            .value(fraction.map(|f| f * 100.).unwrap_or(0.))
                            .loading(fraction.is_none())
                            .xsmall(),
                    ),
                )
                .child(div().flex_shrink_0().text_color(theme.muted_foreground).child(detail))
                .when(more > 0, |el| {
                    el.child(
                        Button::new("more-jobs")
                            .label(format!("+{more}"))
                            .ghost()
                            .xsmall()
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleJobs), cx)),
                    )
                })
                .child(
                    Button::new("strip-cancel")
                        .icon(IconName::Close)
                        .ghost()
                        .xsmall()
                        .tooltip("Cancel")
                        .on_click(move |_, _, cx| jobs_entity.update(cx, |j, cx| j.cancel(id, cx))),
                ),
        )
    }

    /// What a side draws: its file list, or the preview when quick look is
    /// on and the keyboard is on the other side.
    fn side_view(&self, side: Side) -> AnyElement {
        match &self.quick_look {
            Some(view) if side != self.active => view.clone().into_any_element(),
            _ => self.pane(side).clone().into_any_element(),
        }
    }

    fn render_title(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let running = self.jobs.read(cx).running();
        let dark = theme.is_dark();
        TitleBar::new().child(
            h_flex()
                .w_full()
                .pr_2()
                .gap_2()
                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child("Translocator"))
                .child(div().flex_1())
                .child(
                    Button::new("jobs")
                        .icon(if running > 0 { IconName::LoaderCircle } else { IconName::Inbox })
                        .when(running > 0, |b| b.label(format!("{running}")))
                        .ghost()
                        .xsmall()
                        .tab_stop(false)
                        .tooltip(tooltip("Jobs", "toggle-jobs", cx))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleJobs), cx)),
                )
                .child(
                    Button::new("bookmarks")
                        .icon(Lucide::Bookmark)
                        .ghost()
                        .xsmall()
                        .tab_stop(false)
                        .tooltip(tooltip("Bookmarks", "manage-bookmarks", cx))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(ManageBookmarks), cx)),
                )
                .child(
                    Button::new("theme")
                        .icon(if dark { IconName::Sun } else { IconName::Moon })
                        .ghost()
                        .xsmall()
                        .tab_stop(false)
                        .tooltip(if dark { "Switch to light" } else { "Switch to dark" })
                        .on_click(move |_, window, cx| {
                            // Picking a side here is a decision, so it is
                            // remembered instead of following the system
                            // back at the next appearance change.
                            let mode = if dark { AppearanceMode::Light } else { AppearanceMode::Dark };
                            crate::settings::update(cx, |s| s.appearance.mode = mode);
                            crate::themes::apply(Some(window), cx);
                        }),
                )
                .child(
                    Button::new("settings")
                        .icon(Lucide::Settings)
                        .ghost()
                        .xsmall()
                        .tab_stop(false)
                        .tooltip(tooltip("Settings", "open-settings", cx))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
                ),
        )
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let strip = self.render_jobs_strip(cx);
        v_flex()
            .size_full()
            .key_context(WORKSPACE)
            .bg(theme.background)
            .on_action(cx.listener(Self::copy_files))
            .on_action(cx.listener(Self::move_files))
            .on_action(cx.listener(Self::trash_files))
            .on_action(cx.listener(Self::delete_files))
            .on_action(cx.listener(Self::new_folder))
            .on_action(cx.listener(Self::new_file))
            .on_action(cx.listener(Self::rename_file))
            .on_action(cx.listener(Self::view_file))
            .on_action(cx.listener(Self::edit_file))
            .on_action(cx.listener(Self::open_externally))
            .on_action(cx.listener(Self::reveal_in_finder))
            .on_action(cx.listener(Self::copy_path))
            .on_action(cx.listener(Self::open_terminal))
            .on_action(cx.listener(Self::calc_size))
            .on_action(cx.listener(Self::properties))
            .on_action(cx.listener(Self::find_files))
            .on_action(cx.listener(Self::compare_dirs))
            .on_action(cx.listener(Self::sync_dirs))
            .on_action(cx.listener(Self::multi_rename))
            .on_action(cx.listener(Self::extract_archive))
            .on_action(cx.listener(Self::pack_files))
            .on_action(cx.listener(Self::compress))
            .on_action(cx.listener(Self::decompress))
            .on_action(cx.listener(Self::locations_left))
            .on_action(cx.listener(Self::locations_right))
            .on_action(cx.listener(Self::clipboard_copy))
            .on_action(cx.listener(Self::clipboard_cut))
            .on_action(cx.listener(Self::clipboard_paste))
            .on_action(cx.listener(Self::switch_pane))
            .on_action(cx.listener(Self::focus_pane))
            .on_action(cx.listener(Self::swap_panes))
            .on_action(cx.listener(Self::mirror_pane))
            .on_action(cx.listener(Self::toggle_quick_look))
            .on_action(cx.listener(Self::toggle_hidden))
            .on_action(cx.listener(Self::toggle_sudo))
            .on_action(cx.listener(Self::disconnect_action))
            .on_action(cx.listener(Self::bookmark_connection))
            .on_action(cx.listener(Self::manage_bookmarks))
            .on_action(cx.listener(Self::new_bookmark))
            .on_action(cx.listener(Self::focus_command_line))
            .on_action(cx.listener(Self::toggle_console))
            .on_action(cx.listener(Self::toggle_jobs))
            .on_action(cx.listener(Self::quit))
            .child(self.render_title(cx))
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("panes")
                        .with_state(&self.split)
                        .child(resizable_panel().child(self.side_view(Side::Left)))
                        .child(resizable_panel().child(self.side_view(Side::Right))),
                ),
            )
            .when(self.show_console, |el| {
                el.child(
                    div()
                        .h(px(200.))
                        .flex_shrink_0()
                        .border_t_1()
                        .border_color(theme.border)
                        .child(Editor::new(&self.console).h_full().readonly(true).bordered(false)),
                )
            })
            .child(self.render_command_line(cx))
            .children(strip)
            .child(self.render_function_bar(cx))
    }
}

/// "Jobs (⌘J)", or just "Jobs" when the command has no key.
fn tooltip(label: &str, id: &'static str, cx: &App) -> SharedString {
    match shortcut(id, cx) {
        Some(keys) => format!("{label} ({keys})").into(),
        None => label.to_string().into(),
    }
}

fn tl_cursor(lines: u32) -> gpui_kit::component::input::Position {
    gpui_kit::component::input::Position::new(lines, 0)
}

/// A path typed into a dialog: `~`, `~/x`, absolute, or relative to `base`.
fn resolve_input(vfs: &Arc<dyn Vfs>, base: &VPath, text: &str) -> VPath {
    if text == "~" {
        vfs.home()
    } else if let Some(rest) = text.strip_prefix("~/") {
        vfs.home().join(rest)
    } else {
        base.join(text)
    }
}

/// `/local/path` or `user@host:/remote/path`.
pub fn full_path(vfs: &Arc<dyn Vfs>, path: &VPath) -> String {
    if let Some((host, _)) = vfs.host() {
        return full_path(&host, path);
    }
    if vfs.plugin_id() == tl_local::PLUGIN_ID {
        path.display()
    } else {
        format!("{}:{}", vfs.label(), path)
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || "-_.@".contains(c) { c } else { '_' })
        .collect()
}

