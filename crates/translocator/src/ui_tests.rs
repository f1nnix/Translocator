//! Headless UI tests: the real workspace in a GPUI test window, driven
//! through its actions, dialogs and the keyboard, while the file operations
//! run for real on temporary folders.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui::{Action, AnyWindowHandle, App, AppContext as _, Bounds, Entity, Point, TestAppContext, WindowBounds, WindowOptions, px, size};

use crate::actions::*;
use crate::app_state::{self, Settings};
use crate::pane::{FilePane, Side};
use crate::workspace::Workspace;

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    window: AnyWindowHandle,
    ws: Entity<Workspace>,
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

/// Opens the workspace on `<tmp>/left` and `<tmp>/right` after `build`
/// filled the folders.
fn open(cx: &mut TestAppContext, build: impl FnOnce(&Path)) -> Fixture {
    // File operations run on real worker threads.
    cx.executor().allow_parking();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    for dir in ["left", "right", "state"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    build(&root);
    let settings = Settings {
        show_hidden: false,
        left_path: Some(root.join("left").display().to_string()),
        right_path: Some(root.join("right").display().to_string()),
    };
    let state = root.join("state");
    let (window, ws) = cx.update(|cx| {
        gpui_kit::init(cx);
        app_state::init_for_tests(cx, &state, settings);
        bind_keys(cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: size(px(1280.), px(820.)),
            })),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Workspace::new(window, cx))).expect("test window")
    });
    let fixture = Fixture {
        _tmp: tmp,
        root,
        window,
        ws,
    };
    fixture.settle(cx, "both panes to load", |ws, cx| {
        [Side::Left, Side::Right].iter().all(|s| {
            let pane = ws.pane(*s).read(cx);
            !pane.is_loading()
        })
    });
    fixture
}

impl Fixture {
    /// Draws, runs the UI and the worker threads until `done` holds. The
    /// test clock moves on too, so animations and timers complete.
    fn settle(&self, cx: &mut TestAppContext, what: &str, mut done: impl FnMut(&Workspace, &App) -> bool) {
        for _ in 0..2000 {
            self.draw(cx);
            cx.executor().advance_clock(Duration::from_millis(20));
            cx.run_until_parked();
            let ws = self.ws.clone();
            if cx.update(|cx| done(ws.read(cx), cx)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for {what}");
    }

    fn draw(&self, cx: &mut TestAppContext) {
        cx.update_window(self.window, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    }

    /// Dispatches `action` to the focused element, like its key binding.
    fn dispatch(&self, cx: &mut TestAppContext, action: impl Action) {
        self.draw(cx);
        cx.dispatch_action(self.window, action);
        self.draw(cx);
        cx.run_until_parked();
    }

    fn keys(&self, cx: &mut TestAppContext, keys: &str) {
        self.draw(cx);
        cx.simulate_keystrokes(self.window, keys);
        // Let a closing dialog finish its animation and hand focus back.
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
    }

    fn type_text(&self, cx: &mut TestAppContext, text: &str) {
        self.draw(cx);
        cx.simulate_input(self.window, text);
        cx.run_until_parked();
    }

    /// Focuses a pane, as clicking into it would.
    fn focus(&self, cx: &mut TestAppContext, side: Side) {
        let pane = self.pane(cx, side);
        cx.update_window(self.window, |_, window, cx| {
            let focus = pane.read(cx).focus.clone();
            window.focus(&focus, cx);
        })
        .unwrap();
        cx.run_until_parked();
    }

    fn pane(&self, cx: &mut TestAppContext, side: Side) -> Entity<FilePane> {
        let ws = self.ws.clone();
        cx.update(|cx| ws.read(cx).pane(side).clone())
    }

    fn names(&self, cx: &mut TestAppContext, side: Side, marked: bool) -> Vec<String> {
        let pane = self.pane(cx, side);
        cx.update(|cx| {
            let pane = pane.read(cx);
            let names = if marked {
                pane.marked()
            } else {
                pane.listed_entries().into_iter().map(|e| e.name).collect()
            };
            let mut names: Vec<String> = names.iter().map(|n| String::from_utf8_lossy(n).into_owned()).collect();
            names.sort();
            names
        })
    }

    /// Focuses the left pane and puts its cursor on `name`.
    fn select(&self, cx: &mut TestAppContext, name: &str) {
        self.focus(cx, Side::Left);
        let pane = self.pane(cx, Side::Left);
        cx.update(|cx| pane.update(cx, |p, cx| p.reload_select(name.as_bytes().to_vec(), cx)));
        let expected = name.to_string();
        self.settle(cx, "the cursor to move", move |ws, cx| {
            let pane = ws.pane(Side::Left).read(cx);
            !pane.is_loading() && pane.cursor_row().is_some_and(|r| r.name.as_ref() == expected)
        });
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

#[gpui::test]
fn multi_rename_renames_the_marked_files(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        write(&root.join("left/IMG_1.JPG"), "1", 2);
        write(&root.join("left/IMG_2.JPG"), "2", 1);
        write(&root.join("left/keep.txt"), "k", 1);
    });
    let pane = f.pane(cx, Side::Left);
    cx.update(|cx| pane.update(cx, |p, cx| p.set_marks(vec![b"IMG_1.JPG".to_vec(), b"IMG_2.JPG".to_vec()], cx)));
    f.focus(cx, Side::Left);
    f.dispatch(cx, MultiRename);
    // The name mask has focus with its text selected: typing replaces it.
    f.type_text(cx, "photo-[C]");
    f.keys(cx, "enter");
    let root = f.root.clone();
    f.settle(cx, "the renames", move |_, _| root.join("left/photo-2.JPG").exists());
    assert_eq!(f.files("left"), ["keep.txt", "photo-1.JPG", "photo-2.JPG"]);
    assert_eq!(read(f.root.join("left/photo-1.JPG")), "1");
    // The pane lists the new names.
    f.settle(cx, "the pane to reload", |ws, cx| {
        let pane = ws.pane(Side::Left).read(cx);
        !pane.is_loading() && pane.listed_entries().iter().any(|e| e.name == b"photo-1.JPG")
    });
}

#[gpui::test]
fn multi_rename_keeps_the_dialog_open_on_clashes(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        write(&root.join("left/a.txt"), "a", 1);
        write(&root.join("left/b.txt"), "b", 1);
    });
    let pane = f.pane(cx, Side::Left);
    cx.update(|cx| pane.update(cx, |p, cx| p.set_marks(vec![b"a.txt".to_vec(), b"b.txt".to_vec()], cx)));
    f.focus(cx, Side::Left);
    f.dispatch(cx, MultiRename);
    // Both would be called "same.txt": Enter must not rename (nor close).
    f.type_text(cx, "same");
    f.keys(cx, "enter");
    for _ in 0..20 {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(f.files("left"), ["a.txt", "b.txt"]);
    let open = cx.update_window(f.window, |_, window, cx| gpui_kit::component::WindowExt::has_active_dialog(window, cx));
    assert_eq!(open.ok(), Some(true));
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
    let root = f.root.clone();
    for _ in 0..400 {
        f.keys(cx, "enter");
        if root.join("right/a.txt").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    f.settle(cx, "the sync", move |_, _| {
        root.join("left/b.txt").exists()
            && root.join("right/new-dir/deep/x.txt").exists()
            && read(root.join("right/docs/n.txt")) == "newer"
    });
    assert_eq!(read(f.root.join("left/docs/n.txt")), "newer");
    assert_eq!(read(f.root.join("right/a.txt")), "only left");
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
    f.settle(cx, "the archive to open", |ws, cx| {
        let pane = ws.pane(Side::Left).read(cx);
        pane.location().vfs.host().is_some() && !pane.is_loading() && pane.listed_entries().len() == 2
    });
    assert_eq!(f.names(cx, Side::Left, false), ["README", "www"]);
    let title = cx.update(|cx| f.ws.read(cx).pane(Side::Left).read(cx).location().display());
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
    f.settle(cx, "leaving the archive", |ws, cx| {
        let pane = ws.pane(Side::Left).read(cx);
        pane.location().vfs.host().is_none()
            && !pane.is_loading()
            && pane.cursor_row().is_some_and(|r| r.name.as_ref() == "site.tar.gz")
    });

    // ⌥F9 unpacks the whole archive into a new folder of the other pane.
    f.dispatch(cx, ExtractArchive);
    f.type_text(cx, "unpacked");
    f.keys(cx, "enter");
    let root = f.root.clone();
    f.settle(cx, "unpacking", move |_, _| {
        root.join("right/unpacked/README").exists() && root.join("right/unpacked/www/index.html").exists()
    });

    // ⌥F5 packs the selection into a new archive in the other pane.
    f.select(cx, "keep.txt");
    f.dispatch(cx, PackFiles);
    f.keys(cx, "enter");
    let archive = f.root.join("right/keep.tar.gz");
    let path = archive.clone();
    f.settle(cx, "packing", move |_, _| {
        std::process::Command::new("tar")
            .arg("-tzf")
            .arg(&path)
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("keep.txt"))
    });

    // Copying into an archive pane adds to the archive.
    let right = f.pane(cx, Side::Right);
    let file = tl_core::VPath::from_local(&archive);
    cx.update(|cx| right.update(cx, |p, cx| p.navigate(file.parent().unwrap(), Some(b"keep.tar.gz".to_vec()), cx)));
    f.settle(cx, "the right pane", |ws, cx| !ws.pane(Side::Right).read(cx).is_loading());
    f.focus(cx, Side::Right);
    f.dispatch(cx, Open);
    f.settle(cx, "the right archive to open", |ws, cx| {
        let pane = ws.pane(Side::Right).read(cx);
        pane.location().vfs.host().is_some() && !pane.is_loading()
    });
    f.select(cx, "site.tar.gz");
    f.dispatch(cx, CopyFiles);
    f.keys(cx, "enter");
    let path = archive.clone();
    f.settle(cx, "adding to the archive", move |_, _| {
        std::process::Command::new("tar")
            .arg("-tzf")
            .arg(&path)
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("site.tar.gz"))
    });
}
