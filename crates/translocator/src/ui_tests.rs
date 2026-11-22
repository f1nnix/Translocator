//! Headless UI tests: the real workspace in a GPUI test window, over a real
//! core and runtime, driven through its actions, dialogs and the keyboard,
//! while the file operations run for real on temporary folders.
//!
//! The core runs on the runtime's own thread and the file work on its
//! workers, so every step waits ([`Fixture::settle`]) until the runtime has
//! nothing left to do, the shell has taken in everything it sent, and the
//! view has stopped changing. What a test reads is then what the window
//! shows: the core's view model as the shell last received it, the GPUI
//! entities and windows, and the files on disk.

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use gpui::{
    Action, AnyWindowHandle, App, AppContext as _, Bounds, Entity, Point, TestAppContext, WindowBounds,
    WindowOptions, px, size,
};
use tl_app::dialog::DialogKind;
use tl_app::model::SettingsEdit;
use tl_app::pane::{PARENT, PaneInput};
use tl_app::quick_look::Body;
use tl_app::settings::{AppearanceMode, Settings};
use tl_app::view::{LocationKind, PaneView};
use tl_app::{Event, Side, ViewModel};
use tl_runtime::Runtime;

use crate::actions::*;
use crate::app_state::{self, Shell};
use crate::pane::FilePane;
use crate::workspace::Workspace;

/// How long a test waits for anything at all before it gives up.
const DEADLINE: Duration = Duration::from_secs(60);

struct Fixture {
    runtime: Runtime,
    root: PathBuf,
    window: AnyWindowHandle,
    ws: Entity<Workspace>,
    // Last: the runtime is shut down (in `drop`) before its folders go.
    _tmp: tempfile::TempDir,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Stops the runtime's threads, jobs and watchers while the
        // temporary folders still exist.
        self.runtime.shutdown();
    }
}

fn write(path: &Path, text: &str, age_days: u64) {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(path, text).unwrap();
    let time = SystemTime::now() - Duration::from_secs(age_days * 86_400);
    fs::File::options().write(true).open(path).unwrap().set_modified(time).unwrap();
}

fn read(path: PathBuf) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// The members of a tar archive, as `tar -t` lists them (empty if it
/// cannot be read yet).
fn tar_members(path: &Path) -> String {
    std::process::Command::new("tar")
        .arg("-tzf")
        .arg(path)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// The rows a pane lists, ".." left out, in order.
fn listed(pane: &PaneView) -> Vec<String> {
    pane.order
        .iter()
        .filter(|ix| **ix != PARENT)
        .filter_map(|&ix| pane.listing.names.get(ix as usize).cloned())
        .collect()
}

/// The name under a pane's cursor; ".." for the row above the folder.
fn cursor_name(pane: &PaneView) -> Option<String> {
    match *pane.order.get(pane.cursor)? {
        PARENT => Some("..".into()),
        ix => pane.listing.names.get(ix as usize).cloned(),
    }
}

fn pane_view(view: &ViewModel, side: Side) -> &PaneView {
    &view.panes[side.index()]
}

/// The folder a pane is in, as a local path.
fn pane_dir(view: &ViewModel, side: Side) -> PathBuf {
    pane_view(view, side).path.to_local()
}

/// What quick look shows, in the terms the tests compare.
#[derive(Debug, PartialEq)]
enum Preview {
    Nothing,
    Reading,
    Text(String),
    Note(String),
    Image(usize),
    Other(String),
}

fn preview_of(body: &Body) -> Preview {
    match body {
        Body::Nothing => Preview::Nothing,
        Body::Reading => Preview::Reading,
        Body::Text { text, .. } => Preview::Text(text.to_string()),
        Body::Note(note) => Preview::Note(note.clone()),
        Body::Image { bytes, .. } => Preview::Image(bytes.len()),
        other => Preview::Other(format!("{other:?}")),
    }
}

/// Opens the workspace on `<tmp>/left` and `<tmp>/right` after `build`
/// filled the folders (and, if it wants, `<tmp>/state`).
fn open(cx: &mut TestAppContext, build: impl FnOnce(&Path)) -> Fixture {
    // The runtime and the file operations run on real threads.
    cx.executor().allow_parking();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    for dir in ["left", "right", "state"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    build(&root);
    let mut settings = Settings::default();
    settings.session.left_path = Some(root.join("left").display().to_string());
    settings.session.right_path = Some(root.join("right").display().to_string());
    let (runtime, window, ws) = cx.update(|cx| {
        gpui_kit::init(cx);
        // The same order `main` starts in, so a test sees the key map and
        // the menus the application really has.
        let runtime = app_state::init_for_tests(cx, &root, &settings);
        crate::themes::register(cx);
        crate::themes::apply(None, cx);
        crate::actions::handle_system_actions(cx);
        crate::actions::bind_system_keys(cx);
        crate::keymap::apply(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: size(px(1280.), px(820.)),
            })),
            ..Default::default()
        };
        let (window, ws) =
            gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Workspace::new(window, cx))).expect("test window");
        cx.global_mut::<Shell>().main_window = Some(window);
        (runtime, window, ws)
    });
    // gpui reports an empty focus path while a window is inactive, and a
    // pane tells the core it is the active one from a focus event, so
    // without this every test would run with the left pane active whatever
    // it focused.
    cx.update_window(window, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
    let fixture = Fixture {
        runtime,
        root: root.clone(),
        window,
        ws,
        _tmp: tmp,
    };
    fixture.settle(cx, "both panes to load", move |view, _| {
        view.ready
            && [(Side::Left, "left"), (Side::Right, "right")].iter().all(|(side, dir)| {
                let pane = pane_view(view, *side);
                !pane.loading && pane.path.to_local() == root.join(dir)
            })
    });
    fixture
}

impl Fixture {
    /// Draws, runs the UI and lets the runtime work until the view is
    /// current and `done` holds.
    ///
    /// Current means: the runtime has nothing queued, running or about to
    /// fire; the shell has carried out everything the runtime sent it; and
    /// the view the shell holds is the one it held a turn ago — so a
    /// reaction of the shell (a focus event, a dialog's field) has been
    /// through the core too. The GPUI clock moves on each turn, so
    /// animations and the shell's timers complete.
    ///
    /// It gives up on the wall clock rather than after a number of turns:
    /// the work runs on real threads against real files, and a machine with
    /// every core busy takes longer to do the same amount of it.
    fn settle(&self, cx: &mut TestAppContext, what: &str, mut done: impl FnMut(&ViewModel, &App) -> bool) {
        let deadline = Instant::now() + DEADLINE;
        let mut last: Option<Rc<ViewModel>> = None;
        loop {
            self.draw(cx);
            cx.executor().advance_clock(Duration::from_millis(20));
            cx.run_until_parked();
            self.runtime.settle(Duration::from_millis(20));
            cx.run_until_parked();
            let view = cx.update(|cx| app_state::view(cx));
            let current = self.runtime.settle(Duration::ZERO)
                && cx.update(|cx| app_state::ui_idle(cx))
                && last.as_ref().is_some_and(|last| Rc::ptr_eq(last, &view));
            if current && cx.update(|cx| done(&view, cx)) {
                return;
            }
            last = Some(view);
            if Instant::now() >= deadline {
                let state = if current { "the view was current" } else { "the view never settled" };
                panic!("timed out after {DEADLINE:?} waiting for {what} ({state})\n{}", self.snapshot(cx));
            }
        }
    }

    /// Waits until nothing is going on any more.
    fn quiet(&self, cx: &mut TestAppContext) {
        self.settle(cx, "the view to settle", |_, _| true);
    }

    /// The window as it stands, printed when a wait gives up: what a stuck
    /// test was actually looking at.
    fn snapshot(&self, cx: &mut TestAppContext) -> String {
        let view = cx.update(|cx| app_state::view(cx));
        let mut out = format!("active: {:?}\n", view.active);
        for side in [Side::Left, Side::Right] {
            let pane = pane_view(&view, side);
            out.push_str(&format!(
                "{side:?}: {} · {:?} · rows {:?} · cursor {:?} · marked {} · filter {:?} · loading {} · error {:?}\n",
                pane.display,
                pane.kind,
                listed(pane),
                cursor_name(pane),
                pane.marked.len(),
                pane.filter,
                pane.loading,
                pane.error,
            ));
        }
        for dialog in &view.dialogs {
            let kind = format!("{:?}", dialog.kind);
            out.push_str(&format!("dialog {:?}: {}\n", dialog.id, kind.chars().take(300).collect::<String>()));
        }
        for job in &view.jobs {
            out.push_str(&format!("job {}: {:?} {}\n", job.title, job.status, job.detail));
        }
        if let Some(ql) = &view.quick_look {
            out.push_str(&format!("quick look on {:?}: {} {:?}\n", ql.side, ql.name, preview_of(&ql.body)));
        }
        out
    }

    fn draw(&self, cx: &mut TestAppContext) {
        cx.update_window(self.window, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    }

    fn view(&self, cx: &mut TestAppContext) -> Rc<ViewModel> {
        cx.update(|cx| app_state::view(cx))
    }

    /// Dispatches `action` to the focused element, like its key binding,
    /// and waits for what follows.
    fn dispatch(&self, cx: &mut TestAppContext, action: impl Action) {
        self.draw(cx);
        cx.dispatch_action(self.window, action);
        self.quiet(cx);
    }

    /// Sends an event straight to the core, as the shell's own handlers
    /// do, and waits for what follows.
    fn send(&self, cx: &mut TestAppContext, event: Event) {
        cx.update(|cx| app_state::dispatch(event, cx));
        self.quiet(cx);
    }

    /// A change from the settings window.
    fn edit(&self, cx: &mut TestAppContext, edit: SettingsEdit) {
        self.send(cx, Event::Settings(edit));
    }

    fn keys(&self, cx: &mut TestAppContext, keys: &str) {
        self.draw(cx);
        cx.simulate_keystrokes(self.window, keys);
        // Let a closing dialog finish its animation and hand focus back.
        cx.executor().advance_clock(Duration::from_millis(500));
        self.quiet(cx);
    }

    fn type_text(&self, cx: &mut TestAppContext, text: &str) {
        self.draw(cx);
        cx.simulate_input(self.window, text);
        self.quiet(cx);
    }

    /// Focuses a pane, as clicking into it would.
    fn focus(&self, cx: &mut TestAppContext, side: Side) {
        let pane = self.pane(cx, side);
        cx.update_window(self.window, |_, window, cx| {
            let focus = pane.read(cx).focus.clone();
            window.focus(&focus, cx);
        })
        .unwrap();
        // The pane tells the core from its focus event, which gpui
        // dispatches at the end of the next frame.
        self.settle(cx, "the pane to become the active one", move |view, _| view.active == side);
    }

    fn pane(&self, cx: &mut TestAppContext, side: Side) -> Entity<FilePane> {
        let ws = self.ws.clone();
        cx.update(|cx| ws.read(cx).pane(side).clone())
    }

    /// The rows a pane lists (or the marked ones), sorted.
    fn names(&self, cx: &mut TestAppContext, side: Side, marked: bool) -> Vec<String> {
        let view = self.view(cx);
        let pane = pane_view(&view, side);
        let mut names = listed(pane);
        if marked {
            names.retain(|n| pane.marked.contains(n.as_bytes()));
        }
        names.sort();
        names
    }

    /// The rows as drawn, in order, which is what a sort setting changes.
    fn row_names(&self, cx: &mut TestAppContext, side: Side) -> Vec<String> {
        let view = self.view(cx);
        listed(pane_view(&view, side))
    }

    fn filter(&self, cx: &mut TestAppContext, side: Side) -> String {
        self.view(cx).panes[side.index()].filter.clone()
    }

    /// Focuses the left pane and puts its cursor on `name`, nothing marked.
    fn select(&self, cx: &mut TestAppContext, name: &str) {
        self.select_in(cx, Side::Left, name);
    }

    /// Focuses a pane, unmarks what is marked (⌘⇧A) and clicks the row
    /// called `name`, once the pane lists it (a file just made shows up
    /// when the watcher has seen it). With nothing marked, a command acts
    /// on the row under the cursor.
    fn select_in(&self, cx: &mut TestAppContext, side: Side, name: &str) {
        self.focus(cx, side);
        if !self.view(cx).panes[side.index()].marked.is_empty() {
            self.dispatch(cx, UnmarkAll);
        }
        self.click(cx, side, name, false);
    }

    /// A click on the row called `name`; `toggle` is ⌘-click, which marks
    /// or unmarks it. Waits for the row to be listed first.
    fn click(&self, cx: &mut TestAppContext, side: Side, name: &str, toggle: bool) {
        let wanted = name.to_string();
        self.settle(cx, &format!("{name} to be listed"), |view, _| {
            let pane = pane_view(view, side);
            !pane.loading && listed(pane).contains(&wanted)
        });
        let view = self.view(cx);
        let pane = pane_view(&view, side);
        let row = pane
            .order
            .iter()
            .position(|&ix| ix != PARENT && pane.listing.names.get(ix as usize) == Some(&wanted))
            .unwrap();
        self.send(cx, Event::Pane(side, PaneInput::Click { row, toggle, range: false }));
        self.settle(cx, "the cursor to move", |view, _| {
            cursor_name(pane_view(view, side)).as_deref() == Some(name)
        });
    }

    /// The saved folders on the row of a pane, as (name, path).
    fn places(&self, cx: &mut TestAppContext, side: Side) -> Vec<(String, String)> {
        let view = self.view(cx);
        pane_view(&view, side)
            .places
            .iter()
            .map(|p| (p.name.clone(), p.path.display()))
            .collect()
    }

    /// Where the file list starts, which says whether a row above it is
    /// really being drawn.
    fn list_top(&self, cx: &mut TestAppContext, side: Side) -> f32 {
        self.draw(cx);
        let pane = self.pane(cx, side);
        cx.update(|cx| pane.read(cx).list_top())
    }

    /// What quick look is showing, once it has settled on a file.
    fn preview(&self, cx: &mut TestAppContext) -> Preview {
        self.settle(cx, "the preview to be read", |view, _| {
            view.quick_look.as_ref().is_some_and(|ql| ql.body != Body::Reading)
        });
        preview_of(&self.view(cx).quick_look.as_ref().unwrap().body)
    }

    fn preview_name(&self, cx: &mut TestAppContext) -> Option<String> {
        self.view(cx).quick_look.as_ref().map(|ql| ql.name.clone())
    }

    /// The side quick look is drawn on, if it is on.
    fn quick_look_side(&self, cx: &mut TestAppContext) -> Option<Side> {
        let view = self.view(cx);
        let side = view.quick_look.as_ref()?.side;
        // The window draws it in place of the pane without the keyboard.
        assert_ne!(side, view.active, "quick look covers the pane the keyboard is in");
        Some(side)
    }

    fn dialog_open(&self, cx: &mut TestAppContext) -> bool {
        self.draw(cx);
        cx.update_window(self.window, |_, window, cx| gpui_kit::component::WindowExt::has_active_dialog(window, cx))
            .unwrap_or(false)
    }

    /// The settings as they are on disk, which is what the next run reads.
    fn saved_settings(&self) -> Settings {
        let bytes = fs::read(self.root.join("state/settings.json")).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn files(&self, dir: &str) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.root.join(dir))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

#[gpui::test]
fn compare_marks_what_differs(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        write(&root.join("left/same.txt"), "same", 3);
        write(&root.join("right/same.txt"), "same", 3);
        write(&root.join("left/changed.txt"), "new", 1);
        write(&root.join("right/changed.txt"), "old", 5);
        write(&root.join("left/only-left.txt"), "l", 1);
        write(&root.join("right/only-right.txt"), "r", 1);
    });
    f.focus(cx, Side::Left);
    f.dispatch(cx, CompareDirs);
    assert_eq!(f.names(cx, Side::Left, true), ["changed.txt", "only-left.txt"]);
    assert_eq!(f.names(cx, Side::Right, true), ["only-right.txt"]);
}

/// Marks `names` in the left pane: a ⌘-click on each.
fn mark(f: &Fixture, cx: &mut TestAppContext, names: &[&str]) {
    f.focus(cx, Side::Left);
    for name in names {
        f.click(cx, Side::Left, name, true);
    }
    let mut want: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    want.sort();
    assert_eq!(f.names(cx, Side::Left, true), want);
}

#[gpui::test]
fn multi_rename_renames_the_marked_files(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        write(&root.join("left/IMG_1.JPG"), "1", 2);
        write(&root.join("left/IMG_2.JPG"), "2", 1);
        write(&root.join("left/keep.txt"), "k", 1);
    });
    mark(&f, cx, &["IMG_1.JPG", "IMG_2.JPG"]);
    f.focus(cx, Side::Left);
    f.dispatch(cx, MultiRename);
    // The name mask has focus with its text selected: typing replaces it.
    f.type_text(cx, "photo-[C]");
    f.keys(cx, "enter");
    let root = f.root.clone();
    f.settle(cx, "the renames", move |_, _| root.join("left/photo-2.JPG").exists());
    assert_eq!(f.files("left"), ["keep.txt", "photo-1.JPG", "photo-2.JPG"]);
    assert_eq!(read(f.root.join("left/photo-1.JPG")), "1");
    assert_eq!(read(f.root.join("left/photo-2.JPG")), "2");
    // The pane lists the new names.
    f.settle(cx, "the pane to reload", |view, _| {
        let pane = pane_view(view, Side::Left);
        !pane.loading && listed(pane) == ["keep.txt", "photo-1.JPG", "photo-2.JPG"]
    });
}

#[gpui::test]
fn multi_rename_keeps_the_dialog_open_on_clashes(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        write(&root.join("left/a.txt"), "a", 1);
        write(&root.join("left/b.txt"), "b", 1);
    });
    mark(&f, cx, &["a.txt", "b.txt"]);
    f.focus(cx, Side::Left);
    f.dispatch(cx, MultiRename);
    // Both would be called "same.txt": Enter must not rename (nor close).
    f.type_text(cx, "same");
    f.keys(cx, "enter");
    assert_eq!(f.files("left"), ["a.txt", "b.txt"]);
    let view = f.view(cx);
    match view.dialogs.last().map(|d| &d.kind) {
        // The mask did reach the dialog: it is the clash that stops it.
        Some(DialogKind::Rename(rename)) => {
            assert_eq!(rename.name, "same");
            assert_eq!(rename.to_rename, 0, "{:?}", rename.preview);
        }
        other => panic!("the core closed the dialog: {other:?}"),
    }
    assert!(f.dialog_open(cx), "and the window still shows it");
}

#[gpui::test]
fn sync_copies_both_ways(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        write(&root.join("left/a.txt"), "only left", 1);
        write(&root.join("right/b.txt"), "only right", 1);
        write(&root.join("left/docs/n.txt"), "newer", 1);
        write(&root.join("right/docs/n.txt"), "older", 9);
        write(&root.join("left/new-dir/deep/x.txt"), "x", 1);
    });
    f.focus(cx, Side::Left);
    f.dispatch(cx, SyncDirs);
    // Enter synchronizes as soon as the comparison is done.
    f.settle(cx, "the comparison", |view, _| {
        view.dialogs.iter().any(|d| match &d.kind {
            DialogKind::Sync(sync) => sync.comparing.is_none() && sync.plan.is_some(),
            _ => false,
        })
    });
    f.keys(cx, "enter");
    let root = f.root.clone();
    f.settle(cx, "the sync", move |_, _| {
        root.join("left/b.txt").exists()
            && root.join("right/a.txt").exists()
            && root.join("right/new-dir/deep/x.txt").exists()
            && read(root.join("right/docs/n.txt")) == "newer"
    });
    assert_eq!(read(f.root.join("left/docs/n.txt")), "newer");
    assert_eq!(read(f.root.join("right/a.txt")), "only left");
    assert_eq!(read(f.root.join("left/b.txt")), "only right");
    assert_eq!(read(f.root.join("right/new-dir/deep/x.txt")), "x");
}

#[gpui::test]
fn archives_open_as_folders_extract_and_pack(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        write(&root.join("stage/www/index.html"), "<h1>hi</h1>", 1);
        write(&root.join("stage/README"), "readme", 1);
        let status = std::process::Command::new("tar")
            .args(["-czf", "../left/site.tar.gz", "www", "README"])
            .current_dir(root.join("stage"))
            .status()
            .unwrap();
        assert!(status.success());
        write(&root.join("left/keep.txt"), "keep", 1);
    });

    // Enter opens the archive as a folder.
    f.select(cx, "site.tar.gz");
    f.dispatch(cx, Open);
    f.settle(cx, "the archive to open", |view, _| {
        let pane = pane_view(view, Side::Left);
        pane.kind == LocationKind::Archive && !pane.loading && listed(pane).len() == 2
    });
    assert_eq!(f.names(cx, Side::Left, false), ["README", "www"]);
    let title = f.view(cx).panes[0].display.clone();
    assert!(title.ends_with("/left/site.tar.gz"), "{title}");

    // F5 extracts into the other pane (Enter accepts the target).
    f.select(cx, "www");
    f.dispatch(cx, CopyFiles);
    f.keys(cx, "enter");
    let root = f.root.clone();
    f.settle(cx, "the extraction", move |_, _| root.join("right/www/index.html").exists());
    assert_eq!(read(f.root.join("right/www/index.html")), "<h1>hi</h1>");

    // `..` at the archive's root goes back to its folder, onto the archive
    // (the prompt gave the keyboard back to the pane when it closed).
    f.dispatch(cx, GoParent);
    f.settle(cx, "leaving the archive", |view, _| {
        let pane = pane_view(view, Side::Left);
        pane.kind == LocationKind::Local && !pane.loading && cursor_name(pane).as_deref() == Some("site.tar.gz")
    });

    // ⌥F9 unpacks the whole archive into a new folder of the other pane.
    f.dispatch(cx, ExtractArchive);
    f.type_text(cx, "unpacked");
    f.keys(cx, "enter");
    let root = f.root.clone();
    f.settle(cx, "unpacking", move |_, _| {
        root.join("right/unpacked/README").exists() && root.join("right/unpacked/www/index.html").exists()
    });
    assert_eq!(read(f.root.join("right/unpacked/README")), "readme");

    // ⌥F5 packs the selection into a new archive in the other pane.
    f.select(cx, "keep.txt");
    f.dispatch(cx, PackFiles);
    f.keys(cx, "enter");
    let archive = f.root.join("right/keep.tar.gz");
    let path = archive.clone();
    f.settle(cx, "packing", move |_, _| tar_members(&path).contains("keep.txt"));

    // Copying into an archive pane adds to the archive.
    f.select_in(cx, Side::Right, "keep.tar.gz");
    f.dispatch(cx, Open);
    f.settle(cx, "the right archive to open", |view, _| {
        let pane = pane_view(view, Side::Right);
        pane.kind == LocationKind::Archive && !pane.loading && listed(pane) == ["keep.txt"]
    });
    f.select(cx, "site.tar.gz");
    f.dispatch(cx, CopyFiles);
    f.keys(cx, "enter");
    let path = archive.clone();
    f.settle(cx, "adding to the archive", move |_, _| tar_members(&path).contains("site.tar.gz"));
    assert!(tar_members(&archive).contains("keep.txt"), "what was in the archive stays in it");
}

/// A plugin that is written but never registered is invisible in the app,
/// and nothing else would notice.
#[gpui::test]
fn every_provider_is_registered_and_its_fields_make_sense(cx: &mut TestAppContext) {
    let f = open(cx, |_| {});
    let plugins = f.view(cx).plugins.clone();
    let mut ids: Vec<&str> = plugins.iter().map(|p| p.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, ["local", "s3", "ssh", "webdav"]);

    for plugin in plugins.iter() {
        assert!(!plugin.name.is_empty(), "{} has no name", plugin.id);
        let mut keys: Vec<&str> = plugin.fields.iter().map(|f| f.key.as_str()).collect();
        keys.sort();
        let unique = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), unique, "{} repeats a field key", plugin.id);
        // The bookmark editor shows whatever a plugin declares, so a field
        // with no label is a blank row in the dialog.
        assert!(plugin.fields.iter().all(|f| !f.label.is_empty()), "{} has a field with no label", plugin.id);
    }

    // An empty bookmark is what the editor starts with. A plugin with no
    // required field may well accept it — an S3 bookmark with nothing
    // filled in means Amazon with the credentials already on this machine —
    // but one that insists on a field has to say what is missing rather
    // than try to connect. First the plugins' own checks, which the runtime
    // asks when a bookmark is saved…
    let registry = tl_runtime::default_plugins();
    for plugin in plugins.iter().filter(|p| p.fields.iter().any(|f| f.required)) {
        let own = registry.get(&plugin.id).unwrap_or_else(|| panic!("{} is not in the registry", plugin.id));
        assert!(
            own.validate(&Default::default()).is_err(),
            "{} accepts an empty bookmark although a field is required",
            plugin.id
        );
    }

    // …then the editor itself: saving an empty bookmark of such a type
    // keeps the dialog open with the reason under the fields, and saves
    // nothing.
    f.dispatch(cx, NewBookmark);
    let dialog = f.view(cx).dialogs.last().map(|d| d.id).expect("the bookmark editor");
    for plugin in plugins.iter().filter(|p| p.fields.iter().any(|f| f.required)) {
        use tl_app::bookmarks::BookmarksInput;
        f.send(cx, Event::Bookmarks(BookmarksInput::Plugin { dialog, plugin: plugin.id.clone() }));
        f.send(cx, Event::Bookmarks(BookmarksInput::Save { dialog, open: false }));
        let view = f.view(cx);
        let editor = view.dialogs.iter().find(|d| d.id == dialog).map(|d| &d.kind);
        match editor {
            Some(DialogKind::BookmarkEdit(edit)) => {
                assert_eq!(edit.plugin, plugin.id);
                assert!(edit.error.is_some(), "{}: an empty bookmark is saved without a word", plugin.id);
            }
            other => panic!("{}: the editor closed on an empty bookmark ({other:?})", plugin.id),
        }
        assert!(view.bookmarks.bookmarks.is_empty(), "{}: an empty bookmark was saved", plugin.id);
    }
    assert!(f.dialog_open(cx));
}

/// The row of saved folders under the location line: one row per host, and
/// switching connection switches the row.
#[gpui::test]
fn the_places_row_saves_and_switches_folders(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        for dir in ["left/logs", "left/config"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        // A folder saved on a server: it never shows up on this machine's
        // row.
        let mut bookmarks = tl_app::tl_model::Bookmarks::default();
        bookmarks.add_place(tl_core::Place::new("deploy@web-1:22", "etc", tl_core::VPath::parse("/etc")));
        tl_core::bookmark::write(&root.join("state/bookmarks.json"), &bookmarks).unwrap();
    });
    let left = f.root.join("left");
    let enter = |f: &Fixture, cx: &mut TestAppContext, name: &str| {
        f.select(cx, name);
        f.dispatch(cx, Open);
        let want = left.join(name);
        f.settle(cx, "the pane to enter the folder", move |view, _| {
            !pane_view(view, Side::Left).loading && pane_dir(view, Side::Left) == want
        });
    };

    // Nothing is saved for this host, so there is no row to draw (the
    // server's folder is known, just not here).
    assert_eq!(f.view(cx).bookmarks.places.len(), 1, "the server's folder was read");
    assert!(f.places(cx, Side::Left).is_empty());
    let list_top = f.list_top(cx, Side::Left);

    // ⌘D saves the folder the pane is in, named after it.
    enter(&f, cx, "logs");
    f.dispatch(cx, TogglePlace);
    assert_eq!(f.places(cx, Side::Left), [("logs".into(), left.join("logs").display().to_string())]);
    // And the row really takes a strip of the pane, pushing the file list
    // down by its own height.
    let with_row = f.list_top(cx, Side::Left);
    assert!(
        with_row - list_top > 18. && with_row - list_top < 32.,
        "the file list moved by {} points, which is not one row",
        with_row - list_top
    );
    // The other pane is on the same host, so its row has it too.
    assert_eq!(f.places(cx, Side::Right).len(), 1);

    f.dispatch(cx, GoParent);
    enter(&f, cx, "config");
    f.dispatch(cx, TogglePlace);
    let names: Vec<String> = f.places(cx, Side::Left).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["logs", "config"], "saved folders keep their order");

    // ⌘1 and ⌘2 switch between them from anywhere on the host.
    f.dispatch(cx, GoParent);
    let up = left.clone();
    f.settle(cx, "the pane to leave config", move |view, _| pane_dir(view, Side::Left) == up);
    f.dispatch(cx, GoToPlace { index: 0 });
    let logs = left.join("logs");
    f.settle(cx, "⌘1 to go to logs", move |view, _| {
        !pane_view(view, Side::Left).loading && pane_dir(view, Side::Left) == logs
    });
    f.dispatch(cx, GoToPlace { index: 1 });
    let config = left.join("config");
    f.settle(cx, "⌘2 to go to config", move |view, _| {
        !pane_view(view, Side::Left).loading && pane_dir(view, Side::Left) == config
    });
    // A number with nothing behind it does nothing at all.
    f.dispatch(cx, GoToPlace { index: 7 });
    assert_eq!(pane_dir(&f.view(cx), Side::Left), left.join("config"), "the pane stays put");

    // ⌘D again takes the folder off the row, and leaves the other one.
    f.dispatch(cx, TogglePlace);
    let names: Vec<String> = f.places(cx, Side::Left).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["logs"]);

    // Another host's folders never show up here, and are kept all along.
    let view = f.view(cx);
    assert!(view.bookmarks.places.iter().any(|p| p.host == "deploy@web-1:22"));
    let names: Vec<String> = f.places(cx, Side::Left).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["logs"]);
}

/// A marked set of folders, packed and unpacked again.
#[gpui::test]
fn packs_and_extracts_a_set_of_folders(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        write(&root.join("left/one/a.txt"), "a", 1);
        write(&root.join("left/one/deep/b.txt"), "b", 1);
        write(&root.join("left/two/c.txt"), "c", 1);
        write(&root.join("left/loose.txt"), "loose", 1);
    });

    // Mark both folders, leaving the file alone.
    f.focus(cx, Side::Left);
    f.dispatch(cx, MarkByMask);
    f.type_text(cx, "one;two");
    f.keys(cx, "enter");
    assert_eq!(f.names(cx, Side::Left, true), ["one", "two"]);

    // ⌥F5 packs them into one archive in the other pane.
    f.dispatch(cx, PackFiles);
    f.keys(cx, "enter");
    let archive = f.root.join("right/left.tar.gz");
    let path = archive.clone();
    f.settle(cx, "packing the folders", move |_, _| tar_members(&path).contains("two/c.txt"));
    let listing = tar_members(&archive);
    for member in ["one/a.txt", "one/deep/b.txt", "two/c.txt"] {
        assert!(listing.contains(member), "{member} is missing from {listing}");
    }
    assert!(!listing.contains("loose.txt"), "only the marked folders go in: {listing}");

    // ⌥F9 from the other side unpacks the whole thing back.
    f.select_in(cx, Side::Right, "left.tar.gz");
    f.dispatch(cx, ExtractArchive);
    f.type_text(cx, "back");
    f.keys(cx, "enter");
    let root = f.root.clone();
    f.settle(cx, "unpacking the folders", move |_, _| {
        root.join("left/back/one/deep/b.txt").exists() && root.join("left/back/two/c.txt").exists()
    });
    assert_eq!(read(f.root.join("left/back/one/deep/b.txt")), "b");
}

/// Compress and decompress act on the pane they are in, not on the other
/// one.
#[gpui::test]
fn compresses_and_decompresses_in_the_same_folder(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        write(&root.join("left/one/a.txt"), "a", 1);
        write(&root.join("left/two/deep/b.txt"), "b", 1);
    });

    // Mark both folders and compress them where they are.
    f.focus(cx, Side::Left);
    f.dispatch(cx, MarkByMask);
    f.type_text(cx, "one;two");
    f.keys(cx, "enter");
    assert_eq!(f.names(cx, Side::Left, true), ["one", "two"]);
    f.dispatch(cx, Compress);
    f.keys(cx, "enter");
    let archive = f.root.join("left/left.tar.gz");
    let path = archive.clone();
    f.settle(cx, "compressing in place", move |_, _| tar_members(&path).contains("two/deep/b.txt"));
    assert_eq!(
        f.files("right"),
        Vec::<String>::new(),
        "the archive belongs beside the folders, not in the other pane"
    );

    // Decompress proposes a folder named after the archive, so its contents
    // are not strewn over the folder it sits in. Enter accepts that.
    f.select(cx, "left.tar.gz");
    f.dispatch(cx, Decompress);
    f.keys(cx, "enter");
    let root = f.root.clone();
    f.settle(cx, "decompressing in place", move |_, _| {
        root.join("left/left/one/a.txt").exists() && root.join("left/left/two/deep/b.txt").exists()
    });
    assert_eq!(read(f.root.join("left/left/two/deep/b.txt")), "b");
    assert_eq!(f.files("right"), Vec::<String>::new());
}

/// Which pane is the active one has to be obvious at a glance, in either
/// theme. This holds the colours to a measurable gap, because the bug it
/// replaces was exactly a pair of highlights that looked the same.
#[gpui::test]
fn the_active_pane_stands_out_in_both_themes(cx: &mut TestAppContext) {
    use gpui_kit::Hsla;
    use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};

    /// Lightness and colourfulness of `top` drawn over `under`.
    fn over(top: Hsla, under: Hsla) -> (f32, f32) {
        let l = under.l * (1. - top.a) + top.l * top.a;
        let s = under.s * (1. - top.a) + top.s * top.a;
        (l, s)
    }

    let _f = open(cx, |_| {});
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        cx.update(|cx| {
            Theme::change(mode, None, cx);
            let theme = cx.theme().clone();
            let page = theme.background;
            let cursor_here = crate::pane::row_bg(&theme, true, true, false).unwrap();
            let cursor_there = crate::pane::row_bg(&theme, true, false, false).unwrap();
            let (lit, sat) = over(cursor_here, page);
            let (dim, dim_sat) = over(cursor_there, page);

            assert!(
                (lit - dim).abs() > 0.1,
                "{mode:?}: the two cursors are {lit:.3} and {dim:.3} light, which is not a difference anyone can see"
            );
            assert!(
                sat - dim_sat > 0.3,
                "{mode:?}: the active cursor has to carry colour, not just lightness ({sat:.3} against {dim_sat:.3})"
            );
            // And it must stay a background: the row's text sits on it.
            let (text_gap, _) = over(theme.foreground, page);
            assert!(
                (lit - text_gap).abs() > 0.4,
                "{mode:?}: text at {text_gap:.3} on a band at {lit:.3} is unreadable"
            );

            // Marked rows are what an operation acts on, so they cannot be
            // the same colour as everything else.
            let marked = crate::pane::marked_fg(&theme);
            assert!(
                (marked.l - theme.foreground.l).abs() > 0.2 || marked.s > 0.5,
                "{mode:?}: marked text at l={:.3} s={:.3} against plain text at l={:.3}",
                marked.l,
                marked.s,
                theme.foreground.l
            );
            let band = crate::pane::row_bg(&theme, false, false, true).unwrap();
            let (mark_l, mark_s) = over(band, page);
            assert!(
                mark_s > 0.1 && (mark_l - dim).abs() > 0.0,
                "{mode:?}: a marked row needs a tint of its own (l={mark_l:.3} s={mark_s:.3})"
            );
        });
    }
    // Leave the theme as the rest of the suite expects it.
    cx.update(|cx| Theme::change(ThemeMode::Light, None, cx));
}

/// Typing narrows the list to what matches, and the marks and operations
/// work on what is left.
#[gpui::test]
fn typing_filters_the_list(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        write(&root.join("left/app.log"), "a", 1);
        write(&root.join("left/db.log"), "b", 1);
        write(&root.join("left/notes.txt"), "c", 1);
        write(&root.join("left/logrotate.conf"), "d", 1);
        fs::create_dir_all(root.join("left/logs")).unwrap();
        fs::create_dir_all(root.join("left/images")).unwrap();
    });

    f.focus(cx, Side::Left);
    assert_eq!(
        f.names(cx, Side::Left, false),
        ["app.log", "db.log", "images", "logrotate.conf", "logs", "notes.txt"]
    );

    // The whole name is matched, extension included, and folders are in
    // it too.
    f.type_text(cx, "log");
    assert_eq!(f.filter(cx, Side::Left), "log");
    assert_eq!(f.names(cx, Side::Left, false), ["app.log", "db.log", "logrotate.conf", "logs"]);

    // A wildcard matches the name end to end, so only the extension is
    // left — which a plain substring cannot express.
    f.type_text(cx, "s");
    assert_eq!(f.names(cx, Side::Left, false), ["logs"]);
    f.keys(cx, "backspace");
    assert_eq!(f.filter(cx, Side::Left), "log");
    f.type_text(cx, "*.log");
    assert_eq!(f.filter(cx, Side::Left), "log*.log");
    f.keys(cx, "backspace backspace backspace backspace backspace backspace backspace backspace");
    assert_eq!(f.filter(cx, Side::Left), "", "eight characters, eight backspaces");
    f.type_text(cx, "*.log");
    assert_eq!(f.names(cx, Side::Left, false), ["app.log", "db.log"]);

    // Escape puts everything back.
    f.keys(cx, "escape");
    assert_eq!(f.filter(cx, Side::Left), "");
    assert_eq!(f.names(cx, Side::Left, false).len(), 6);

    // With no filter to shorten, backspace is still "go to the enclosing
    // folder".
    f.keys(cx, "backspace");
    let root = f.root.clone();
    f.settle(cx, "the pane to go up", move |view, _| {
        !pane_view(view, Side::Left).loading && pane_dir(view, Side::Left) == root
    });
    assert_eq!(cursor_name(pane_view(&f.view(cx), Side::Left)).as_deref(), Some("left"));
}

/// A filter is a view, so marking inside one and acting on it has to do
/// what it looks like it does.
#[gpui::test]
fn marks_and_operations_follow_the_filter(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        write(&root.join("left/app.log"), "a", 1);
        write(&root.join("left/db.log"), "b", 1);
        write(&root.join("left/notes.txt"), "c", 1);
    });

    f.focus(cx, Side::Left);
    f.type_text(cx, "*.log");
    assert_eq!(f.names(cx, Side::Left, false), ["app.log", "db.log"]);

    // Mark All takes the rows that are showing, not the whole folder.
    f.dispatch(cx, MarkAll);
    assert_eq!(f.names(cx, Side::Left, true), ["app.log", "db.log"]);

    // A mark survives the filter being narrowed and widened again: it
    // belongs to the file, not to the view.
    f.type_text(cx, "x");
    assert_eq!(f.names(cx, Side::Left, false), Vec::<String>::new());
    f.keys(cx, "backspace");
    assert_eq!(f.names(cx, Side::Left, true), ["app.log", "db.log"]);

    // F5 copies what is marked and showing, and leaves the rest behind.
    f.dispatch(cx, CopyFiles);
    f.keys(cx, "enter");
    let root = f.root.clone();
    f.settle(cx, "the copy", move |_, _| root.join("right/db.log").exists() && root.join("right/app.log").exists());
    assert_eq!(f.files("right"), ["app.log", "db.log"]);

    // And the filter is still on, with the marks still in it.
    assert_eq!(f.filter(cx, Side::Left), "*.log");
    assert_eq!(f.names(cx, Side::Left, true), ["app.log", "db.log"]);
}

/// Keystrokes as this platform spells them: GPUI writes the same modifier
/// as `cmd-` on macOS and `super-` on Linux, so a literal cannot be
/// compared against what it hands back.
fn as_spelled<const N: usize>(keys: [&str; N]) -> Vec<String> {
    keys.iter().map(|keys| gpui::Keystroke::parse(keys).unwrap().unparse()).collect()
}

/// The keys GPUI would show for an action in the workspace's context,
/// which is where menu items and tooltips get theirs.
fn advertised(cx: &mut TestAppContext, f: &Fixture, action: &dyn Action) -> Vec<String> {
    f.draw(cx);
    cx.update_window(f.window, |_, window, _| {
        let context = gpui::KeyContext::parse(crate::actions::WORKSPACE).unwrap();
        window
            .bindings_for_action_in_context(action, context)
            .iter()
            .flat_map(|binding| binding.keystrokes())
            .map(|keystroke| keystroke.unparse())
            .collect::<Vec<_>>()
    })
    .unwrap()
}

/// Whether hidden files are shown, as the core has it.
fn hidden(f: &Fixture, cx: &mut TestAppContext) -> bool {
    f.view(cx).settings.general.show_hidden
}

#[gpui::test]
fn a_rebound_key_moves_the_command_and_the_old_key_goes_quiet(cx: &mut TestAppContext) {
    let f = open(cx, |root| write(&root.join("left/.hidden"), "h", 1));
    f.focus(cx, Side::Left);

    assert_eq!(advertised(cx, &f, &ToggleHidden), as_spelled(["cmd-shift-.", "cmd->"]));

    // The default key works, and the file list follows it.
    f.keys(cx, "cmd-shift-.");
    assert!(hidden(&f, cx));
    assert_eq!(f.row_names(cx, Side::Left), [".hidden"]);
    f.keys(cx, "cmd-shift-.");
    assert!(!hidden(&f, cx));
    assert_eq!(f.row_names(cx, Side::Left), Vec::<String>::new());

    // Move it to F9, as the settings window does; the shell rebuilds the
    // key map when the keys in the settings change.
    f.edit(
        cx,
        SettingsEdit::Keys {
            command: "toggle-hidden".into(),
            keys: Some(vec!["f9".into()]),
        },
    );

    f.keys(cx, "f9");
    assert!(hidden(&f, cx), "F9 is the key now");
    f.keys(cx, "cmd-shift-.");
    assert!(hidden(&f, cx), "the old key must do nothing, not toggle it back");

    // Menus and tooltips look the binding up in GPUI's key map rather than
    // in the settings, so it has to agree about which key is gone.
    assert_eq!(advertised(cx, &f, &ToggleHidden), as_spelled(["f9"]));

    // And the defaults come back.
    f.edit(
        cx,
        SettingsEdit::Keys {
            command: "toggle-hidden".into(),
            keys: None,
        },
    );
    f.keys(cx, "cmd-shift-.");
    assert!(!hidden(&f, cx));
    f.keys(cx, "f9");
    assert!(!hidden(&f, cx), "F9 is free again");
}

#[gpui::test]
fn applying_the_same_key_map_again_changes_nothing(cx: &mut TestAppContext) {
    // Every row of the keys page resets through `apply`, so a page reset
    // calls it once per command: it has to be free when nothing changed,
    // or the key map would grow by a few hundred bindings each time.
    let f = open(cx, |_| {});
    let count = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let f5 = vec![gpui::Keystroke::parse("f5").unwrap()];
            cx.all_bindings_for_input(&f5).len()
        })
    };
    let before = count(cx);
    assert!(before > 0, "F5 is bound to copying files");
    for _ in 0..5 {
        cx.update(crate::keymap::apply);
    }
    assert_eq!(count(cx), before);

    // A real change does add to it, and still leaves one binding in force.
    f.edit(
        cx,
        SettingsEdit::Keys {
            command: "copy-files".into(),
            keys: Some(vec!["f9".into()]),
        },
    );
    assert!(count(cx) > before, "the marker that takes F5 away is a binding too");
    let live = cx
        .update_window(f.window, |_, window, _| {
            let context = gpui::KeyContext::parse(crate::actions::PANE).unwrap();
            window.bindings_for_action_in_context(&CopyFiles, context).len()
        })
        .unwrap();
    assert_eq!(live, 1, "exactly one key copies files");
}

#[gpui::test]
fn a_setting_changed_from_the_keyboard_reaches_the_file(cx: &mut TestAppContext) {
    let f = open(cx, |root| write(&root.join("left/.hidden"), "h", 1));
    f.focus(cx, Side::Left);
    assert!(!f.saved_settings().general.show_hidden);
    f.keys(cx, "cmd-shift-.");
    assert!(
        f.saved_settings().general.show_hidden,
        "a setting is written as it changes, not only when the app quits"
    );
}

#[gpui::test]
fn folders_can_be_sorted_in_with_the_files(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        write(&root.join("left/a.txt"), "a", 1);
        write(&root.join("left/m-dir/x.txt"), "x", 1);
        write(&root.join("left/z.txt"), "z", 1);
    });
    assert_eq!(f.row_names(cx, Side::Left), ["m-dir", "a.txt", "z.txt"]);
    let loads = |f: &Fixture, cx: &mut TestAppContext| f.view(cx).panes[0].listing.entries.clone();
    let listing = loads(&f, cx);

    // No reload: the panes rebuild their rows when the setting changes.
    f.edit(cx, SettingsEdit::FoldersFirst(false));
    assert_eq!(f.row_names(cx, Side::Left), ["a.txt", "m-dir", "z.txt"]);
    assert!(std::sync::Arc::ptr_eq(&listing, &loads(&f, cx)), "the folder was listed again");
}

#[gpui::test]
fn deleting_permanently_asks_even_when_trashing_does_not(cx: &mut TestAppContext) {
    let f = open(cx, |root| write(&root.join("left/gone.txt"), "g", 1));
    f.edit(cx, SettingsEdit::ConfirmTrash(false));
    f.select(cx, "gone.txt");
    f.dispatch(cx, DeleteFiles);
    assert!(
        matches!(f.view(cx).dialogs.last().map(|d| &d.kind), Some(DialogKind::Confirm(_))),
        "a delete that cannot be undone has to be confirmed, whatever the setting says"
    );
    assert!(f.dialog_open(cx), "and the window shows the question");
    assert_eq!(f.files("left"), ["gone.txt"], "and nothing is gone yet");
}

#[gpui::test]
fn the_chosen_theme_paints_the_window(cx: &mut TestAppContext) {
    use gpui_kit::component::{ActiveTheme as _, Theme};

    let f = open(cx, |_| {});
    for (mode, dark, name, background) in [
        (AppearanceMode::Dark, true, "Gruvbox Dark", "#282828"),
        (AppearanceMode::Light, false, "Solarized Light", "#fdf6e3"),
    ] {
        // What the settings window sends; the shell applies the theme when
        // the settings come back changed.
        if dark {
            f.edit(cx, SettingsEdit::DarkTheme(name.into()));
        } else {
            f.edit(cx, SettingsEdit::LightTheme(name.into()));
        }
        f.edit(cx, SettingsEdit::AppearanceMode(mode));
        let font_size = f.view(cx).settings.general.font_size();
        cx.update(|cx| {
            assert_eq!(cx.theme().theme_name().as_ref(), name);
            assert_eq!(hex(cx.theme().background), background);
            // A theme file carries no font size, so the one the settings
            // ask for has to survive being applied.
            assert_eq!(f32::from(cx.theme().font_size), font_size);
            assert_eq!(Theme::global(cx).is_dark(), dark);
        });
    }

    // An unknown theme in a hand-edited file falls back instead of failing.
    f.edit(cx, SettingsEdit::AppearanceMode(AppearanceMode::Dark));
    f.edit(cx, SettingsEdit::DarkTheme("Nonesuch".into()));
    cx.update(|cx| assert_eq!(cx.theme().theme_name().as_ref(), crate::themes::DEFAULT_DARK));
}

/// A colour as the theme file writes it.
fn hex(color: gpui_kit::Hsla) -> String {
    let rgba = color.to_rgb();
    let byte = |v: f32| (v * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}", byte(rgba.r), byte(rgba.g), byte(rgba.b))
}

#[gpui::test]
fn the_settings_window_opens_once_and_draws(cx: &mut TestAppContext) {
    let f = open(cx, |_| {});
    let before = cx.update(|cx| cx.windows().len());
    // ⌘, and the menu send the command to the core, which asks the shell
    // for the window.
    f.dispatch(cx, OpenSettings);
    let handle = cx.update(|cx| cx.global::<Shell>().settings_window);
    let handle = handle.expect("the settings window is remembered");
    assert_eq!(cx.update(|cx| cx.windows().len()), before + 1);

    // Every page, every group and every key row is built here: this is
    // where a mistake in one of them would show up.
    cx.update_window(handle, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    cx.run_until_parked();

    // Asking again brings that window forward rather than opening a second.
    f.dispatch(cx, OpenSettings);
    assert_eq!(cx.update(|cx| cx.windows().len()), before + 1);
    assert_eq!(cx.update(|cx| cx.global::<Shell>().settings_window), Some(handle));
}

#[gpui::test]
fn quick_look_previews_what_the_cursor_is_on(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        write(&root.join("left/notes.txt"), "the quick brown fox\n", 1);
        write(&root.join("left/zzz.bin"), "head\0tail", 1);
        write(&root.join("left/sub/inner.txt"), "inner", 1);
    });
    f.select(cx, "notes.txt");
    assert!(f.quick_look_side(cx).is_none());

    f.dispatch(cx, ToggleQuickLook);
    // The preview takes the pane opposite the one with the keyboard.
    assert_eq!(f.quick_look_side(cx), Some(Side::Right));
    assert_eq!(f.preview_name(cx).as_deref(), Some("notes.txt"));
    assert_eq!(f.preview(cx), Preview::Text("the quick brown fox\n".into()));

    // It follows the cursor, and says what it cannot show.
    f.select(cx, "zzz.bin");
    assert_eq!(f.preview(cx), Preview::Note("Binary file".into()));
    f.select(cx, "sub");
    assert_eq!(f.preview(cx), Preview::Note("Folder".into()));

    // Walking into a folder keeps the mode, and the preview follows. The
    // cursor lands on the row above the folder, which has nothing to show.
    f.keys(cx, "enter");
    f.settle(cx, "the folder to open", |view, _| {
        let pane = pane_view(view, Side::Left);
        !pane.loading && listed(pane) == ["inner.txt"]
    });
    assert_eq!(f.preview(cx), Preview::Nothing);
    f.keys(cx, "down");
    assert_eq!(f.preview_name(cx).as_deref(), Some("inner.txt"));
    assert_eq!(f.preview(cx), Preview::Text("inner".into()));
    assert_eq!(f.quick_look_side(cx), Some(Side::Right));

    // And ⌘Y again gives the pane back.
    f.dispatch(cx, ToggleQuickLook);
    assert!(f.quick_look_side(cx).is_none());
}

#[gpui::test]
fn quick_look_moves_to_the_other_side_with_the_keyboard(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        write(&root.join("left/left.txt"), "on the left", 1);
        write(&root.join("right/right.txt"), "on the right", 1);
    });
    f.select(cx, "left.txt");
    f.dispatch(cx, ToggleQuickLook);
    assert_eq!(f.quick_look_side(cx), Some(Side::Right));
    assert_eq!(f.preview(cx), Preview::Text("on the left".into()));

    // Tab hands the keyboard to the other folder; the preview changes
    // sides with it rather than leaving the pane it was in undrawn.
    f.keys(cx, "tab");
    f.settle(cx, "the right pane to take the keyboard", |view, _| view.active == Side::Right);
    assert_eq!(f.quick_look_side(cx), Some(Side::Left));
    // The right pane's cursor starts on the row above the folder, so the
    // preview empties rather than keeping the file from the other side.
    assert_eq!(f.preview(cx), Preview::Nothing);
    f.keys(cx, "down");
    assert_eq!(f.preview(cx), Preview::Text("on the right".into()));

    // The file list still has the keys: marking works where the cursor is.
    f.dispatch(cx, ToggleMark);
    assert_eq!(f.names(cx, Side::Right, true), ["right.txt"]);
}

#[gpui::test]
fn quick_look_reads_an_image_as_an_image(cx: &mut TestAppContext) {
    // The smallest PNG that decodes: a single transparent pixel.
    const PIXEL: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00,
        0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d,
        0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let f = open(cx, |root| {
        fs::write(root.join("left/dot.png"), PIXEL).unwrap();
        fs::write(root.join("left/empty.txt"), "").unwrap();
    });
    f.select(cx, "dot.png");
    f.dispatch(cx, ToggleQuickLook);
    assert_eq!(f.preview(cx), Preview::Image(PIXEL.len()));

    f.select(cx, "empty.txt");
    assert_eq!(f.preview(cx), Preview::Note("Empty file".into()));
}

#[gpui::test]
fn a_folder_that_changes_mid_listing_still_lands_the_cursor(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        write(&root.join("left/a.txt"), "a", 1);
        write(&root.join("left/sub/inner.txt"), "i", 1);
        write(&root.join("left/target.txt"), "t", 1);
    });
    f.select(cx, "sub");
    f.dispatch(cx, Open);
    let sub = f.root.join("left/sub");
    f.settle(cx, "the pane to enter sub", move |view, _| {
        !pane_view(view, Side::Left).loading && pane_dir(view, Side::Left) == sub
    });
    f.select(cx, "inner.txt");

    // Going up lists the folder with the cursor to land on the one just
    // left. The folder changes before that listing comes back — a job
    // finishing, or anything else the watcher sees — and the listing starts
    // again: both are sent before either is answered.
    f.draw(cx);
    cx.dispatch_action(f.window, GoParent);
    cx.dispatch_action(f.window, Refresh);

    let left = f.root.join("left");
    f.settle(cx, "the listing", move |view, _| {
        !pane_view(view, Side::Left).loading && pane_dir(view, Side::Left) == left
    });
    assert_eq!(
        cursor_name(pane_view(&f.view(cx), Side::Left)).as_deref(),
        Some("sub"),
        "the second listing dropped the cursor the first was asked to place"
    );
}

/// The labels of the actions in a menu, submenus left out.
fn menu_labels(menu: &gpui::OwnedMenu) -> Vec<String> {
    menu.items
        .iter()
        .filter_map(|item| match item {
            gpui::OwnedMenuItem::Action { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn the_menu_bar_carries_what_the_desktop_expects(cx: &mut TestAppContext) {
    let _f = open(cx, |_| {});
    let menus = cx.update(|cx| cx.get_menus()).expect("the menu bar is set");
    let names: Vec<String> = menus.iter().map(|menu| menu.name.to_string()).collect();

    // macOS takes over the menu it finds under each of these names: the
    // list of open windows goes in one, its help search field in the other.
    assert_eq!(names.first().map(String::as_str), Some("Translocator"));
    assert!(names.contains(&"Window".to_string()), "{names:?}");
    assert!(names.contains(&"Help".to_string()), "{names:?}");

    let app_menu = menu_labels(&menus[0]);
    for item in ["About Translocator", "Settings…", "Quit Translocator"] {
        assert!(app_menu.contains(&item.to_string()), "{app_menu:?}");
    }
    // Hiding is a macOS idea, and there is nothing to hide behind elsewhere.
    for item in ["Hide Translocator", "Hide Others", "Show All"] {
        assert_eq!(app_menu.contains(&item.to_string()), cfg!(target_os = "macos"), "{item} in {app_menu:?}");
    }

    let window = menus.iter().find(|menu| menu.name == "Window").unwrap();
    assert_eq!(menu_labels(window), ["Minimize", "Zoom"]);
    let help = menus.iter().find(|menu| menu.name == "Help").unwrap();
    assert_eq!(menu_labels(help), ["Translocator Help"]);
}

#[gpui::test]
fn the_system_keys_are_bound_where_macos_expects_them(cx: &mut TestAppContext) {
    let _f = open(cx, |_| {});
    let bound = |cx: &mut TestAppContext, keys: &str| {
        cx.update(|cx| {
            let stroke = vec![gpui::Keystroke::parse(keys).unwrap()];
            cx.all_bindings_for_input(&stroke)
                .iter()
                .map(|binding| binding.action().name().to_string())
                .collect::<Vec<_>>()
        })
    };
    if cfg!(target_os = "macos") {
        assert!(bound(cx, "cmd-h").contains(&"tl::Hide".to_string()));
        assert!(bound(cx, "cmd-alt-h").contains(&"tl::HideOthers".to_string()));
        assert!(bound(cx, "cmd-m").contains(&"tl::Minimize".to_string()));
    } else {
        assert!(bound(cx, "cmd-h").is_empty(), "hiding is a macOS idea");
    }
    // And they take none of the keys the file manager already uses.
    for (keys, action) in [("cmd-shift-h", "tl::GoHome"), ("cmd-shift-m", "tl::MultiRename")] {
        assert!(bound(cx, keys).contains(&action.to_string()), "{keys} lost {action}");
    }
}
