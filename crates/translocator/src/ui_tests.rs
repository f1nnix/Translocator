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
    // gpui reports an empty focus path while a window is inactive, and a
    // pane learns it is the active one from a focus event, so without this
    // every test would run with the left pane active whatever it focused.
    cx.update_window(window, |_, window, _| window.activate_window()).unwrap();
    cx.run_until_parked();
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
        // The workspace learns which pane is active from a focus event,
        // which gpui dispatches at the end of the next frame.
        self.settle(cx, "the pane to become the active one", move |ws, _| ws.active_side() == side);
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

    /// The saved folders of the host a pane is on, as (name, path).
    fn places(&self, cx: &mut TestAppContext, side: Side) -> Vec<(String, String)> {
        let pane = self.pane(cx, side);
        cx.update(|cx| {
            let host = pane.read(cx).host_key();
            cx.global::<app_state::AppState>()
                .bookmarks
                .places(&host)
                .into_iter()
                .map(|p| (p.name, p.path.display()))
                .collect()
        })
    }

    /// Where the file list starts, which says whether a row above it is
    /// really being drawn.
    fn list_top(&self, cx: &mut TestAppContext, side: Side) -> f32 {
        self.draw(cx);
        let pane = self.pane(cx, side);
        cx.update(|cx| f32::from(pane.read(cx).list_origin.y))
    }

    /// Whether the pane draws the row at all.
    fn has_places(&self, cx: &mut TestAppContext, side: Side) -> bool {
        let pane = self.pane(cx, side);
        cx.update(|cx| pane.read(cx).has_places(cx))
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

/// A plugin that is written but never registered is invisible in the app,
/// and nothing else would notice.
#[gpui::test]
fn every_provider_is_registered_and_its_fields_make_sense(cx: &mut TestAppContext) {
    let f = open(cx, |_| {});
    let _ = &f;
    cx.update(|cx| {
        let registry = &cx.global::<app_state::AppState>().registry;
        let mut ids: Vec<&str> = registry.all().iter().map(|p| p.id()).collect();
        ids.sort();
        assert_eq!(ids, ["local", "s3", "ssh", "webdav"]);

        for plugin in registry.all() {
            let fields = plugin.fields();
            assert!(!plugin.name().is_empty(), "{} has no name", plugin.id());
            let mut keys: Vec<&str> = fields.iter().map(|f| f.key).collect();
            keys.sort();
            let unique = keys.len();
            keys.dedup();
            assert_eq!(keys.len(), unique, "{} repeats a field key", plugin.id());
            // The bookmark editor shows whatever a plugin declares, so a
            // field with no label is a blank row in the dialog.
            assert!(
                fields.iter().all(|f| !f.label.is_empty()),
                "{} has a field with no label",
                plugin.id()
            );
            // An empty bookmark is what the editor starts with. A plugin
            // with no required field may well accept it — an S3 bookmark
            // with nothing filled in means Amazon with the credentials
            // already on this machine — but one that insists on a field
            // has to say what is missing rather than try to connect.
            if fields.iter().any(|f| f.required) {
                assert!(
                    plugin.validate(&Default::default()).is_err(),
                    "{} accepts an empty bookmark although a field is required",
                    plugin.id()
                );
            }
        }
    });
}

/// The row of saved folders under the location line: one row per host, and
/// switching connection switches the row.
#[gpui::test]
fn the_places_row_saves_and_switches_folders(cx: &mut TestAppContext) {
    let f = open(cx, |root| {
        for dir in ["left/logs", "left/config"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
    });
    let left = f.root.join("left");
    let enter = |f: &Fixture, cx: &mut TestAppContext, name: &str| {
        f.select(cx, name);
        f.dispatch(cx, Open);
        let want = tl_core::VPath::from_local(&left.join(name));
        f.settle(cx, "the pane to enter the folder", move |ws, cx| {
            let pane = ws.pane(Side::Left).read(cx);
            !pane.is_loading() && pane.location().path == want
        });
    };

    // Nothing is saved yet, so there is no row to draw.
    assert!(!f.has_places(cx, Side::Left));
    let list_top = f.list_top(cx, Side::Left);

    // ⌘D saves the folder the pane is in, named after it.
    enter(&f, cx, "logs");
    f.dispatch(cx, TogglePlace);
    assert_eq!(
        f.places(cx, Side::Left),
        [("logs".into(), left.join("logs").display().to_string())]
    );
    assert!(f.has_places(cx, Side::Left));
    // And the row really takes a strip of the pane, pushing the file list
    // down by its own height.
    f.settle(cx, "the row to appear", |_, _| true);
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
    f.settle(cx, "the pane to leave config", |ws, cx| {
        !ws.pane(Side::Left).read(cx).is_loading()
    });
    f.dispatch(cx, GoToPlace { index: 0 });
    f.settle(cx, "⌘1 to go to logs", |ws, cx| {
        let pane = ws.pane(Side::Left).read(cx);
        !pane.is_loading() && pane.location().path.file_name() == Some(b"logs")
    });
    f.dispatch(cx, GoToPlace { index: 1 });
    f.settle(cx, "⌘2 to go to config", |ws, cx| {
        let pane = ws.pane(Side::Left).read(cx);
        !pane.is_loading() && pane.location().path.file_name() == Some(b"config")
    });
    // A number with nothing behind it does nothing at all.
    f.dispatch(cx, GoToPlace { index: 7 });
    f.settle(cx, "the pane to stay put", |ws, cx| {
        let pane = ws.pane(Side::Left).read(cx);
        !pane.is_loading() && pane.location().path.file_name() == Some(b"config")
    });

    // ⌘D again takes the folder off the row, and leaves the other one.
    f.dispatch(cx, TogglePlace);
    let names: Vec<String> = f.places(cx, Side::Left).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["logs"]);

    // Another host's folders never show up here.
    cx.update(|cx| {
        let place = tl_core::Place::new("deploy@web-1:22", "etc", tl_core::VPath::parse("/etc"));
        cx.global_mut::<app_state::AppState>().bookmarks.add_place(place).unwrap();
    });
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
    f.settle(cx, "packing the folders", move |_, _| {
        std::process::Command::new("tar")
            .arg("-tzf")
            .arg(&path)
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("two/c.txt"))
    });
    let listing = String::from_utf8_lossy(
        &std::process::Command::new("tar").arg("-tzf").arg(&archive).output().unwrap().stdout,
    )
    .into_owned();
    for member in ["one/a.txt", "one/deep/b.txt", "two/c.txt"] {
        assert!(listing.contains(member), "{member} is missing from {listing}");
    }
    assert!(!listing.contains("loose.txt"), "only the marked folders go in: {listing}");

    // ⌥F9 from the other side unpacks the whole thing back.
    f.focus(cx, Side::Right);
    let right = f.pane(cx, Side::Right);
    cx.update(|cx| right.update(cx, |p, cx| p.reload_select(b"left.tar.gz".to_vec(), cx)));
    f.settle(cx, "the cursor on the archive", |ws, cx| {
        let pane = ws.pane(Side::Right).read(cx);
        !pane.is_loading() && pane.cursor_row().is_some_and(|r| r.name.as_ref() == "left.tar.gz")
    });
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
    f.settle(cx, "compressing in place", move |_, _| {
        std::process::Command::new("tar")
            .arg("-tzf")
            .arg(&path)
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("two/deep/b.txt"))
    });
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
    use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
    use gpui_kit::Hsla;

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
    let filter = |cx: &mut TestAppContext| {
        let pane = f.pane(cx, Side::Left);
        cx.update(|cx| pane.read(cx).filter().to_string())
    };

    f.focus(cx, Side::Left);
    assert_eq!(
        f.names(cx, Side::Left, false),
        ["app.log", "db.log", "images", "logrotate.conf", "logs", "notes.txt"]
    );

    // The whole name is matched, extension included, and folders are in
    // it too.
    f.type_text(cx, "log");
    assert_eq!(filter(cx), "log");
    assert_eq!(
        f.names(cx, Side::Left, false),
        ["app.log", "db.log", "logrotate.conf", "logs"]
    );

    // A wildcard matches the name end to end, so only the extension is
    // left — which a plain substring cannot express.
    f.type_text(cx, "s");
    assert_eq!(f.names(cx, Side::Left, false), ["logs"]);
    f.keys(cx, "backspace");
    assert_eq!(filter(cx), "log");
    f.type_text(cx, "*.log");
    assert_eq!(filter(cx), "log*.log");
    f.keys(cx, "backspace backspace backspace backspace backspace backspace backspace backspace");
    assert_eq!(filter(cx), "", "eight characters, eight backspaces");
    f.type_text(cx, "*.log");
    assert_eq!(f.names(cx, Side::Left, false), ["app.log", "db.log"]);

    // Escape puts everything back.
    f.keys(cx, "escape");
    assert_eq!(filter(cx), "");
    assert_eq!(f.names(cx, Side::Left, false).len(), 6);

    // With no filter to shorten, backspace is still "go to the enclosing
    // folder".
    f.keys(cx, "backspace");
    f.settle(cx, "the pane to go up", |ws, cx| {
        let pane = ws.pane(Side::Left).read(cx);
        !pane.is_loading() && pane.location().path.file_name() != Some(b"left")
    });
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
    f.settle(cx, "the copy", move |_, _| root.join("right/db.log").exists());
    assert_eq!(f.files("right"), ["app.log", "db.log"]);

    // And the filter is still on, with the marks still in it.
    let pane = f.pane(cx, Side::Left);
    assert_eq!(cx.update(|cx| pane.read(cx).filter().to_string()), "*.log");
    assert_eq!(f.names(cx, Side::Left, true), ["app.log", "db.log"]);
}
