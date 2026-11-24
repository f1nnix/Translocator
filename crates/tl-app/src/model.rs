//! The whole state of the application, and the dispatch of every event to
//! the module that owns it.

use std::collections::HashMap;
use std::sync::Arc;

use crux_core::Command as CruxCommand;
use tl_model::{Bookmarks, PluginInfo, VPath};

use crate::capabilities::*;
use crate::conn::Conns;
use crate::dialog::Dialog;
use crate::editor::Editor;
use crate::ids::{ConnId, DialogId, EditorId, Ids, JobId, TabId};
use crate::jobs::Jobs;
use crate::pane::{Pane, Side};
use crate::quick_look::QuickLook;
use crate::settings::{AppearanceMode, DateFormat, Settings, SortKey, ViewMode};
use crate::{Event, Out, bookmarks, columns, dialog, editor, jobs, ops, pane, properties, quick_look, rename, sync};

/// Files put on the internal clipboard with ⌘C / ⌘X.
#[derive(Clone, Debug, PartialEq)]
pub struct FileClip {
    pub conn: ConnId,
    pub dir: VPath,
    pub names: Vec<Vec<u8>>,
    pub elevated: bool,
    pub cut: bool,
}

/// The console under the panes: what the command line printed.
#[derive(Clone, Debug, Default)]
pub struct Console {
    pub text: Arc<String>,
    /// Bumped on every change, so the shell knows when to copy the text.
    pub version: u64,
    pub shown: bool,
}

impl Console {
    /// Appends, keeping the text bounded: past 512 KiB only the last
    /// 256 KiB stay, cut at a line boundary.
    pub fn append(&mut self, text: &str) {
        let buf = Arc::make_mut(&mut self.text);
        buf.push_str(text);
        if buf.len() > 512 * 1024 {
            let mut cut = buf.len() - 256 * 1024;
            while !buf.is_char_boundary(cut) {
                cut += 1;
            }
            let cut = buf[cut..].find('\n').map(|i| cut + i + 1).unwrap_or(cut);
            buf.drain(..cut);
        }
        self.version += 1;
    }
}

#[derive(Debug, Default)]
pub struct Model {
    pub ids: Ids,
    /// `Start` has been handled and the stored state has arrived.
    pub ready: bool,
    pub settings: Arc<Settings>,
    pub bookmarks: Arc<Bookmarks>,
    /// One-line descriptions of the bookmarks, by id (from the plugins).
    pub summaries: Arc<HashMap<String, String>>,
    pub plugins: Arc<Vec<PluginInfo>>,
    pub conns: Conns,
    pub panes: [Pane; 2],
    pub active: Side,
    /// Modal dialogs, the last one on top.
    pub dialogs: Vec<Dialog>,
    pub jobs: Jobs,
    pub clipboard: Option<FileClip>,
    pub console: Console,
    /// Bumped to ask the shell to clear the command line.
    pub cmdline_clears: u64,
    pub quick_look: Option<QuickLook>,
    pub editors: Vec<Editor>,
    /// The jobs sheet is open.
    pub jobs_open: bool,
    /// Bumped to ask the shell to bring up the settings window.
    pub settings_requests: u64,
    /// Local folders for the locations menu, as last reported.
    pub local_folders: Arc<Vec<LocalFolder>>,
    /// Asks the shell to open a side's keyboard locations menu.
    pub locations_request: Option<(Side, u64)>,
    /// Asks the shell to put the keyboard on a pane or the command line.
    pub focus_request: Option<(FocusTarget, u64)>,
    pub boot: ops::Boot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusTarget {
    Pane(Side),
    CommandLine,
}

impl Model {
    pub fn pane(&self, side: Side) -> &Pane {
        &self.panes[side.index()]
    }

    pub fn pane_mut(&mut self, side: Side) -> &mut Pane {
        &mut self.panes[side.index()]
    }

    pub fn dialog(&self, id: DialogId) -> Option<&Dialog> {
        self.dialogs.iter().find(|d| d.id == id)
    }

    pub fn dialog_mut(&mut self, id: DialogId) -> Option<&mut Dialog> {
        self.dialogs.iter_mut().find(|d| d.id == id)
    }

    pub fn close_dialog(&mut self, id: DialogId) -> Option<Dialog> {
        let at = self.dialogs.iter().position(|d| d.id == id)?;
        Some(self.dialogs.remove(at))
    }

    pub fn focus(&mut self, target: FocusTarget) {
        let n = self.focus_request.map(|(_, n)| n + 1).unwrap_or(1);
        self.focus_request = Some((target, n));
    }

    /// Writes the settings after an edit.
    pub fn save_settings(&self, out: &mut Out) {
        out.push(CruxCommand::notify_shell(StoreOp::SaveSettings(Box::new((*self.settings).clone()))).into());
    }

    /// Writes the bookmarks after an edit; a failure is a toast.
    pub fn save_bookmarks(&self, out: &mut Out, what: &'static str) {
        let data = (*self.bookmarks).clone();
        out.push(
            CruxCommand::request_from_shell(StoreOp::SaveBookmarks(data))
                .then_send(move |saved| Event::Shell(Box::new(Reply::BookmarksSaved { what, saved }))),
        );
    }
}

/// Keyboard, menu and button commands. Those that act on a pane act on the
/// active one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    // cursor and marks
    CursorUp,
    CursorDown,
    /// In the grid, the icon to the side; in the column view, out to the
    /// folder above and into the one under the cursor. Nothing in the list.
    CursorLeft,
    CursorRight,
    /// How the active pane shows its folder.
    ViewAs(ViewMode),
    /// Moves by `rows` (what fits in the list, as the shell measured it).
    PageUp { rows: usize },
    PageDown { rows: usize },
    CursorHome,
    CursorEnd,
    MarkUp,
    MarkDown,
    ToggleMark,
    MarkAll,
    UnmarkAll,
    InvertMarks,
    MarkByMask,
    UnmarkByMask,
    // navigation
    Open,
    GoParent,
    GoBack,
    GoForward,
    GoHome,
    GoToPath,
    /// A printable character typed into the list: the type-ahead filter.
    Type(String),
    FilterBackspace,
    /// Escape in the list: clears the filter. Answers whether it did.
    ClearFilter,
    Refresh,
    ToggleHidden,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    SwitchPane,
    SwapPanes,
    MirrorPane,
    ToggleQuickLook,
    Locations(Option<Side>),
    TogglePlace,
    GoToPlace(usize),
    // files
    ViewFile,
    EditFile,
    NewFile,
    CopyFiles,
    MoveFiles,
    NewFolder,
    TrashFiles,
    DeleteFiles,
    RenameFile,
    Properties,
    CopyPath,
    OpenTerminal,
    OpenExternally,
    RevealInFinder,
    CalcSize,
    FindFiles,
    CompareDirs,
    SyncDirs,
    MultiRename,
    OpenArchive,
    ExtractArchive,
    PackFiles,
    Compress,
    Decompress,
    ClipboardCopy,
    ClipboardCut,
    ClipboardPaste,
    // connections and app
    ToggleSudo,
    Disconnect,
    BookmarkConnection,
    ManageBookmarks,
    NewBookmark,
    FocusCommandLine,
    FocusPane,
    ToggleJobs,
    ToggleConsole,
    OpenSettings,
    About,
    OpenHelp,
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandLineInput {
    /// Enter in the command line, with what it holds.
    Run(String),
}

/// Edits from the settings window and the toggles that write settings.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsEdit {
    RestoreFolders(bool),
    ShowHidden(bool),
    FoldersFirst(bool),
    Sort(SortKey),
    SortDescending(bool),
    ShowIcons(bool),
    ConfirmTrash(bool),
    FontSize(f32),
    RowHeight(f32),
    DateFormat(DateFormat),
    Terminal(String),
    AppearanceMode(AppearanceMode),
    LightTheme(String),
    DarkTheme(String),
    /// The keys of a command; `None` goes back to the defaults.
    Keys { command: String, keys: Option<Vec<String>> },
    /// Every setting of the General page back to its default.
    ResetGeneral,
}

/// An answer from the shell, with what it answers.
// A message that lives for one update; boxing would only move the bytes.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Reply {
    Loaded(StoreOutput),
    Plugins(PluginOutput),
    Summaries { ids: Vec<String>, summaries: Vec<String> },
    BookmarksSaved { what: &'static str, saved: StoreOutput },
    LocalFolders(PlatformOutput),
    Prompt(PromptOutput),
    Health(Health),
    Job { id: JobId, update: JobUpdate },
    Watch { tab: TabId, out: WatchOutput },
    Pane(pane::PaneReply),
    Columns(columns::ColumnsReply),
    Ops(ops::OpsReply),
    Editor(EditorId, editor::EditorReply),
    QuickLook(quick_look::QuickLookReply),
    Rename(DialogId, rename::RenameReply),
    Sync(DialogId, sync::SyncReply),
    Bookmarks(bookmarks::BookmarksReply),
    /// A notification-only effect finished; nothing to do.
    Nothing,
}

pub fn update(event: Event, model: &mut Model, out: &mut Out) {
    match event {
        Event::Start { seed } => ops::start(model, seed, out),
        Event::Quitting => {
            model.save_settings(out);
            out.push(CruxCommand::notify_shell(StoreOp::Flush).into());
        }
        Event::Command(cmd) => command(model, cmd, out),
        Event::Pane(side, input) => pane::input(model, side, input, out),
        Event::Focus(side) => {
            if model.active != side {
                model.active = side;
            }
        }
        Event::Dialog(id, input) => dialog::input(model, id, input, out),
        Event::CommandLine(CommandLineInput::Run(text)) => ops::run_command(model, text, out),
        Event::Jobs(input) => jobs::input(model, input, out),
        Event::Settings(edit) => ops::edit_settings(model, edit, out),
        Event::Bookmarks(input) => bookmarks::input(model, input, out),
        Event::Rename(id, input) => rename::input(model, id, input, out),
        Event::Sync(id, input) => sync::input(model, id, input, out),
        Event::Properties(id, input) => properties::input(model, id, input, out),
        Event::Editor(id, input) => editor::input(model, id, input, out),
        Event::DropExternal { side, paths } => ops::copy_external(model, side, paths, out),
        Event::Shell(reply) => self::reply(model, *reply, out),
    }
}

fn reply(model: &mut Model, reply: Reply, out: &mut Out) {
    match reply {
        Reply::Loaded(loaded) => ops::loaded(model, loaded, out),
        Reply::Plugins(plugins) => ops::plugins(model, plugins, out),
        Reply::Summaries { ids, summaries } => {
            let map = Arc::make_mut(&mut model.summaries);
            map.clear();
            map.extend(ids.into_iter().zip(summaries));
        }
        Reply::BookmarksSaved { what, saved } => {
            if let StoreOutput::Saved(Err(err)) = saved {
                out.error(format!("Cannot save the {what}: {err}"));
            }
        }
        Reply::LocalFolders(PlatformOutput::Folders(folders)) => model.local_folders = Arc::new(folders),
        Reply::LocalFolders(_) => {}
        Reply::Prompt(prompt) => dialog::prompt(model, prompt, out),
        Reply::Health(health) => ops::health(model, health, out),
        Reply::Job { id, update } => jobs::update(model, id, update, out),
        Reply::Watch { tab, out: change } => pane::watched(model, tab, change, out),
        Reply::Pane(reply) => pane::reply(model, reply, out),
        Reply::Columns(reply) => columns::reply(model, reply, out),
        Reply::Ops(reply) => ops::reply(model, reply, out),
        Reply::Editor(id, reply) => editor::reply(model, id, reply, out),
        Reply::QuickLook(reply) => quick_look::reply(model, reply, out),
        Reply::Rename(id, reply) => rename::reply(model, id, reply, out),
        Reply::Sync(id, reply) => sync::reply(model, id, reply, out),
        Reply::Bookmarks(reply) => bookmarks::reply(model, reply, out),
        Reply::Nothing => {}
    }
}

fn command(model: &mut Model, cmd: Command, out: &mut Out) {
    // What belongs to the app rather than to a pane works from the start.
    let app_level = matches!(cmd, Command::Quit | Command::About | Command::OpenSettings | Command::OpenHelp);
    if !model.ready && !app_level {
        return;
    }
    if pane::is_pane_command(&cmd) {
        let side = model.active;
        pane::command(model, side, cmd, out);
    } else {
        ops::command(model, cmd, out);
    }
}

/// A shell answer wrapped as an event.
pub fn shell(reply: Reply) -> Event {
    Event::Shell(Box::new(reply))
}
