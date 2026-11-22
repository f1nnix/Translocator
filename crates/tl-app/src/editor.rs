//! Built-in viewer (F3) and editor (F4) windows. Files on servers are read
//! and written over the same provider as the pane, so saving through sudo
//! works like `sudo nano` on the server.
//!
//! Each window is an [`Editor`] in the model. It is read first (a stat,
//! then a read) and only shows up in [`views`] once the text is there, so
//! the shell opens a window per [`EditorView`] it sees and closes the
//! window whose editor is gone. The shell's text widget owns the live text:
//! it loads [`EditorView::content`] whenever [`EditorView::content_version`]
//! changes, says when the first edit happens ([`EditorInput::Changed`]) and
//! hands the text over only to save it.

use std::sync::Arc;

use tl_model::util::human_size;
use tl_model::{VPath, VfsError, VfsResult};

use crate::Out;
use crate::capabilities::*;
use crate::dialog::{self, ConfirmPurpose};
use crate::ids::{ConnId, EditorId};
use crate::jobs::{self, Spec};
use crate::model::{Model, Reply};
use crate::pane;

/// The viewer reads at most this much.
pub const VIEW_LIMIT: u64 = 8 * 1024 * 1024;
/// Larger files are not opened for editing.
pub const EDIT_LIMIT: u64 = 16 * 1024 * 1024;
/// A NUL in this much of the beginning means the file is not text.
const SNIFF: usize = 8192;

/// Where an editor is in its life.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Waiting for the stat.
    Statting,
    /// Waiting for the read; the size is what the stat said.
    Reading { size: u64 },
    /// The window is open.
    Ready,
}

/// A notification inside the editor's own window ("Saved").
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub kind: ToastKind,
    pub message: String,
    /// Bumped for each one, so the shell shows a repeated message again.
    pub seq: u64,
}

/// The native "Save changes?" prompt the shell should show.
#[derive(Clone, Debug, PartialEq)]
pub struct ClosePrompt {
    /// `Save changes to “name”?`
    pub message: String,
    pub detail: &'static str,
    /// In order: Save, Don't Save, Cancel.
    pub buttons: [&'static str; 3],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Editor {
    pub id: EditorId,
    pub conn: ConnId,
    pub path: VPath,
    /// Reads and writes run as root.
    pub elevated: bool,
    pub readonly: bool,
    pub status: Status,
    /// The window title: `name — parent[ (sudo)]`, fixed when it opens.
    pub title: String,
    /// `/path` locally, `label:/path` on a server.
    pub full_path: String,
    /// Syntax for the text widget, from [`language_for`].
    pub language: &'static str,
    /// The warning bar under the header.
    pub notice: Option<String>,
    pub dirty: bool,
    pub saving: bool,
    /// The text the shell's widget should hold, as of `content_version`.
    pub content: Arc<str>,
    /// Bumped whenever `content` should replace what the widget holds.
    pub content_version: u64,
    pub close_prompt: Option<ClosePrompt>,
    /// The window closes once the running save succeeds.
    close_after_save: bool,
    /// The text of the save in flight, kept for a retry with sudo.
    saving_text: Option<String>,
    pub notification: Option<Notice>,
}

impl Editor {
    pub fn name(&self) -> String {
        self.path.file_name_lossy()
    }

    pub fn is_ready(&self) -> bool {
        self.status == Status::Ready
    }

    fn notify(&mut self, kind: ToastKind, message: impl Into<String>) {
        let seq = self.notification.as_ref().map_or(1, |n| n.seq + 1);
        self.notification = Some(Notice {
            kind,
            message: message.into(),
            seq,
        });
    }
}

/// What the shell sends for an editor window.
#[derive(Clone, Debug, PartialEq)]
pub enum EditorInput {
    /// The text widget changed (the shell may send this for every edit;
    /// only the first one of an editable file matters).
    Changed,
    /// ⌘S or the Save button, with the widget's text.
    Save { text: String },
    /// ⌘W or the window's close button. The shell keeps the window open:
    /// it goes when the editor leaves the view, or after a prompt.
    Close,
    /// The answer to [`EditorView::close_prompt`].
    CloseAnswer(CloseChoice),
    /// Close without asking, discarding changes.
    CloseNow,
    /// The window is gone already (the app quits, the shell closed it).
    Closed,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CloseChoice {
    /// Save, then close.
    Save { text: String },
    DontSave,
    Cancel,
}

#[derive(Debug)]
pub enum EditorReply {
    Stat(FsOutput),
    Read(FsOutput),
}

/// What a finished save job does next.
#[derive(Clone, Debug, PartialEq)]
pub enum After {
    Saved,
}

/// What a yes in a confirmation asked by an editor does.
#[derive(Clone, Debug, PartialEq)]
pub enum Confirm {
    /// Open the file again with sudo (the first try is gone).
    OpenElevated { conn: ConnId, path: VPath, readonly: bool },
    /// Save the editor's text again with sudo.
    SaveElevated,
}

/// The syntax the editor and quick look colour a file with, by name.
pub fn language_for(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match (lower.as_str(), ext) {
        ("makefile" | "gnumakefile", _) => "make",
        ("dockerfile", _) => "bash",
        (_, "rs") => "rust",
        (_, "toml") => "toml",
        (_, "json" | "jsonc") => "json",
        (_, "md" | "markdown") => "markdown",
        (_, "py") => "python",
        (_, "js" | "mjs" | "cjs" | "jsx") => "javascript",
        (_, "ts" | "mts" | "cts") => "typescript",
        (_, "tsx") => "tsx",
        (_, "yaml" | "yml") => "yaml",
        (_, "go") => "go",
        (_, "c" | "h") => "c",
        (_, "cpp" | "cc" | "cxx" | "hpp" | "hh") => "cpp",
        (_, "html" | "htm") => "html",
        (_, "css") => "css",
        (_, "sql") => "sql",
        (_, "diff" | "patch") => "diff",
        (_, "sh" | "bash" | "zsh" | "command" | "bashrc" | "zshrc" | "profile") => "bash",
        (_, "mk") => "make",
        _ if lower.starts_with(".bash") || lower.starts_with(".zsh") || lower == ".profile" => "bash",
        _ => "text",
    }
}

fn get(model: &mut Model, id: EditorId) -> Option<&mut Editor> {
    model.editors.iter_mut().find(|e| e.id == id)
}

fn remove(model: &mut Model, id: EditorId) -> Option<Editor> {
    let at = model.editors.iter().position(|e| e.id == id)?;
    Some(model.editors.remove(at))
}

/// Reads the file, then opens a window for it.
pub fn open(model: &mut Model, conn: ConnId, path: VPath, elevated: bool, readonly: bool, out: &mut Out) {
    let id = model.ids.editor();
    let parent = path.parent().unwrap_or_else(VPath::root);
    let title = format!(
        "{} — {}{}",
        path.file_name_lossy(),
        model.conns.display(conn, &parent),
        if elevated { " (sudo)" } else { "" }
    );
    let editor = Editor {
        id,
        conn,
        language: language_for(&path.file_name_lossy()),
        full_path: model.conns.full_path(conn, &path),
        path: path.clone(),
        elevated,
        readonly,
        status: Status::Statting,
        title,
        notice: None,
        dirty: false,
        saving: false,
        content: Arc::from(""),
        content_version: 0,
        close_prompt: None,
        close_after_save: false,
        saving_text: None,
        notification: None,
    };
    model.editors.push(editor);
    out.fs(FsOp::Stat { conn, path, elevated }, move |o| Reply::Editor(id, EditorReply::Stat(o)));
}

pub fn input(model: &mut Model, id: EditorId, input: EditorInput, out: &mut Out) {
    match input {
        EditorInput::Changed => {
            if let Some(e) = get(model, id)
                && e.is_ready()
                && !e.readonly
            {
                e.dirty = true;
            }
        }
        EditorInput::Save { text } => save(model, id, text, false, out),
        EditorInput::Close => {
            let Some(e) = get(model, id) else { return };
            if e.is_ready() && e.dirty {
                e.close_prompt = Some(ClosePrompt {
                    message: format!("Save changes to “{}”?", e.name()),
                    detail: "Your changes will be lost if you don't save them.",
                    buttons: ["Save", "Don't Save", "Cancel"],
                });
            } else {
                remove(model, id);
            }
        }
        EditorInput::CloseAnswer(choice) => {
            let Some(e) = get(model, id) else { return };
            e.close_prompt = None;
            match choice {
                CloseChoice::Save { text } => save(model, id, text, true, out),
                CloseChoice::DontSave => {
                    remove(model, id);
                }
                CloseChoice::Cancel => {}
            }
        }
        EditorInput::CloseNow | EditorInput::Closed => {
            remove(model, id);
        }
    }
}

/// Writes the text; `close` closes the window once it is saved.
fn save(model: &mut Model, id: EditorId, text: String, close: bool, out: &mut Out) {
    let Some(e) = get(model, id) else { return };
    if !e.is_ready() || e.readonly || e.saving {
        return;
    }
    e.saving = true;
    e.close_after_save = close;
    let work = Work::WriteFile {
        conn: e.conn,
        path: e.path.clone(),
        data: text.clone().into_bytes(),
        elevated: e.elevated,
    };
    e.saving_text = Some(text);
    jobs::start(model, Spec::quiet(work, jobs::After::Editor(id, After::Saved)).no_retry(), out);
}

/// What came back from the stat or the read.
pub fn reply(model: &mut Model, id: EditorId, reply: EditorReply, out: &mut Out) {
    let Some(e) = get(model, id) else { return };
    let (conn, elevated, readonly) = (e.conn, e.elevated, e.readonly);
    let path = e.path.clone();
    let result = match (reply, &e.status) {
        (EditorReply::Stat(FsOutput::Stat(stat)), Status::Statting) => {
            match stat.and_then(|meta| meta.ok_or_else(|| VfsError::NotFound(path.display()))) {
                Ok(meta) if !readonly && meta.size > EDIT_LIMIT => Err(VfsError::other(format!(
                    "{} is too large to edit ({}). Press F3 to view it.",
                    path.file_name_lossy(),
                    human_size(meta.size)
                ))),
                Ok(meta) => {
                    e.status = Status::Reading { size: meta.size };
                    let limit = if readonly { VIEW_LIMIT } else { EDIT_LIMIT };
                    let op = FsOp::Read {
                        conn,
                        path,
                        limit,
                        elevated,
                    };
                    out.fs(op, move |o| Reply::Editor(id, EditorReply::Read(o)));
                    return;
                }
                Err(err) => Err(err),
            }
        }
        (EditorReply::Read(FsOutput::Read(read)), Status::Reading { size }) => {
            let size = *size;
            read.map(|(data, _)| loaded(e, data, size))
        }
        _ => return,
    };
    let Err(err) = result else { return };
    remove(model, id);
    match err {
        VfsError::PermissionDenied(msg) if !elevated && model.conns.caps(conn).elevation => {
            dialog::ask_confirm(
                model,
                "Permission denied",
                format!("{msg}\n\nOpen it with sudo?"),
                "Use sudo",
                false,
                ConfirmPurpose::Editor {
                    editor: id,
                    action: Confirm::OpenElevated { conn, path, readonly },
                },
            );
        }
        err => out.error(err.to_string()),
    }
}

/// The file is read: decides what the window shows and whether it can be
/// edited.
fn loaded(e: &mut Editor, data: Vec<u8>, file_size: u64) {
    let mut notice = None;
    let read = data.len() as u64;
    if read > 0 && read < file_size {
        notice = Some(format!(
            "Showing the first {} of {}.",
            human_size(read),
            human_size(file_size)
        ));
    }
    let binary = data.iter().take(SNIFF).any(|b| *b == 0);
    let text = match String::from_utf8(data) {
        Ok(text) => text,
        Err(err) => {
            if !e.readonly {
                notice = Some("Not valid UTF-8: opened read-only so saving cannot damage it.".into());
            }
            e.readonly = true;
            String::from_utf8_lossy(err.as_bytes()).into_owned()
        }
    };
    if binary {
        e.readonly = true;
        notice = Some("Binary file: shown as text, read-only.".into());
    }
    e.notice = notice;
    e.content = Arc::from(text);
    e.content_version += 1;
    e.status = Status::Ready;
}

/// A save job ended.
pub fn finished(model: &mut Model, id: EditorId, then: After, result: VfsResult<JobOutput>, out: &mut Out) {
    let After::Saved = then;
    let can_elevate = model.editors.iter().find(|e| e.id == id).is_some_and(|e| model.conns.caps(e.conn).elevation);
    let Some(e) = get(model, id) else { return };
    e.saving = false;
    let (conn, elevated, close) = (e.conn, e.elevated, e.close_after_save);
    let dir = e.path.parent().unwrap_or_else(VPath::root);
    match result {
        Ok(_) => {
            e.dirty = false;
            e.notice = None;
            e.saving_text = None;
            if !close {
                e.notify(ToastKind::Success, "Saved");
            }
            let key = model.conns.location_key(conn);
            pane::refresh_dir(model, &key, &dir, out);
            if close {
                remove(model, id);
            }
        }
        Err(VfsError::PermissionDenied(msg)) if !elevated && can_elevate => {
            dialog::ask_confirm(
                model,
                "Permission denied",
                format!("{msg}\n\nSave as root with sudo? The file keeps its owner and permissions."),
                "Save with sudo",
                false,
                ConfirmPurpose::Editor {
                    editor: id,
                    action: Confirm::SaveElevated,
                },
            );
        }
        Err(err) => {
            e.saving_text = None;
            dialog::error(model, "Cannot save", err.to_string());
        }
    }
}

/// The user said yes to a question an editor asked.
pub fn confirmed(model: &mut Model, id: EditorId, action: Confirm, out: &mut Out) {
    match action {
        Confirm::OpenElevated { conn, path, readonly } => open(model, conn, path, true, readonly, out),
        Confirm::SaveElevated => {
            let Some(e) = get(model, id) else { return };
            let Some(text) = e.saving_text.take() else { return };
            e.elevated = true;
            let close = e.close_after_save;
            save(model, id, text, close, out);
        }
    }
}

/// An open editor window, as the shell draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct EditorView {
    pub id: EditorId,
    pub title: String,
    /// The header's path, in a monospace font.
    pub full_path: String,
    /// The "sudo" tag.
    pub elevated: bool,
    /// The "read-only" tag; the text widget is read-only and there is no
    /// Save button.
    pub readonly: bool,
    /// The "modified" tag.
    pub dirty: bool,
    pub saving: bool,
    /// "Save" or "Saving…".
    pub save_label: &'static str,
    /// The Save button is enabled.
    pub can_save: bool,
    pub language: &'static str,
    pub notice: Option<String>,
    pub content: Arc<str>,
    pub content_version: u64,
    pub close_prompt: Option<ClosePrompt>,
    /// Shown in this window when its `seq` changes.
    pub notification: Option<Notice>,
    /// The footer's right side.
    pub hint: &'static str,
}

pub fn views(model: &Model) -> Vec<EditorView> {
    model
        .editors
        .iter()
        .filter(|e| e.is_ready())
        .map(|e| EditorView {
            id: e.id,
            title: e.title.clone(),
            full_path: e.full_path.clone(),
            elevated: e.elevated,
            readonly: e.readonly,
            dirty: e.dirty,
            saving: e.saving,
            save_label: if e.saving { "Saving…" } else { "Save" },
            can_save: !e.readonly && e.dirty && !e.saving,
            language: e.language,
            notice: e.notice.clone(),
            content: e.content.clone(),
            content_version: e.content_version,
            close_prompt: e.close_prompt.clone(),
            notification: e.notification.clone(),
            hint: if e.readonly { "F3 viewer · ⌘W close" } else { "⌘S save · ⌘W close" },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crux_core::{App as _, Command, Request};
    use tl_model::{Capabilities, Entry, EntryKind};

    use super::*;
    use crate::conn::ConnInfo;
    use crate::dialog::{DialogInput, DialogKind};
    use crate::ids::LOCAL;
    use crate::{Cmd, Effect, Event, Translocator};

    struct Harness {
        model: Model,
        cmds: Vec<Cmd>,
    }

    impl Harness {
        fn new(elevation: bool) -> Self {
            let mut model = Model {
                ready: true,
                ..Model::default()
            };
            model.conns.insert(ConnInfo {
                id: LOCAL,
                plugin: "local".into(),
                label: "Local".into(),
                location_key: "local".into(),
                caps: Capabilities {
                    elevation,
                    ..Default::default()
                },
                home: VPath::root(),
                bookmark: None,
                host: None,
                server_info: None,
                local_home: None,
            });
            Harness { model, cmds: Vec::new() }
        }

        fn keep(&mut self, mut cmd: Cmd) -> Vec<Effect> {
            let effects = cmd.effects().filter(|e| !e.is_render()).collect();
            self.cmds.push(cmd);
            effects
        }

        fn run(&mut self, f: impl FnOnce(&mut Model, &mut Out)) -> Vec<Effect> {
            let mut out = Out::default();
            f(&mut self.model, &mut out);
            self.keep(Command::all(out.cmds))
        }

        fn send(&mut self, event: Event) -> Vec<Effect> {
            let cmd = Translocator.update(event, &mut self.model);
            self.keep(cmd)
        }

        /// Answers a request and runs the events it sends back.
        fn resolve<Op: crux_core::capability::Operation>(
            &mut self,
            mut req: Request<Op>,
            output: Op::Output,
        ) -> Vec<Effect> {
            req.resolve(output).unwrap();
            let events: Vec<Event> = self.cmds.iter_mut().flat_map(|c| c.events().collect::<Vec<_>>()).collect();
            events.into_iter().flat_map(|e| self.send(e)).collect()
        }

        fn editor(&mut self, id: EditorId, input: EditorInput) -> Vec<Effect> {
            self.send(Event::Editor(id, input))
        }
    }

    fn fs(effects: Vec<Effect>) -> Request<FsOp> {
        let mut fs: Vec<_> = effects.into_iter().filter_map(Effect::into_fs).collect();
        assert_eq!(fs.len(), 1, "{fs:?}");
        fs.pop().unwrap()
    }

    fn job(effects: Vec<Effect>) -> Request<JobOp> {
        let mut jobs: Vec<_> = effects.into_iter().filter_map(Effect::into_job).collect();
        assert_eq!(jobs.len(), 1, "{jobs:?}");
        jobs.pop().unwrap()
    }

    fn toasts(effects: &[Effect]) -> Vec<String> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Platform(req) => match &req.operation {
                    PlatformOp::Toast { message, .. } => Some(message.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    fn file(size: u64) -> Entry {
        let mut entry = Entry::new("a.txt", EntryKind::File);
        entry.size = size;
        entry
    }

    /// Opens `/a.txt` and answers the stat; returns the editor and what
    /// the answer asked for.
    fn open_stat(h: &mut Harness, readonly: bool, stat: VfsResult<Option<Entry>>) -> (EditorId, Vec<Effect>) {
        let effects = h.run(|m, out| open(m, LOCAL, VPath::parse("/a.txt"), false, readonly, out));
        let id = h.model.editors.last().unwrap().id;
        let req = fs(effects);
        assert_eq!(
            req.operation,
            FsOp::Stat {
                conn: LOCAL,
                path: VPath::parse("/a.txt"),
                elevated: false
            }
        );
        assert!(views(&h.model).is_empty(), "no window before the text is read");
        (id, h.resolve(req, FsOutput::Stat(stat)))
    }

    fn open_ready(h: &mut Harness, readonly: bool, data: &[u8], size: u64) -> EditorId {
        let (id, effects) = open_stat(h, readonly, Ok(Some(file(size))));
        let req = fs(effects);
        let limit = if readonly { VIEW_LIMIT } else { EDIT_LIMIT };
        assert!(matches!(req.operation, FsOp::Read { limit: l, .. } if l == limit));
        h.resolve(req, FsOutput::Read(Ok((data.to_vec(), size))));
        id
    }

    #[test]
    fn languages() {
        assert_eq!(language_for("main.rs"), "rust");
        assert_eq!(language_for("Makefile"), "make");
        assert_eq!(language_for("rules.mk"), "make");
        assert_eq!(language_for("Dockerfile"), "bash");
        assert_eq!(language_for("nginx.conf"), "text");
        assert_eq!(language_for(".bashrc"), "bash");
        assert_eq!(language_for(".bash_profile"), "bash");
        assert_eq!(language_for("App.TSX"), "tsx");
    }

    #[test]
    fn a_file_is_read_then_shown() {
        let mut h = Harness::new(false);
        let id = open_ready(&mut h, false, b"hello\n", 6);
        let v = &views(&h.model)[0];
        assert_eq!(v.id, id);
        assert_eq!(v.title, "a.txt — /");
        assert_eq!(v.full_path, "/a.txt");
        assert_eq!(&*v.content, "hello\n");
        assert_eq!(v.content_version, 1);
        assert_eq!(v.language, "text");
        assert_eq!(v.notice, None);
        assert!(!v.readonly && !v.dirty && !v.can_save);
        assert_eq!(v.save_label, "Save");
        assert_eq!(v.hint, "⌘S save · ⌘W close");
    }

    #[test]
    fn the_viewer_reads_less_and_says_so() {
        let mut h = Harness::new(false);
        open_ready(&mut h, true, b"abcd", 10);
        let v = &views(&h.model)[0];
        assert!(v.readonly);
        assert_eq!(v.notice.as_deref(), Some("Showing the first 4 B of 10 B."));
        assert_eq!(v.hint, "F3 viewer · ⌘W close");
    }

    #[test]
    fn what_is_not_text_opens_read_only() {
        let mut h = Harness::new(false);
        open_ready(&mut h, false, b"caf\xe9", 4);
        let v = &views(&h.model)[0];
        assert!(v.readonly);
        assert_eq!(v.notice.as_deref(), Some("Not valid UTF-8: opened read-only so saving cannot damage it."));

        let mut h = Harness::new(false);
        open_ready(&mut h, true, b"caf\xe9", 4);
        assert_eq!(views(&h.model)[0].notice, None, "a viewer says nothing about UTF-8");

        let mut h = Harness::new(false);
        open_ready(&mut h, false, b"head\0tail", 9);
        let v = &views(&h.model)[0];
        assert!(v.readonly);
        assert_eq!(v.notice.as_deref(), Some("Binary file: shown as text, read-only."));
    }

    #[test]
    fn failures_to_open_are_toasts() {
        let mut h = Harness::new(false);
        let (_, effects) = open_stat(&mut h, false, Ok(None));
        assert_eq!(toasts(&effects), ["No such file or directory: /a.txt"]);
        assert!(h.model.editors.is_empty());

        let mut h = Harness::new(false);
        let (_, effects) = open_stat(&mut h, false, Ok(Some(file(EDIT_LIMIT + 1))));
        assert_eq!(toasts(&effects), ["a.txt is too large to edit (16.0 MB). Press F3 to view it."]);
        assert!(effects.iter().all(|e| !matches!(e, Effect::Fs(_))));
        assert!(h.model.editors.is_empty());

        // The viewer reads the beginning of a large file.
        let mut h = Harness::new(false);
        let (_, effects) = open_stat(&mut h, true, Ok(Some(file(EDIT_LIMIT + 1))));
        fs(effects);

        // Without sudo, permission denied is a toast too.
        let mut h = Harness::new(false);
        let (_, effects) = open_stat(&mut h, false, Err(VfsError::PermissionDenied("/a.txt".into())));
        assert_eq!(toasts(&effects), ["Permission denied: /a.txt"]);
        assert!(h.model.dialogs.is_empty());
    }

    #[test]
    fn permission_denied_offers_sudo() {
        let mut h = Harness::new(true);
        let (_, effects) = open_stat(&mut h, false, Err(VfsError::PermissionDenied("/a.txt".into())));
        assert!(toasts(&effects).is_empty());
        assert!(h.model.editors.is_empty());
        let dialog = h.model.dialogs.last().unwrap().clone();
        let DialogKind::Confirm(confirm) = &dialog.kind else { panic!("{dialog:?}") };
        assert_eq!(confirm.title, "Permission denied");
        assert_eq!(confirm.message, "/a.txt\n\nOpen it with sudo?");
        assert_eq!(confirm.ok, "Use sudo");
        let req = fs(h.send(Event::Dialog(dialog.id, DialogInput::Submit)));
        assert!(matches!(req.operation, FsOp::Stat { elevated: true, .. }));
        let req = fs(h.resolve(req, FsOutput::Stat(Ok(Some(file(3))))));
        assert!(matches!(req.operation, FsOp::Read { elevated: true, .. }));
        h.resolve(req, FsOutput::Read(Ok((b"abc".to_vec(), 3))));
        let v = &views(&h.model)[0];
        assert!(v.elevated);
        assert_eq!(v.title, "a.txt — / (sudo)");
    }

    #[test]
    fn closing_a_modified_file_asks_first() {
        let mut h = Harness::new(false);
        let id = open_ready(&mut h, false, b"one", 3);
        h.editor(id, EditorInput::Changed);
        assert!(views(&h.model)[0].dirty && views(&h.model)[0].can_save);

        h.editor(id, EditorInput::Close);
        let prompt = views(&h.model)[0].close_prompt.clone().unwrap();
        assert_eq!(prompt.message, "Save changes to “a.txt”?");
        assert_eq!(prompt.detail, "Your changes will be lost if you don't save them.");
        assert_eq!(prompt.buttons, ["Save", "Don't Save", "Cancel"]);

        h.editor(id, EditorInput::CloseAnswer(CloseChoice::Cancel));
        assert_eq!(views(&h.model)[0].close_prompt, None);

        h.editor(id, EditorInput::Close);
        let req = job(h.editor(id, EditorInput::CloseAnswer(CloseChoice::Save { text: "two".into() })));
        let JobOp::Start { work, .. } = &req.operation else { panic!() };
        assert_eq!(
            *work,
            Work::WriteFile {
                conn: LOCAL,
                path: VPath::parse("/a.txt"),
                data: b"two".to_vec(),
                elevated: false
            }
        );
        assert!(views(&h.model)[0].saving);
        assert_eq!(views(&h.model)[0].save_label, "Saving…");
        let effects = h.resolve(req, JobUpdate::Finished(Ok(JobOutput::Done)));
        assert!(h.model.editors.is_empty(), "saved, then closed");
        // Both panes show `/`, and both list it again.
        let lists: Vec<_> = effects.into_iter().filter_map(Effect::into_fs).collect();
        assert_eq!(lists.len(), 2);
        assert!(lists.iter().all(|l| matches!(&l.operation, FsOp::List { path, .. } if path.is_root())));

        // Don't Save closes, as does closing an unmodified file.
        let mut h = Harness::new(false);
        let id = open_ready(&mut h, false, b"one", 3);
        h.editor(id, EditorInput::Changed);
        h.editor(id, EditorInput::Close);
        h.editor(id, EditorInput::CloseAnswer(CloseChoice::DontSave));
        assert!(h.model.editors.is_empty());
        let id = open_ready(&mut h, false, b"one", 3);
        h.editor(id, EditorInput::Close);
        assert!(h.model.editors.is_empty());
        let id = open_ready(&mut h, false, b"one", 3);
        h.editor(id, EditorInput::Closed);
        assert!(h.model.editors.is_empty());
    }

    #[test]
    fn saving_clears_modified_and_says_so() {
        let mut h = Harness::new(false);
        let id = open_ready(&mut h, true, b"abcd", 10);
        h.editor(id, EditorInput::Changed);
        assert!(!views(&h.model)[0].dirty, "a viewer is never modified");
        assert!(h.editor(id, EditorInput::Save { text: "x".into() }).is_empty());

        let mut h = Harness::new(false);
        let id = open_ready(&mut h, false, b"one", 3);
        h.editor(id, EditorInput::Changed);
        let req = job(h.editor(id, EditorInput::Save { text: "two".into() }));
        assert!(h.editor(id, EditorInput::Save { text: "two".into() }).is_empty(), "one save at a time");
        h.resolve(req, JobUpdate::Finished(Ok(JobOutput::Done)));
        let v = &views(&h.model)[0];
        assert!(!v.dirty && !v.saving);
        let notice = v.notification.clone().unwrap();
        assert_eq!((notice.kind, notice.message.as_str(), notice.seq), (ToastKind::Success, "Saved", 1));
    }

    #[test]
    fn a_denied_save_offers_sudo() {
        let mut h = Harness::new(true);
        let id = open_ready(&mut h, false, b"one", 3);
        h.editor(id, EditorInput::Changed);
        let req = job(h.editor(id, EditorInput::Save { text: "two".into() }));
        h.resolve(req, JobUpdate::Finished(Err(VfsError::PermissionDenied("/a.txt".into()))));
        let dialog = h.model.dialogs.last().unwrap().clone();
        let DialogKind::Confirm(confirm) = &dialog.kind else { panic!("{dialog:?}") };
        assert_eq!(
            confirm.message,
            "/a.txt\n\nSave as root with sudo? The file keeps its owner and permissions."
        );
        assert_eq!(confirm.ok, "Save with sudo");
        let req = job(h.send(Event::Dialog(dialog.id, DialogInput::Submit)));
        let JobOp::Start { work, .. } = &req.operation else { panic!() };
        assert!(matches!(work, Work::WriteFile { elevated: true, data, .. } if data == b"two"));
        h.resolve(req, JobUpdate::Finished(Ok(JobOutput::Done)));
        let v = &views(&h.model)[0];
        assert!(v.elevated && !v.dirty);
    }

    #[test]
    fn other_save_errors_are_dialogs() {
        let mut h = Harness::new(true);
        let id = open_ready(&mut h, false, b"one", 3);
        h.editor(id, EditorInput::Changed);
        let req = job(h.editor(id, EditorInput::Save { text: "two".into() }));
        h.resolve(req, JobUpdate::Finished(Err(VfsError::other("disk full"))));
        let dialog = h.model.dialogs.last().unwrap();
        assert_eq!(
            dialog.kind,
            DialogKind::Message {
                title: "Cannot save".into(),
                message: "disk full".into()
            }
        );
        let v = &views(&h.model)[0];
        assert!(v.dirty && !v.saving);
    }
}
