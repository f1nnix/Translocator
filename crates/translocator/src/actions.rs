//! The actions the application can perform, and the menu bar.
//!
//! Which keys reach them is [`crate::keymap`]'s table: classic dual-pane
//! bindings (F3–F8, Tab, Space/Insert to mark) plus the macOS conventions
//! people expect (⌘⌫ to trash, ⌘C/⌘V, ⌘[ / ⌘]), all of them rebindable.

use gpui_kit::*;

actions!(
    tl,
    [
        // Cursor and marking.
        CursorUp,
        CursorDown,
        PageUp,
        PageDown,
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
        // Navigation.
        Open,
        GoParent,
        GoBack,
        GoForward,
        GoHome,
        GoToPath,
        FilterBackspace,
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
        LocationsLeft,
        LocationsRight,
        Locations,
        // File operations.
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
        // Connections and app.
        ToggleSudo,
        Disconnect,
        TogglePlace,
        BookmarkConnection,
        ManageBookmarks,
        NewBookmark,
        FocusCommandLine,
        FocusPane,
        ToggleJobs,
        ToggleConsole,
        OpenSettings,
        Quit,
        // Editor window.
        SaveFile,
        CloseEditor,
    ]
);

/// Jump to the n-th saved folder of the pane's host (⌘1…⌘9).
#[derive(Clone, Debug, Default, PartialEq, Action)]
#[action(namespace = tl, no_json)]
pub struct GoToPlace {
    pub index: usize,
}

/// Key context of a file pane.
pub const PANE: &str = "FilePane";
/// Key context of the main window (panes, command line, function bar).
pub const WORKSPACE: &str = "Workspace";
/// Key context of viewer/editor windows.
pub const EDITOR: &str = "EditorWindow";
/// Key context of the command line under the panes.
pub const COMMAND_LINE: &str = "CommandLine";

/// The application menu bar. [`crate::keymap::apply`] sets it after the key
/// map, so every item shows the key it actually answers to.
pub fn set_menus(cx: &mut App) {
    cx.set_menus([
        Menu::new("Translocator").items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Quit Translocator", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Tab", NewTab),
            MenuItem::action("New Folder", NewFolder),
            MenuItem::action("New File", NewFile),
            MenuItem::separator(),
            MenuItem::action("View", ViewFile),
            MenuItem::action("Edit", EditFile),
            MenuItem::action("Open Externally", OpenExternally),
            MenuItem::separator(),
            MenuItem::action("Copy to Other Pane…", CopyFiles),
            MenuItem::action("Move to Other Pane…", MoveFiles),
            MenuItem::action("Rename…", RenameFile),
            MenuItem::action("Multi-Rename…", MultiRename),
            MenuItem::action("Move to Trash", TrashFiles),
            MenuItem::action("Delete Permanently…", DeleteFiles),
            MenuItem::separator(),
            MenuItem::action("Properties…", Properties),
            MenuItem::action("Calculate Folder Size", CalcSize),
            MenuItem::action("Copy Path", CopyPath),
            MenuItem::separator(),
            MenuItem::action("Close Tab", CloseTab),
        ]),
        Menu::new("Commands").items([
            MenuItem::action("Compare Directories", CompareDirs),
            MenuItem::action("Synchronize Directories…", SyncDirs),
            MenuItem::action("Multi-Rename…", MultiRename),
            MenuItem::separator(),
            MenuItem::action("Open Archive", OpenArchive),
            MenuItem::action("Compress…", Compress),
            MenuItem::action("Decompress…", Decompress),
            MenuItem::action("Pack into Archive in Other Pane…", PackFiles),
            MenuItem::action("Extract Archive to Other Pane…", ExtractArchive),
            MenuItem::separator(),
            MenuItem::action("Find Files…", FindFiles),
            MenuItem::action("Calculate Folder Size", CalcSize),
        ]),
        Menu::new("Go").items([
            MenuItem::action("Back", GoBack),
            MenuItem::action("Forward", GoForward),
            MenuItem::action("Enclosing Folder", GoParent),
            MenuItem::action("Home", GoHome),
            MenuItem::action("Go to Folder…", GoToPath),
            MenuItem::action("Find Files…", FindFiles),
            MenuItem::action("Locations…", Locations),
            MenuItem::separator(),
            MenuItem::action("Open Terminal Here", OpenTerminal),
            MenuItem::action("Command Line", FocusCommandLine),
        ]),
        Menu::new("View").items([
            MenuItem::action("Show Hidden Files", ToggleHidden),
            MenuItem::action("Refresh", Refresh),
            MenuItem::separator(),
            MenuItem::action("Quick Look", ToggleQuickLook),
            MenuItem::action("Swap Panes", SwapPanes),
            MenuItem::action("Same Folder in Other Pane", MirrorPane),
            MenuItem::separator(),
            MenuItem::action("Jobs", ToggleJobs),
            MenuItem::action("Console", ToggleConsole),
        ]),
        Menu::new("Connection").items([
            MenuItem::action("Save This Folder", TogglePlace),
            MenuItem::separator(),
            MenuItem::action("New Bookmark…", NewBookmark),
            MenuItem::action("New Bookmark for This Folder…", BookmarkConnection),
            MenuItem::action("Manage Bookmarks…", ManageBookmarks),
            MenuItem::separator(),
            MenuItem::action("Toggle sudo", ToggleSudo),
            MenuItem::action("Disconnect", Disconnect),
        ]),
        Menu::new("Selection").items([
            MenuItem::action("Mark / Unmark", ToggleMark),
            MenuItem::action("Mark All", MarkAll),
            MenuItem::action("Unmark All", UnmarkAll),
            MenuItem::action("Invert Marks", InvertMarks),
            MenuItem::action("Mark by Mask…", MarkByMask),
            MenuItem::action("Unmark by Mask…", UnmarkByMask),
        ]),
    ]);
}
