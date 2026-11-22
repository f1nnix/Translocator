//! The FFI layer over a real runtime and the real core, on temporary
//! folders: what the snapshot and rows say, and when the versions move.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tl_runtime::{Config, Runtime, Ui};

use crate::Core;
use crate::input::{Command, Input};
use crate::rows::IconKind;
use crate::snapshot::Snapshot;
use crate::types::Side;

struct Quiet;

impl Ui for Quiet {
    fn render(&self) {}
    fn toast(&self, _: tl_runtime::ToastKind, _: String) {}
    fn write_clipboard(&self, _: String) {}
    fn open(&self, _: &Path) {}
    fn reveal(&self, _: &Path) {}
    fn open_url(&self, _: &str) {}
    fn quit(&self) {}
}

const WAIT: Duration = Duration::from_secs(15);

struct Fixture {
    _tmp: tempfile::TempDir,
    rt: Runtime,
    core: Arc<Core>,
}

impl Fixture {
    fn start() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (data, left, right) = (root.join("data"), root.join("left"), root.join("right"));
        for dir in [&data, &left, &right, &left.join("dir")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(left.join("a.txt"), b"abc").unwrap();
        std::fs::write(left.join("b.rs"), b"fn main() {}").unwrap();
        std::fs::write(left.join(".hidden"), b"").unwrap();
        let settings = serde_json::json!({
            "session": { "left_path": left.to_str().unwrap(), "right_path": right.to_str().unwrap() }
        });
        std::fs::write(data.join("settings.json"), settings.to_string()).unwrap();
        let config = Config {
            data_dir: data,
            cache_dir: root.join("cache"),
            runtime_dir: Some(root.join("run")),
            askpass_exe: None,
            plugins: None,
        };
        let rt = Runtime::start(config, Arc::new(Quiet));
        let core = Core::with_runtime(rt.clone());
        let f = Fixture { _tmp: tmp, rt, core };
        f.wait(|s| s.ready && !s.left.loading && !s.right.loading && s.left.row_count == 4);
        f
    }

    fn wait(&self, done: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(self.rt.settle(WAIT));
            let snap = self.core.snapshot();
            if done(&snap) {
                return snap;
            }
            assert!(Instant::now() < deadline, "timed out: {snap:#?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn command(&self, command: Command) -> Snapshot {
        self.core.send(Input::Command { command });
        assert!(self.rt.settle(WAIT));
        self.core.snapshot()
    }
}

#[test]
fn rows_are_formatted_and_versions_move_only_with_the_rows() {
    let f = Fixture::start();
    let first = f.core.snapshot();
    let left = &first.left;
    // "..", the folder, then the files by name; the dotfile is hidden.
    assert_eq!((left.row_count, left.shown, left.cursor), (4, 3, 0));
    let rows = f.core.rows(Side::Left, 0, 100);
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["..", "dir", "a.txt", "b.rs"]);
    assert_eq!((rows[0].size.as_str(), rows[0].icon, rows[0].is_parent), ("UP", IconKind::Parent, true));
    assert_eq!((rows[1].size.as_str(), rows[1].icon, rows[1].ext.as_str()), ("DIR", IconKind::Folder, ""));
    assert_eq!((rows[2].size.as_str(), rows[2].icon, rows[2].ext.as_str()), ("3 B", IconKind::Text, "txt"));
    assert_eq!(rows[3].icon, IconKind::Code);
    assert_eq!(rows[2].date.len(), 16, "ISO date: {}", rows[2].date);
    assert!(rows[2].mode.starts_with("-rw"), "{}", rows[2].mode);
    assert!(rows[0].cursor && !rows[1].cursor);
    // A range past the end is cut short.
    assert_eq!(f.core.rows(Side::Left, 3, 10).len(), 1);
    assert!(f.core.rows(Side::Left, 9, 10).is_empty());

    // Nothing changed: same version.
    assert_eq!(f.core.snapshot().left.listing_version, left.listing_version);

    // The cursor moves without the rows changing.
    let moved = f.command(Command::CursorDown);
    assert_eq!(moved.left.cursor, 1);
    assert_eq!(moved.left.listing_version, left.listing_version);
    assert_eq!(moved.right.listing_version, first.right.listing_version);

    // A mark changes the rows.
    let marked = f.command(Command::ToggleMark);
    assert_ne!(marked.left.listing_version, left.listing_version);
    assert_eq!(marked.right.listing_version, first.right.listing_version);
    assert_eq!(marked.left.selection_count, 1);
    let rows = f.core.rows(Side::Left, 0, 100);
    assert!(rows[1].marked && !rows[2].marked);

    // Dragging a marked row takes the marks; an unmarked one only itself.
    let drag = f.core.drag(Side::Left, 1).unwrap();
    assert_eq!(drag.dragged.names, vec![b"dir".to_vec()]);
    assert_eq!(drag.label, "dir");
    assert!(drag.local_paths[0].ends_with("/left/dir") && drag.local_dirs == [true]);
    let drag = f.core.drag(Side::Left, 3).unwrap();
    assert_eq!(drag.dragged.names, vec![b"b.rs".to_vec()]);
    assert!(f.core.drag(Side::Left, 0).is_none(), "the parent row is not dragged");

    // A setting the rows show (the date format) moves the version too.
    let before = f.core.snapshot().left.listing_version;
    f.core.send(Input::Settings {
        edit: crate::settings::SettingsEdit::DateFormat {
            format: crate::types::DateFormat::DayFirst,
        },
    });
    let after = f.wait(|s| s.settings.general.date_format == crate::types::DateFormat::DayFirst);
    assert_ne!(after.left.listing_version, before);
    assert_eq!(&f.core.rows(Side::Left, 2, 1)[0].date[2..3], ".");
    f.rt.shutdown();
}

#[test]
fn dialogs_and_keys_reach_the_snapshot() {
    let f = Fixture::start();
    let snap = f.command(Command::NewFolder);
    let Some(dialog) = snap.dialogs.last() else { panic!("no dialog") };
    let crate::snapshot::DialogKindSnapshot::Input { dialog: input } = &dialog.kind else {
        panic!("not an input dialog: {dialog:?}")
    };
    assert_eq!((input.title.as_str(), input.ok.as_str()), ("New Folder", "Create"));
    f.core.send(Input::Dialog {
        id: dialog.id,
        input: crate::input::DialogInput::Cancel,
    });
    let snap = f.wait(|s| s.dialogs.is_empty());
    assert!(snap.dialogs.is_empty());

    let table = f.core.key_table();
    let copy = table.iter().find(|c| c.id == "copy-files").unwrap();
    assert_eq!(copy.bindings[0].pretty, "F5");
    let keys_version = snap.settings.keys_version;
    f.core.send(Input::Settings {
        edit: crate::settings::SettingsEdit::Keys {
            command: "copy-files".into(),
            keys: Some(vec!["f9".into()]),
        },
    });
    let snap = f.wait(|s| s.settings.keys_version != keys_version);
    assert!(snap.settings.keys_version > keys_version);
    let copy = f.core.key_table().into_iter().find(|c| c.id == "copy-files").unwrap();
    assert_eq!(copy.bindings[0].keys, "f9");
    f.rt.shutdown();
}

#[test]
fn the_bookmark_editor_and_manager_reach_the_snapshot() {
    use crate::bookmarks::{BookmarkFieldKind, BookmarkIcon, BookmarksInput};
    use crate::snapshot::DialogKindSnapshot;

    let f = Fixture::start();
    let editor = |s: &Snapshot| {
        s.dialogs.iter().rev().find_map(|d| match &d.kind {
            DialogKindSnapshot::BookmarkEdit { dialog } => Some((d.id, dialog.clone())),
            _ => None,
        })
    };
    let snap = f.command(Command::NewBookmark);
    let (id, edit) = editor(&snap).expect("the editor opens");
    assert_eq!((edit.title.as_str(), edit.is_new, edit.plugin.as_str()), ("New Bookmark", true, "ssh"));
    assert!(edit.plugins.iter().any(|p| p.id == "local"), "{:?}", edit.plugins);
    assert_eq!(edit.autofocus.as_deref(), Some("host"));
    assert!(!edit.description.is_empty());
    assert!(edit.has_advanced && !edit.advanced);
    assert_eq!(edit.advanced_label, "Show advanced options");
    assert!(edit.fields.iter().all(|f| !f.advanced));
    let basic = edit.fields.len();

    f.core.send(Input::Bookmarks {
        input: BookmarksInput::ToggleAdvanced { dialog: id },
    });
    let (_, edit) = editor(&f.wait(|s| editor(s).is_some_and(|(_, e)| e.advanced))).unwrap();
    assert!(edit.fields.len() > basic);
    assert_eq!(edit.advanced_label, "Hide advanced options");

    // A local folder: one path field, which "Choose…" fills in.
    f.core.send(Input::Bookmarks {
        input: BookmarksInput::Plugin {
            dialog: id,
            plugin: "local".into(),
        },
    });
    let (_, edit) = editor(&f.wait(|s| editor(s).is_some_and(|(_, e)| e.plugin == "local"))).unwrap();
    assert!(edit.picks_directories);
    assert_eq!(edit.fields.len(), 1);
    assert_eq!((edit.fields[0].key.as_str(), edit.fields[0].kind), ("path", BookmarkFieldKind::LocalPath));
    f.core.send(Input::Bookmarks {
        input: BookmarksInput::PathChosen {
            dialog: id,
            key: "path".into(),
            path: b"/tmp".to_vec(),
        },
    });
    let (_, edit) = editor(&f.wait(|s| editor(s).is_some_and(|(_, e)| !e.fields[0].value.is_empty()))).unwrap();
    assert_eq!(edit.fields[0].value, "/tmp");
    f.core.send(Input::Bookmarks {
        input: BookmarksInput::Save { dialog: id, open: false },
    });
    let snap = f.wait(|s| editor(s).is_none() && s.bookmarks.len() == 1);
    assert!(snap.dialogs.is_empty());

    // The manager lists it.
    let snap = f.command(Command::ManageBookmarks);
    let manager = snap
        .dialogs
        .iter()
        .find_map(|d| match &d.kind {
            DialogKindSnapshot::Bookmarks { dialog } => Some(dialog.clone()),
            _ => None,
        })
        .expect("the manager opens");
    assert_eq!(manager.title, "Bookmarks");
    assert!(!manager.empty_text.is_empty());
    let row = &manager.rows[0];
    assert_eq!(row.icon, BookmarkIcon::Local);
    assert!(!row.can_move_up && !row.can_move_down && !row.connected);
    assert_eq!(row.open_label, "Connect");
    f.rt.shutdown();
}
