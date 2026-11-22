//! A real runtime driving the real core against temporary folders on the
//! local disk: what the user does goes in as events, and the checks are on
//! the disk and on the view.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tl_app::dialog::{DialogInput, DialogKind};
use tl_app::model::{Command, SettingsEdit};
use tl_app::pane::PaneInput;
use tl_app::view::LocationKind;
use tl_app::{Event, Side, ToastKind, ViewModel};
use tl_core::transfer::ConflictChoice;
use tl_core::{
    Bookmark, CancelToken, FieldSpec, PluginEnv, PluginRegistry, ProviderPlugin, VPath, Vfs, VfsError, VfsResult,
};
use tl_runtime::{Config, Runtime, Ui};

// ----- a window that only takes notes -----------------------------------------------

#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
enum Call {
    Toast(ToastKind, String),
    Clipboard(String),
    Open(PathBuf),
    Reveal(PathBuf),
    Url(String),
    Quit,
}

#[derive(Default)]
struct Recorder {
    renders: AtomicUsize,
    calls: Mutex<Vec<Call>>,
}

impl Ui for Recorder {
    fn render(&self) {
        self.renders.fetch_add(1, Ordering::SeqCst);
    }
    fn toast(&self, kind: ToastKind, message: String) {
        self.calls.lock().push(Call::Toast(kind, message));
    }
    fn write_clipboard(&self, text: String) {
        self.calls.lock().push(Call::Clipboard(text));
    }
    fn open(&self, path: &Path) {
        self.calls.lock().push(Call::Open(path.to_path_buf()));
    }
    fn reveal(&self, path: &Path) {
        self.calls.lock().push(Call::Reveal(path.to_path_buf()));
    }
    fn open_url(&self, url: &str) {
        self.calls.lock().push(Call::Url(url.into()));
    }
    fn quit(&self) {
        self.calls.lock().push(Call::Quit);
    }
}

// ----- the fixture -------------------------------------------------------------------------

struct App {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
    left: PathBuf,
    right: PathBuf,
    ui: Arc<Recorder>,
    rt: Runtime,
}

const WAIT: Duration = Duration::from_secs(15);

impl App {
    /// Panes in `left` and `right` (the settings say so); `setup` fills
    /// the folders first.
    fn start(setup: impl FnOnce(&App0), plugins: Option<PluginRegistry>) -> App {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (data, left, right) = (root.join("data"), root.join("left"), root.join("right"));
        for dir in [&data, &left, &right] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let settings = serde_json::json!({
            "session": { "left_path": left.to_str().unwrap(), "right_path": right.to_str().unwrap() }
        });
        std::fs::write(data.join("settings.json"), settings.to_string()).unwrap();
        setup(&App0 {
            root: root.clone(),
            data: data.clone(),
            left: left.clone(),
            right: right.clone(),
        });
        let ui = Arc::new(Recorder::default());
        let config = Config {
            data_dir: data.clone(),
            cache_dir: root.join("cache"),
            runtime_dir: Some(root.join("run")),
            askpass_exe: None,
            plugins,
        };
        let rt = Runtime::start(config, ui.clone());
        let app = App {
            _tmp: tmp,
            root,
            data,
            left,
            right,
            ui,
            rt,
        };
        app.wait("both panes listed", |v| {
            v.ready && v.panes.iter().all(|p| !p.loading)
        });
        app
    }

    fn send(&self, event: Event) {
        self.rt.dispatch(event);
    }

    fn command(&self, command: Command) {
        self.send(Event::Command(command));
    }

    /// Lets everything finish, then waits (for watchers, which the
    /// runtime cannot see coming) until `done` holds.
    fn wait(&self, what: &str, done: impl Fn(&ViewModel) -> bool) -> ViewModel {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(self.rt.settle(WAIT), "the runtime did not settle while waiting for {what}");
            let view = self.rt.view();
            if done(&view) {
                return view;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}: {:#?}", view.dialogs);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn settled(&self) -> ViewModel {
        assert!(self.rt.settle(WAIT), "the runtime did not settle");
        self.rt.view()
    }

    /// Puts the cursor of `side` on `name`.
    fn click(&self, side: Side, name: &str) {
        let view = self.rt.view();
        let row = row_of(&view, side, name).unwrap_or_else(|| panic!("{name} is not listed: {:?}", names(&view, side)));
        self.send(Event::Pane(
            side,
            PaneInput::Click {
                row,
                toggle: false,
                range: false,
            },
        ));
    }

    /// The dialog on top, once there is one of the expected kind.
    fn dialog(&self, what: &str, kind: impl Fn(&DialogKind) -> bool) -> tl_app::dialog::Dialog {
        let view = self.wait(what, |v| v.dialogs.last().is_some_and(|d| kind(&d.kind)));
        view.dialogs.last().unwrap().clone()
    }

    fn submit(&self, what: &str, kind: impl Fn(&DialogKind) -> bool) {
        let dialog = self.dialog(what, kind);
        self.send(Event::Dialog(dialog.id, DialogInput::Submit));
    }

    fn errors(&self) -> Vec<String> {
        self.ui
            .calls
            .lock()
            .iter()
            .filter_map(|c| match c {
                Call::Toast(ToastKind::Error, m) => Some(m.clone()),
                _ => None,
            })
            .collect()
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.rt.shutdown();
    }
}

/// The folders, before the runtime starts.
struct App0 {
    #[allow(dead_code)]
    root: PathBuf,
    data: PathBuf,
    left: PathBuf,
    right: PathBuf,
}

fn names(view: &ViewModel, side: Side) -> Vec<String> {
    let pane = &view.panes[side as usize];
    pane.order
        .iter()
        .filter_map(|ix| pane.listing.names.get(*ix as usize).cloned())
        .collect()
}

fn row_of(view: &ViewModel, side: Side, name: &str) -> Option<usize> {
    let pane = &view.panes[side as usize];
    pane.order
        .iter()
        .position(|ix| pane.listing.names.get(*ix as usize).is_some_and(|n| n == name))
}

fn lists(side: Side, name: &'static str) -> impl Fn(&ViewModel) -> bool {
    move |v| row_of(v, side, name).is_some()
}

fn read(path: impl AsRef<Path>) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn is_input(kind: &DialogKind) -> bool {
    matches!(kind, DialogKind::Input(_))
}

fn is_confirm(kind: &DialogKind) -> bool {
    matches!(kind, DialogKind::Confirm(_))
}

// ----- startup --------------------------------------------------------------------------------

#[test]
fn startup_loads_the_settings_and_lists_both_panes() {
    let app = App::start(
        |d| {
            std::fs::write(d.left.join("a.txt"), "alpha").unwrap();
            std::fs::create_dir(d.left.join("docs")).unwrap();
            std::fs::write(d.right.join("b.txt"), "beta").unwrap();
        },
        None,
    );
    let view = app.wait("listings", |v| lists(Side::Left, "a.txt")(v) && lists(Side::Right, "b.txt")(v));
    assert!(view.ready);
    assert_eq!(view.panes[0].path, VPath::from_local(&app.left));
    assert_eq!(view.panes[1].path, VPath::from_local(&app.right));
    assert_eq!(names(&view, Side::Left), ["docs", "a.txt"]);
    assert!(view.panes[0].error.is_none(), "{:?}", view.panes[0].error);
    assert!(view.panes[0].space.is_some(), "free space comes with the listing");
    let plugins: Vec<&str> = view.plugins.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(plugins, ["ssh", "s3", "webdav", "local"]);
    assert!(view.local_folders.iter().any(|f| f.label == "Root (/)" && f.volume));
    assert!(app.ui.renders.load(Ordering::SeqCst) > 0, "the view was rendered");
    assert!(app.errors().is_empty(), "{:?}", app.errors());
}

#[test]
fn a_broken_settings_file_is_kept_aside_and_the_defaults_used() {
    let app = App::start(|d| std::fs::write(d.data.join("settings.json"), "{ not json").unwrap(), None);
    let view = app.settled();
    assert!(view.ready);
    assert_eq!(read(app.data.join("settings.json.broken")), "{ not json");
    // The panes open at home, since the file named no folders.
    let home = VPath::from_local(Path::new(&std::env::var("HOME").unwrap()));
    assert_eq!(view.panes[0].path, home);
}

// ----- files ----------------------------------------------------------------------------------

#[test]
fn copying_marked_files_to_the_other_pane() {
    let app = App::start(
        |d| {
            std::fs::write(d.left.join("a.txt"), "alpha").unwrap();
            std::fs::create_dir(d.left.join("sub")).unwrap();
            std::fs::write(d.left.join("sub/x.txt"), "x").unwrap();
        },
        None,
    );
    app.wait("left listing", lists(Side::Left, "sub"));
    app.command(Command::MarkAll);
    app.command(Command::CopyFiles);
    app.submit("the copy dialog", is_input);
    let view = app.wait("the copies listed", |v| {
        v.running == 0 && lists(Side::Right, "a.txt")(v) && lists(Side::Right, "sub")(v)
    });
    assert_eq!(read(app.right.join("a.txt")), "alpha");
    assert_eq!(read(app.right.join("sub/x.txt")), "x");
    assert!(app.left.join("a.txt").exists(), "a copy leaves the source");
    assert!(view.jobs.iter().all(|j| !j.is_running()));
    assert!(app.errors().is_empty(), "{:?}", app.errors());
}

#[test]
fn a_conflict_is_asked_and_answered() {
    let app = App::start(
        |d| {
            std::fs::write(d.left.join("a.txt"), "new").unwrap();
            std::fs::write(d.left.join("b.txt"), "b").unwrap();
            std::fs::write(d.right.join("a.txt"), "old").unwrap();
        },
        None,
    );
    app.wait("left listing", lists(Side::Left, "a.txt"));
    app.command(Command::MarkAll);
    app.command(Command::CopyFiles);
    app.submit("the copy dialog", is_input);

    // The job waits for the answer, and the runtime counts that as settled.
    let conflict = app.dialog("the conflict", |k| matches!(k, DialogKind::Conflict { .. }));
    let DialogKind::Conflict { conflict: clash, .. } = &conflict.kind else { unreachable!() };
    assert_eq!(clash.source.name, b"a.txt");
    assert_eq!(read(app.right.join("a.txt")), "old", "nothing is copied before the answer");
    assert_eq!(app.rt.view().running, 1);

    app.send(Event::Dialog(conflict.id, DialogInput::Choose(ConflictChoice::Overwrite)));
    app.wait("the copy", |v| v.running == 0 && lists(Side::Right, "b.txt")(v));
    assert_eq!(read(app.right.join("a.txt")), "new");
    assert_eq!(read(app.right.join("b.txt")), "b");
}

#[test]
fn cancelling_a_conflict_stops_the_copy() {
    let app = App::start(
        |d| {
            std::fs::write(d.left.join("a.txt"), "new").unwrap();
            std::fs::write(d.right.join("a.txt"), "old").unwrap();
        },
        None,
    );
    app.wait("left listing", lists(Side::Left, "a.txt"));
    app.click(Side::Left, "a.txt");
    app.command(Command::CopyFiles);
    app.submit("the copy dialog", is_input);
    let conflict = app.dialog("the conflict", |k| matches!(k, DialogKind::Conflict { .. }));
    app.send(Event::Dialog(conflict.id, DialogInput::Cancel));
    let view = app.wait("the job to end", |v| v.running == 0);
    assert!(view.dialogs.is_empty(), "{:?}", view.dialogs);
    assert_eq!(read(app.right.join("a.txt")), "old");
}

#[test]
fn deleting_permanently() {
    let app = App::start(
        |d| {
            std::fs::write(d.left.join("doomed.txt"), "x").unwrap();
            std::fs::write(d.left.join("kept.txt"), "y").unwrap();
        },
        None,
    );
    app.wait("left listing", lists(Side::Left, "doomed.txt"));
    app.click(Side::Left, "doomed.txt");
    app.command(Command::DeleteFiles);
    app.submit("the delete confirmation", is_confirm);
    let view = app.wait("the listing without it", |v| v.running == 0 && !lists(Side::Left, "doomed.txt")(v));
    assert!(!app.left.join("doomed.txt").exists());
    assert!(app.left.join("kept.txt").exists());
    assert_eq!(names(&view, Side::Left), ["kept.txt"]);
}

#[test]
fn making_nested_folders() {
    let app = App::start(|_| {}, None);
    app.command(Command::NewFolder);
    let dialog = app.dialog("the new folder dialog", is_input);
    app.send(Event::Dialog(dialog.id, DialogInput::Text("one/two/three".into())));
    app.send(Event::Dialog(dialog.id, DialogInput::Submit));
    app.wait("the folder listed", |v| v.running == 0 && lists(Side::Left, "one")(v));
    assert!(app.left.join("one/two/three").is_dir());
}

// ----- settings --------------------------------------------------------------------------------

#[test]
fn a_settings_toggle_reaches_the_file() {
    let app = App::start(|_| {}, None);
    let path = app.data.join("settings.json");
    app.send(Event::Settings(SettingsEdit::ShowHidden(true)));
    app.settled();
    let saved: serde_json::Value = serde_json::from_str(&read(&path)).unwrap();
    assert_eq!(saved["general"]["show_hidden"], true);
    assert_eq!(saved["version"], 1);
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    // Setting it to what it already is writes nothing.
    std::fs::write(&path, "sentinel").unwrap();
    app.send(Event::Settings(SettingsEdit::ShowHidden(true)));
    app.settled();
    assert_eq!(read(&path), "sentinel");

    app.send(Event::Settings(SettingsEdit::ShowHidden(false)));
    app.settled();
    let saved: serde_json::Value = serde_json::from_str(&read(&path)).unwrap();
    assert_eq!(saved["general"]["show_hidden"], false);
}

// ----- watching ----------------------------------------------------------------------------------

#[test]
fn a_watched_folder_follows_outside_changes() {
    let app = App::start(|d| std::fs::write(d.right.join("old.txt"), "o").unwrap(), None);
    app.wait("right listing", lists(Side::Right, "old.txt"));
    // FSEvents takes a moment to start reporting.
    std::thread::sleep(Duration::from_millis(300));
    std::fs::write(app.right.join("new.txt"), "n").unwrap();
    app.wait("the new file listed", lists(Side::Right, "new.txt"));
    std::fs::remove_file(app.right.join("old.txt")).unwrap();
    app.wait("the old file gone", |v| !lists(Side::Right, "old.txt")(v));
}

// ----- archives ----------------------------------------------------------------------------------

fn make_tar(path: &Path) {
    let file = std::fs::File::create(path).unwrap();
    let mut builder = tar::Builder::new(file);
    for (name, body) in [("./top.txt", "top"), ("./docs/readme.txt", "read me")] {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(1_700_000_000);
        header.set_cksum();
        builder.append_data(&mut header, name, body.as_bytes()).unwrap();
    }
    builder.finish().unwrap();
}

#[test]
fn an_archive_opens_as_a_folder_and_extracts() {
    let app = App::start(|d| make_tar(&d.left.join("pack.tar")), None);
    app.wait("left listing", lists(Side::Left, "pack.tar"));
    app.click(Side::Left, "pack.tar");
    app.command(Command::OpenArchive);
    let view = app.wait("the archive's members", |v| {
        v.panes[0].kind == LocationKind::Archive && lists(Side::Left, "top.txt")(v)
    });
    assert_eq!(names(&view, Side::Left), ["docs", "top.txt"]);

    app.command(Command::GoParent);
    app.wait("back in the folder", |v| v.panes[0].kind == LocationKind::Local && lists(Side::Left, "pack.tar")(v));
    app.click(Side::Left, "pack.tar");
    app.command(Command::ExtractArchive);
    app.submit("the extract dialog", is_input);
    app.wait("the extracted files", |v| v.running == 0 && lists(Side::Right, "top.txt")(v));
    assert_eq!(read(app.right.join("top.txt")), "top");
    assert_eq!(read(app.right.join("docs/readme.txt")), "read me");
    assert!(app.errors().is_empty(), "{:?}", app.errors());
}

// ----- prompts from a provider -----------------------------------------------------------------

/// Connects to the local disk once given the password "sesame".
struct Guarded;

impl ProviderPlugin for Guarded {
    fn id(&self) -> &'static str {
        "guarded"
    }
    fn name(&self) -> &'static str {
        "Guarded folder"
    }
    fn description(&self) -> &'static str {
        "A local folder behind a password."
    }
    fn fields(&self) -> Vec<FieldSpec> {
        vec![FieldSpec::text("path", "Folder").required()]
    }
    fn summary(&self, bookmark: &Bookmark) -> String {
        format!("guarded:{}", bookmark.get("path"))
    }
    fn connect(&self, _bookmark: &Bookmark, env: &PluginEnv, _cancel: &CancelToken) -> VfsResult<Arc<dyn Vfs>> {
        match env.prompter.secret("Password", "The guarded folder's password:") {
            Some(answer) if answer == "sesame" => Ok(tl_local::local_vfs()),
            Some(_) => Err(VfsError::Auth("wrong password".into())),
            None => Err(VfsError::Cancelled),
        }
    }
    fn initial_path(&self, bookmark: &Bookmark, _vfs: &dyn Vfs) -> VPath {
        VPath::parse(bookmark.get("path"))
    }
}

#[test]
fn a_provider_asks_for_a_password_and_gets_it() {
    let mut plugins = PluginRegistry::new();
    plugins.register(Arc::new(Guarded));
    plugins.register(Arc::new(tl_local::LocalPlugin));
    let app = App::start(
        |d| {
            let vault = d.root.join("vault");
            std::fs::create_dir(&vault).unwrap();
            std::fs::write(vault.join("secret.txt"), "s").unwrap();
            let mut bookmark = Bookmark::with_id("b1", "Vault", "guarded");
            bookmark.set("path", vault.to_str().unwrap());
            let mut data = tl_core::bookmark::Bookmarks::default();
            data.upsert(bookmark);
            tl_core::bookmark::write(&d.data.join("bookmarks.json"), &data).unwrap();
        },
        Some(plugins),
    );
    let view = app.settled();
    assert_eq!(view.summaries.get("b1").map(String::as_str), Some(format!("guarded:{}", app.root.join("vault").display()).as_str()));

    app.send(Event::Pane(Side::Left, PaneInput::OpenBookmark("b1".into())));
    let secret = app.dialog("the password prompt", |k| matches!(k, DialogKind::Secret { .. }));
    app.send(Event::Dialog(secret.id, DialogInput::Text("sesame".into())));
    app.send(Event::Dialog(secret.id, DialogInput::Submit));
    let view = app.wait("the vault listed", |v| v.running == 0 && lists(Side::Left, "secret.txt")(v));
    assert_eq!(view.panes[0].path, VPath::from_local(&app.root.join("vault")));
}

#[test]
fn shutdown_unblocks_a_waiting_prompt() {
    let mut plugins = PluginRegistry::new();
    plugins.register(Arc::new(Guarded));
    plugins.register(Arc::new(tl_local::LocalPlugin));
    let app = App::start(
        |d| {
            let mut bookmark = Bookmark::with_id("b1", "Vault", "guarded");
            bookmark.set("path", d.root.to_str().unwrap());
            let mut data = tl_core::bookmark::Bookmarks::default();
            data.upsert(bookmark);
            tl_core::bookmark::write(&d.data.join("bookmarks.json"), &data).unwrap();
        },
        Some(plugins),
    );
    app.send(Event::Pane(Side::Left, PaneInput::OpenBookmark("b1".into())));
    app.dialog("the password prompt", |k| matches!(k, DialogKind::Secret { .. }));
    let started = Instant::now();
    app.rt.shutdown();
    assert!(started.elapsed() < Duration::from_secs(5), "shutdown waited on the prompt");
    // Once down, events go nowhere and the view still answers.
    app.send(Event::Command(Command::Refresh));
    let _ = app.rt.view();
}
