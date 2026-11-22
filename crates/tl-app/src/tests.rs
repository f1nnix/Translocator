//! Journeys through the core, with a small in-memory shell answering the
//! effects: no files, no threads, no window.

use std::collections::{HashMap, VecDeque};

use crux_core::Core;
use tl_model::{Bookmarks, Capabilities, Entry, EntryKind, VPath};

use crate::conn::ConnInfo;
use crate::model::Command as C;
use crate::*;

struct Shell {
    core: Core<Translocator>,
    files: HashMap<String, Vec<Entry>>,
    /// Listings held back, to answer out of order.
    held: Vec<crux_core::Request<FsOp>>,
    hold: bool,
    toasts: Vec<String>,
}

fn file(name: &str, size: u64) -> Entry {
    let mut e = Entry::new(name.as_bytes().to_vec(), EntryKind::File);
    e.size = size;
    e.mtime = Some(1_700_000_000);
    e
}

fn dir(name: &str) -> Entry {
    Entry::new(name.as_bytes().to_vec(), EntryKind::Dir)
}

impl Shell {
    fn new(files: &[(&str, Vec<Entry>)]) -> Shell {
        let mut shell = Shell {
            core: Core::new(),
            files: files.iter().map(|(p, e)| (p.to_string(), e.clone())).collect(),
            held: Vec::new(),
            hold: false,
            toasts: Vec::new(),
        };
        shell.send(Event::Start { seed: 7 });
        shell
    }

    fn send(&mut self, event: Event) {
        let effects = self.core.process_event(event);
        self.run(effects);
    }

    fn cmd(&mut self, cmd: C) {
        self.send(Event::Command(cmd));
    }

    fn run(&mut self, effects: Vec<Effect>) {
        let mut queue: VecDeque<Effect> = effects.into();
        while let Some(effect) = queue.pop_front() {
            let more = match effect {
                Effect::Render(_) => vec![],
                Effect::Store(mut req) => match req.operation {
                    StoreOp::Load => {
                        let mut settings = settings::Settings::default();
                        settings.general.restore_folders = true;
                        settings.session.left_path = Some("/left".into());
                        settings.session.right_path = Some("/right".into());
                        let out = StoreOutput::Loaded {
                            settings: Box::new(settings),
                            bookmarks: Bookmarks::default(),
                        };
                        self.core.resolve(&mut req, out).unwrap()
                    }
                    StoreOp::SaveBookmarks(_) => self.core.resolve(&mut req, StoreOutput::Saved(Ok(()))).unwrap(),
                    _ => vec![],
                },
                Effect::Plugin(mut req) => match &req.operation {
                    PluginOp::List => {
                        let local = ConnInfo {
                            id: LOCAL,
                            plugin: "local".into(),
                            label: "Local".into(),
                            location_key: "local".into(),
                            caps: Capabilities {
                                trash: true,
                                exec: true,
                                ..Default::default()
                            },
                            home: VPath::parse("/home"),
                            bookmark: None,
                            host: None,
                            server_info: None,
                            local_home: Some("/home".into()),
                        };
                        let out = PluginOutput::Plugins { plugins: vec![], local };
                        self.core.resolve(&mut req, out).unwrap()
                    }
                    PluginOp::Summaries(b) => {
                        let n = b.len();
                        self.core.resolve(&mut req, PluginOutput::Summaries(vec![String::new(); n])).unwrap()
                    }
                    _ => vec![],
                },
                Effect::Fs(mut req) => match req.operation.clone() {
                    FsOp::List { path, .. } if self.hold => {
                        let _ = path;
                        self.held.push(req);
                        vec![]
                    }
                    FsOp::List { path, .. } => {
                        let entries = self
                            .files
                            .get(&path.display())
                            .cloned()
                            .ok_or_else(|| tl_model::VfsError::NotFound(path.display()));
                        self.core.resolve(&mut req, FsOutput::Listing { entries, space: None }).unwrap()
                    }
                    FsOp::Stat { path, .. } => {
                        let found = self.files.contains_key(&path.display()).then(|| dir(&path.file_name_lossy()));
                        self.core.resolve(&mut req, FsOutput::Stat(Ok(found))).unwrap()
                    }
                    _ => vec![],
                },
                Effect::Job(mut req) => match &req.operation {
                    JobOp::Start { .. } => self
                        .core
                        .resolve(&mut req, JobUpdate::Finished(Ok(JobOutput::Done)))
                        .unwrap(),
                    _ => vec![],
                },
                Effect::Platform(mut req) => match req.operation.clone() {
                    PlatformOp::Toast { message, .. } => {
                        self.toasts.push(message);
                        vec![]
                    }
                    PlatformOp::LocalFolders => self.core.resolve(&mut req, PlatformOutput::Folders(vec![])).unwrap(),
                    _ => vec![],
                },
                _ => vec![],
            };
            queue.extend(more);
        }
    }

    fn release_held(&mut self) {
        self.hold = false;
        for mut req in std::mem::take(&mut self.held) {
            let FsOp::List { path, .. } = &req.operation else { continue };
            let entries = Ok(self.files.get(&path.display()).cloned().unwrap_or_default());
            let effects = self.core.resolve(&mut req, FsOutput::Listing { entries, space: None }).unwrap();
            self.run(effects);
        }
    }

    fn view(&self) -> ViewModel {
        self.core.view()
    }

    fn names(&self, side: Side) -> Vec<String> {
        let pane = &self.view().panes[side.index()];
        pane.order
            .iter()
            .map(|ix| match *ix {
                pane::PARENT => "..".to_string(),
                ix => pane.listing.names[ix as usize].clone(),
            })
            .collect()
    }

    fn cursor_name(&self, side: Side) -> String {
        let pane = &self.view().panes[side.index()];
        self.names(side)[pane.cursor].clone()
    }

    fn marked(&self, side: Side) -> Vec<String> {
        let pane = &self.view().panes[side.index()];
        let mut m: Vec<String> = pane.marked.iter().map(|n| String::from_utf8_lossy(n).into_owned()).collect();
        m.sort();
        m
    }

    fn typ(&mut self, text: &str) {
        for c in text.chars() {
            self.cmd(C::Type(c.to_string()));
        }
    }
}

fn logs() -> Vec<(&'static str, Vec<Entry>)> {
    vec![
        (
            "/left",
            vec![file("app.log", 10), file("db.log", 20), file("logrotate.conf", 5), dir("logs"), file("notes.txt", 1), dir("src")],
        ),
        ("/left/logs", vec![file("old.log", 3)]),
        ("/right", vec![file("r.txt", 1)]),
    ]
}

#[test]
fn startup_restores_both_folders() {
    let shell = Shell::new(&logs());
    let view = shell.view();
    assert!(view.ready);
    assert_eq!(view.panes[0].display, "/left");
    assert_eq!(view.panes[1].display, "/right");
    assert_eq!(shell.names(Side::Left)[0], "..", "not at the root, so there is a way up");
    assert_eq!(view.title, "/left — Translocator");
}

#[test]
fn typing_filters_the_list() {
    let mut shell = Shell::new(&logs());
    shell.typ("log");
    assert_eq!(shell.names(Side::Left), ["..", "logs", "app.log", "db.log", "logrotate.conf"]);
    shell.typ("s");
    assert_eq!(shell.names(Side::Left), ["..", "logs"]);
    shell.cmd(C::FilterBackspace);
    shell.cmd(C::FilterBackspace);
    shell.cmd(C::FilterBackspace);
    shell.cmd(C::FilterBackspace);
    assert_eq!(shell.view().panes[0].filter, "");
    shell.typ("*.log");
    assert_eq!(shell.names(Side::Left), ["..", "app.log", "db.log"]);
    shell.cmd(C::ClearFilter);
    assert_eq!(shell.names(Side::Left).len(), 7);
    // Backspace with no filter goes up.
    shell.cmd(C::FilterBackspace);
    assert_eq!(shell.view().panes[0].display, "/");
}

#[test]
fn plus_and_minus_ask_for_a_mask_only_before_typing() {
    let mut shell = Shell::new(&logs());
    shell.cmd(C::Type("+".into()));
    let view = shell.view();
    assert!(matches!(&view.dialogs[0].kind, dialog::DialogKind::Input(d) if d.title == "Mark Files"));
    let id = view.dialogs[0].id;
    shell.send(Event::Dialog(id, dialog::DialogInput::Text("*.log;notes*".into())));
    shell.send(Event::Dialog(id, dialog::DialogInput::Submit));
    assert_eq!(shell.marked(Side::Left), ["app.log", "db.log", "notes.txt"]);
    shell.typ("x+");
    assert_eq!(shell.view().panes[0].filter, "x+");
}

#[test]
fn marks_follow_the_filter_and_survive_it() {
    let mut shell = Shell::new(&logs());
    shell.typ("*.log");
    shell.cmd(C::MarkAll);
    assert_eq!(shell.marked(Side::Left), ["app.log", "db.log"]);
    shell.typ("zzz");
    assert_eq!(shell.names(Side::Left), [".."]);
    for _ in 0..3 {
        shell.cmd(C::FilterBackspace);
    }
    assert_eq!(shell.marked(Side::Left), ["app.log", "db.log"]);
    let view = shell.view();
    assert_eq!(view.panes[0].status, "2 of 2 marked · 30 B");
}

#[test]
fn invert_marks_flips_files_only() {
    let mut shell = Shell::new(&logs());
    shell.send(Event::Pane(Side::Left, pane::PaneInput::Click { row: 1, toggle: true, range: false }));
    assert_eq!(shell.marked(Side::Left), ["logs"]);
    shell.cmd(C::InvertMarks);
    assert_eq!(shell.marked(Side::Left), ["app.log", "db.log", "logrotate.conf", "logs", "notes.txt"]);
}

#[test]
fn going_up_lands_on_the_folder_we_came_from() {
    let mut shell = Shell::new(&logs());
    shell.send(Event::Pane(Side::Left, pane::PaneInput::Click { row: 1, toggle: false, range: false }));
    assert_eq!(shell.cursor_name(Side::Left), "logs");
    shell.cmd(C::Open);
    assert_eq!(shell.view().panes[0].display, "/left/logs");
    shell.cmd(C::GoParent);
    assert_eq!(shell.cursor_name(Side::Left), "logs");
    shell.cmd(C::GoBack);
    assert_eq!(shell.view().panes[0].display, "/left/logs");
    shell.cmd(C::GoBack);
    assert_eq!(shell.cursor_name(Side::Left), "logs", "back lands on the folder it came out of");
}

#[test]
fn a_folder_that_changes_mid_listing_still_lands_the_cursor() {
    let mut shell = Shell::new(&logs());
    shell.send(Event::Pane(Side::Left, pane::PaneInput::Click { row: 3, toggle: false, range: false }));
    shell.cmd(C::RenameFile);
    let view = shell.view();
    let dialog::DialogKind::Input(d) = &view.dialogs[0].kind else { panic!("rename asks") };
    assert_eq!((d.text.as_str(), d.select), ("app.log", Some((0, 3))), "the stem is preselected");
    let id = view.dialogs[0].id;
    // The rename asks for the cursor on its new name, and a refresh
    // supersedes that listing before it is answered.
    shell.hold = true;
    shell.send(Event::Dialog(id, dialog::DialogInput::Text("zz.log".into())));
    shell.send(Event::Dialog(id, dialog::DialogInput::Submit));
    shell.cmd(C::Refresh);
    shell.files.get_mut("/left").unwrap()[0] = file("zz.log", 10);
    shell.release_held();
    assert_eq!(shell.cursor_name(Side::Left), "zz.log");
}

#[test]
fn folders_can_be_sorted_in_with_the_files() {
    let mut shell = Shell::new(&logs());
    assert_eq!(shell.names(Side::Left)[1], "logs");
    shell.send(Event::Settings(model::SettingsEdit::FoldersFirst(false)));
    assert_eq!(
        shell.names(Side::Left),
        ["..", "app.log", "db.log", "logrotate.conf", "logs", "notes.txt", "src"]
    );
}

#[test]
fn deleting_permanently_asks_even_when_trashing_does_not() {
    let mut shell = Shell::new(&logs());
    shell.send(Event::Settings(model::SettingsEdit::ConfirmTrash(false)));
    shell.send(Event::Pane(Side::Left, pane::PaneInput::Click { row: 3, toggle: false, range: false }));
    shell.cmd(C::DeleteFiles);
    let view = shell.view();
    let [d] = view.dialogs.as_slice() else { panic!("one dialog") };
    let dialog::DialogKind::Confirm(c) = &d.kind else { panic!("a confirmation") };
    assert_eq!(c.title, "Delete “app.log” permanently?");
    assert!(c.danger);
}

#[test]
fn places_are_saved_per_host_and_jumped_to() {
    let mut shell = Shell::new(&logs());
    assert!(shell.view().panes[0].places.is_empty());
    shell.cmd(C::TogglePlace);
    let view = shell.view();
    assert_eq!(view.panes[0].places.len(), 1);
    assert_eq!(view.panes[0].places[0].name, "left");
    assert!(view.panes[0].place_saved);
    assert_eq!(view.panes[1].places.len(), 1, "the other pane is on the same host");
    shell.cmd(C::SwitchPane);
    shell.cmd(C::GoToPlace(0));
    assert_eq!(shell.view().panes[1].display, "/left");
    shell.cmd(C::GoToPlace(7));
    assert_eq!(shell.view().panes[1].display, "/left");
    shell.cmd(C::TogglePlace);
    assert!(shell.view().panes[0].places.is_empty());
}

#[test]
fn compare_marks_what_differs() {
    let mut a = file("changed.txt", 2);
    a.mtime = Some(2_000_000_000);
    let b = file("changed.txt", 1);
    let mut shell = Shell::new(&[
        ("/left", vec![a, file("same.txt", 1), file("only-left.txt", 1)]),
        ("/right", vec![b, file("same.txt", 1), file("only-right.txt", 1)]),
    ]);
    shell.cmd(C::CompareDirs);
    assert_eq!(shell.marked(Side::Left), ["changed.txt", "only-left.txt"]);
    assert_eq!(shell.marked(Side::Right), ["only-right.txt"]);
    assert!(shell.toasts.last().unwrap().starts_with("Left: 1 only there, 1 newer"));
}

#[test]
fn the_console_stays_bounded() {
    let mut console = model::Console::default();
    let line = "x".repeat(1023) + "\n";
    for _ in 0..600 {
        console.append(&line);
    }
    assert!(console.text.len() <= 512 * 1024, "{}", console.text.len());
    assert!(console.text.len() >= 256 * 1024);
    assert!(console.text.starts_with('x'), "cut at a line boundary");
    assert_eq!(console.version, 600);
}

