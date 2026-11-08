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
        // What the desktop expects of an application, rather than of a
        // file manager. See `system` below.
        About,
        OpenHelp,
        Hide,
        HideOthers,
        ShowAll,
        Minimize,
        Zoom,
        ToggleFullScreen,
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

/// Where the Help menu sends people: the README is the documentation.
pub const HELP_URL: &str = env!("CARGO_PKG_REPOSITORY");

/// The commands a desktop expects of any application, wired to the
/// platform. The rest of the application's actions are handled by the
/// window that can carry them out; these belong to no window, or to
/// whichever one is in front.
pub fn handle_system_actions(cx: &mut App) {
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
    cx.on_action(|_: &OpenSettings, cx: &mut App| crate::settings_window::open(cx));
    cx.on_action(|_: &OpenHelp, cx: &mut App| cx.open_url(HELP_URL));
    cx.on_action(|_: &Hide, cx: &mut App| cx.hide());
    cx.on_action(|_: &HideOthers, cx: &mut App| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx: &mut App| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx: &mut App| in_front(cx, |window| window.minimize_window()));
    cx.on_action(|_: &Zoom, cx: &mut App| in_front(cx, |window| window.zoom_window()));
    cx.on_action(|_: &ToggleFullScreen, cx: &mut App| {
        in_front(cx, |window| window.toggle_fullscreen())
    });
    cx.on_action(|_: &About, cx: &mut App| {
        if let Some(handle) = cx.active_window() {
            let _ = handle.update(cx, |_, window, cx| crate::dialogs::about(window, cx));
        }
    });
}

/// Runs `act` on the window in front, if there is one. A command from the
/// menu bar arrives with no window of its own.
fn in_front(cx: &mut App, act: impl FnOnce(&mut Window)) {
    if let Some(handle) = cx.active_window() {
        let _ = handle.update(cx, |_, window, _| act(window));
    }
}

/// The keys macOS puts these on. They are deliberately not in
/// [`crate::keymap`]'s table: ⌘H and ⌘M belong to the system rather than to
/// this application, and an application that let you rebind them would be
/// the odd one out. Bound before the key map, so a key the user does assign
/// to something else still wins.
#[cfg(target_os = "macos")]
pub fn bind_system_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
    ]);
}

#[cfg(not(target_os = "macos"))]
pub fn bind_system_keys(_cx: &mut App) {}

/// The application menu bar. [`crate::keymap::apply`] sets it after the key
/// map, so every item shows the key it actually answers to.
pub fn set_menus(cx: &mut App) {
    // The first menu is the application menu, and macOS expects a
    // particular set of commands in it. Elsewhere there is no such
    // convention, and no system to hide the application behind.
    let mut app_menu = vec![
        MenuItem::action("About Translocator", About),
        MenuItem::separator(),
        MenuItem::action("Settings…", OpenSettings),
    ];
    if cfg!(target_os = "macos") {
        app_menu.extend([
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Translocator", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
        ]);
    }
    app_menu.extend([
        MenuItem::separator(),
        MenuItem::action("Quit Translocator", Quit),
    ]);

    cx.set_menus([
        Menu::new("Translocator").items(app_menu),
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
            MenuItem::separator(),
            MenuItem::action("Enter Full Screen", ToggleFullScreen),
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
        // Named "Window" so macOS takes it over: it adds the list of open
        // windows, and the marks that say which one is in front.
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
        ]),
        // Named "Help" for the same reason: macOS puts its search field at
        // the top of the menu it finds under that name.
        Menu::new("Help").items([MenuItem::action("Translocator Help", OpenHelp)]),
    ]);
}
