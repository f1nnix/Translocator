//! What a native shell sends: every [`tl_app::Event`] a UI produces, in
//! FFI-friendly form, with the conversion into the core's event.
//!
//! Paths the shell made up itself (files dropped from Finder) cross as
//! UTF-8 strings; paths and names that came from the core (a breadcrumb, a
//! saved folder, a dragged entry) go back as the raw bytes the snapshot
//! carried, so a non-UTF-8 name on a server is never mangled.

use tl_app::{DialogId, EditorId, Event, JobId, model, pane};

use crate::types::{ConflictChoice, Side, SortKey, TransferMode, ViewMode, vpath};
use crate::{bookmarks, editor, properties, rename, settings, sync};

/// Everything the UI tells the core.
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum Input {
    /// A key, menu or button command, acting on the active pane (or the
    /// whole window).
    Command { command: Command },
    /// Mouse and pane-local input on one side.
    Pane { side: Side, input: PaneInput },
    /// A pane got the keyboard.
    Focus { side: Side },
    /// An answer or edit in a dialog, by its id in `Snapshot.dialogs`.
    Dialog { id: u64, input: DialogInput },
    CommandLine { input: CommandLineInput },
    Jobs { input: JobsInput },
    Settings { edit: settings::SettingsEdit },
    Bookmarks { input: bookmarks::BookmarksInput },
    Rename { dialog: u64, input: rename::RenameInput },
    Sync { dialog: u64, input: sync::SyncInput },
    Properties { dialog: u64, input: properties::PropertiesInput },
    /// A viewer or editor window, by its id in `Snapshot.editors`.
    Editor { editor: u64, input: editor::EditorInput },
    /// Files dropped from another application (absolute local paths).
    DropExternal { side: Side, paths: Vec<String> },
}

impl From<Input> for Event {
    fn from(input: Input) -> Event {
        match input {
            Input::Command { command } => Event::Command(command.into()),
            Input::Pane { side, input } => Event::Pane(side.into(), input.into()),
            Input::Focus { side } => Event::Focus(side.into()),
            Input::Dialog { id, input } => Event::Dialog(DialogId(id), input.into()),
            Input::CommandLine { input } => Event::CommandLine(input.into()),
            Input::Jobs { input } => Event::Jobs(input.into()),
            Input::Settings { edit } => Event::Settings(edit.into()),
            Input::Bookmarks { input } => Event::Bookmarks(input.into()),
            Input::Rename { dialog, input } => Event::Rename(DialogId(dialog), input.into()),
            Input::Sync { dialog, input } => Event::Sync(DialogId(dialog), input.into()),
            Input::Properties { dialog, input } => Event::Properties(DialogId(dialog), input.into()),
            Input::Editor { editor, input } => Event::Editor(EditorId(editor), input.into()),
            Input::DropExternal { side, paths } => Event::DropExternal {
                side: side.into(),
                paths: paths
                    .iter()
                    .map(|p| tl_app::tl_model::VPath::from_local(std::path::Path::new(p)))
                    .collect(),
            },
        }
    }
}

/// Keyboard, menu and button commands (`tl_app::model::Command`).
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    // cursor and marks
    CursorUp,
    CursorDown,
    /// In the grid, the icon to the side; in the column view, out to the
    /// folder above and into the one under the cursor.
    CursorLeft,
    CursorRight,
    /// How the active pane shows its folder.
    ViewAs { mode: ViewMode },
    /// Moves by `rows`: what fits in the list, as the shell measured it.
    PageUp { rows: u32 },
    PageDown { rows: u32 },
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
    Type { text: String },
    FilterBackspace,
    /// Escape in the list: clears the filter.
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
    /// The locations menu of a side, or of the active one.
    Locations { side: Option<Side> },
    TogglePlace,
    GoToPlace { index: u32 },
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

/// Generates both conversions for the variants without fields; the
/// others are listed by hand on each side.
macro_rules! commands {
    ($($name:ident),* $(,)?) => {
        impl From<Command> for model::Command {
            fn from(cmd: Command) -> model::Command {
                match cmd {
                    $(Command::$name => model::Command::$name,)*
                    Command::PageUp { rows } => model::Command::PageUp { rows: rows as usize },
                    Command::PageDown { rows } => model::Command::PageDown { rows: rows as usize },
                    Command::Type { text } => model::Command::Type(text),
                    Command::Locations { side } => model::Command::Locations(side.map(Into::into)),
                    Command::GoToPlace { index } => model::Command::GoToPlace(index as usize),
                    Command::ViewAs { mode } => model::Command::ViewAs(mode.into()),
                }
            }
        }

        impl From<model::Command> for Command {
            fn from(cmd: model::Command) -> Command {
                match cmd {
                    $(model::Command::$name => Command::$name,)*
                    model::Command::PageUp { rows } => Command::PageUp { rows: crate::types::usize32(rows) },
                    model::Command::PageDown { rows } => Command::PageDown { rows: crate::types::usize32(rows) },
                    model::Command::Type(text) => Command::Type { text },
                    model::Command::Locations(side) => Command::Locations { side: side.map(Into::into) },
                    model::Command::GoToPlace(index) => Command::GoToPlace { index: crate::types::usize32(index) },
                    model::Command::ViewAs(mode) => Command::ViewAs { mode: mode.into() },
                }
            }
        }
    };
}

commands!(
    CursorUp,
    CursorDown,
    CursorLeft,
    CursorRight,
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
    Open,
    GoParent,
    GoBack,
    GoForward,
    GoHome,
    GoToPath,
    FilterBackspace,
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
    TogglePlace,
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
);

/// What a drag from a pane carries (see `Core::drag`).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Dragged {
    pub side: Side,
    pub conn: u64,
    /// The folder the names are in, as raw bytes.
    pub dir: Vec<u8>,
    /// Raw entry names.
    pub names: Vec<Vec<u8>>,
    pub elevated: bool,
}

impl From<Dragged> for pane::Dragged {
    fn from(d: Dragged) -> Self {
        pane::Dragged {
            side: d.side.into(),
            conn: tl_app::ConnId(d.conn),
            dir: vpath(&d.dir),
            names: d.names,
            elevated: d.elevated,
        }
    }
}

/// Mouse and pane-local input (`tl_app::pane::PaneInput`).
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum PaneInput {
    /// A press on a row: `toggle` (⌘) toggles its mark, `range` (⇧) marks
    /// from the anchor; neither moves the cursor.
    Click { row: u32, toggle: bool, range: bool },
    DoubleClick { row: u32 },
    /// A right press: an unmarked row gets the cursor before the menu opens.
    ContextClick { row: u32 },
    SortBy { key: SortKey },
    SelectTab { index: u32 },
    CloseTab { index: u32 },
    /// A breadcrumb or a saved folder: the raw path from the snapshot.
    Navigate { path: Vec<u8> },
    /// A "This Mac" entry of the locations menu: its raw path.
    OpenLocal { path: Vec<u8> },
    OpenBookmark { id: String },
    RenamePlace { id: String },
    MovePlace { id: String, delta: i32 },
    RemovePlace { id: String },
    Retry,
    Reconnect,
    Disconnect,
    /// The locations menu opened: refresh what it lists.
    LocationsShown,
    /// Files from the other pane dropped here.
    Drop { drag: Dragged, mode: TransferMode },
    /// A press on a row of a column other than the pane's own, in the
    /// column view: `column` indexes `PaneSnapshot.columns`, and one past
    /// their end is the preview column.
    ColumnClick { column: u32, row: u32 },
    /// How many icons a row of the grid holds now.
    GridColumns { count: u32 },
    /// Switches this pane's view.
    ViewAs { mode: ViewMode },
}

impl From<PaneInput> for pane::PaneInput {
    fn from(input: PaneInput) -> Self {
        use pane::PaneInput as P;
        match input {
            PaneInput::Click { row, toggle, range } => P::Click {
                row: row as usize,
                toggle,
                range,
            },
            PaneInput::DoubleClick { row } => P::DoubleClick { row: row as usize },
            PaneInput::ContextClick { row } => P::ContextClick { row: row as usize },
            PaneInput::SortBy { key } => P::SortBy(key.into()),
            PaneInput::SelectTab { index } => P::SelectTab(index as usize),
            PaneInput::CloseTab { index } => P::CloseTab(index as usize),
            PaneInput::Navigate { path } => P::Navigate(vpath(&path)),
            PaneInput::OpenLocal { path } => P::OpenLocal(vpath(&path)),
            PaneInput::OpenBookmark { id } => P::OpenBookmark(id),
            PaneInput::RenamePlace { id } => P::RenamePlace(id),
            PaneInput::MovePlace { id, delta } => P::MovePlace {
                id,
                delta: delta as isize,
            },
            PaneInput::RemovePlace { id } => P::RemovePlace(id),
            PaneInput::Retry => P::Retry,
            PaneInput::Reconnect => P::Reconnect,
            PaneInput::Disconnect => P::Disconnect,
            PaneInput::LocationsShown => P::LocationsShown,
            PaneInput::Drop { drag, mode } => P::Drop {
                drag: drag.into(),
                mode: mode.into(),
            },
            PaneInput::ColumnClick { column, row } => P::ColumnClick {
                column: column as usize,
                row: row as usize,
            },
            PaneInput::GridColumns { count } => P::GridColumns(count as usize),
            PaneInput::ViewAs { mode } => P::ViewAs(mode.into()),
        }
    }
}

/// Edits and answers in a generic dialog (`tl_app::dialog::DialogInput`).
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum DialogInput {
    /// The text field changed.
    Text { text: String },
    /// The conflict dialog's "apply to all" box.
    ApplyAll { on: bool },
    /// OK, Enter.
    Submit,
    /// Cancel, Escape, or the dialog closed some other way.
    Cancel,
    /// A conflict dialog's button.
    Choose { choice: ConflictChoice },
    /// A search hit was clicked, by its index in the results.
    Pick { index: u32 },
    /// The about box's README button.
    OpenReadme,
}

impl From<DialogInput> for tl_app::dialog::DialogInput {
    fn from(input: DialogInput) -> Self {
        use tl_app::dialog::DialogInput as D;
        match input {
            DialogInput::Text { text } => D::Text(text),
            DialogInput::ApplyAll { on } => D::ApplyAll(on),
            DialogInput::Submit => D::Submit,
            DialogInput::Cancel => D::Cancel,
            DialogInput::Choose { choice } => D::Choose(choice.into()),
            DialogInput::Pick { index } => D::Pick(index as usize),
            DialogInput::OpenReadme => D::OpenReadme,
        }
    }
}

#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum CommandLineInput {
    /// Enter in the command line, with what it holds.
    Run { text: String },
}

impl From<CommandLineInput> for model::CommandLineInput {
    fn from(input: CommandLineInput) -> Self {
        match input {
            CommandLineInput::Run { text } => model::CommandLineInput::Run(text),
        }
    }
}

/// The jobs list and strip.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum JobsInput {
    Cancel { job: u64 },
    ClearFinished,
    /// The jobs popover was opened or closed by the shell itself.
    SheetShown { shown: bool },
}

impl From<JobsInput> for tl_app::jobs::JobsInput {
    fn from(input: JobsInput) -> Self {
        use tl_app::jobs::JobsInput as J;
        match input {
            JobsInput::Cancel { job } => J::Cancel(JobId(job)),
            JobsInput::ClearFinished => J::ClearFinished,
            JobsInput::SheetShown { shown } => J::SheetShown(shown),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(input: Input) -> String {
        format!("{:?}", Event::from(input))
    }

    #[test]
    fn commands_map_both_ways() {
        let all = [
            Command::PageDown { rows: 17 },
            Command::Type { text: "a".into() },
            Command::Locations { side: Some(Side::Right) },
            Command::Locations { side: None },
            Command::GoToPlace { index: 3 },
            Command::Quit,
            Command::ClipboardPaste,
        ];
        for cmd in all {
            let core: model::Command = cmd.clone().into();
            assert_eq!(Command::from(core), cmd);
        }
        assert_eq!(
            model::Command::from(Command::PageDown { rows: 17 }),
            model::Command::PageDown { rows: 17 }
        );
        // Every command of the shared key table has a mirror.
        for spec in tl_app::keys::COMMANDS {
            if let tl_app::keys::KeyTarget::Command(cmd) = &spec.target {
                let back: model::Command = Command::from(cmd.clone()).into();
                assert_eq!(&back, cmd, "{}", spec.id);
            }
        }
    }

    #[test]
    fn inputs_become_the_core_events() {
        assert_eq!(event(Input::Focus { side: Side::Right }), "Focus(Right)");
        assert_eq!(
            event(Input::Pane {
                side: Side::Left,
                input: PaneInput::Click {
                    row: 4,
                    toggle: true,
                    range: false
                }
            }),
            "Pane(Left, Click { row: 4, toggle: true, range: false })"
        );
        assert_eq!(
            event(Input::Dialog {
                id: 9,
                input: DialogInput::Choose {
                    choice: ConflictChoice::KeepBoth
                }
            }),
            "Dialog(DialogId(9), Choose(KeepBoth))"
        );
        assert_eq!(
            event(Input::Jobs {
                input: JobsInput::Cancel { job: 5 }
            }),
            "Jobs(Cancel(JobId(5)))"
        );
        let Event::Pane(_, pane::PaneInput::Navigate(path)) = Event::from(Input::Pane {
            side: Side::Left,
            input: PaneInput::Navigate {
                path: b"/srv/caf\xe9".to_vec(),
            },
        }) else {
            panic!("not a navigation");
        };
        assert_eq!(path.as_bytes(), b"/srv/caf\xe9", "raw bytes survive");
        let Event::DropExternal { side, paths } = Event::from(Input::DropExternal {
            side: Side::Right,
            paths: vec!["/Users/me/a.txt".into()],
        }) else {
            panic!("not a drop");
        };
        assert_eq!(side, tl_app::Side::Right);
        assert_eq!(paths[0].display(), "/Users/me/a.txt");
        let Event::Pane(_, pane::PaneInput::Drop { drag, mode }) = Event::from(Input::Pane {
            side: Side::Right,
            input: PaneInput::Drop {
                drag: Dragged {
                    side: Side::Left,
                    conn: 3,
                    dir: b"/tmp".to_vec(),
                    names: vec![b"x".to_vec()],
                    elevated: true,
                },
                mode: TransferMode::Move,
            },
        }) else {
            panic!("not a drop");
        };
        assert_eq!(drag.conn, tl_app::ConnId(3));
        assert_eq!(drag.names, vec![b"x".to_vec()]);
        assert_eq!(mode, tl_app::tl_model::transfer::TransferMode::Move);
    }
}
