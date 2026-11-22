import AppKit
import SwiftUI

/// The menu bar: every command of the key table, in the GPUI shell's
/// menus, each with the user's current shortcut. Commands of the file list
/// are enabled only while it has the keyboard, so text fields keep their
/// keys; the Edit menu's Cut/Copy/Paste/Select All act on files there and
/// on text everywhere else.
struct AppCommands: Commands {
    let store: AppStore

    var body: some Commands {
        CommandGroup(replacing: .appInfo) {
            item("About Translocator", .about)
        }
        CommandGroup(replacing: .appSettings) {
            item("Settings…", .openSettings)
        }
        CommandGroup(replacing: .appTermination) {
            item("Quit Translocator", .quit)
        }

        CommandGroup(replacing: .newItem) {
            item("New Tab", .newTab)
            item("New Folder…", .newFolder)
            item("New File…", .newFile)
            Divider()
            item("View", .viewFile)
            item("Edit", .editFile)
            item("Open Externally", .openExternally)
            item("Reveal in Finder", .revealInFinder)
            Divider()
            item("Copy to Other Pane…", .copyFiles)
            item("Move to Other Pane…", .moveFiles)
            item("Rename…", .renameFile)
            item("Multi-Rename…", .multiRename)
            item("Move to Trash", .trashFiles)
            item("Delete Permanently…", .deleteFiles)
            Divider()
            item("Properties…", .properties)
            item("Calculate Folder Size", .calcSize)
            item("Copy Path", .copyPath)
            Divider()
            item("Close Tab", .closeTab)
        }
        CommandGroup(replacing: .saveItem) {}
        CommandGroup(replacing: .printItem) {}

        CommandGroup(replacing: .pasteboard) {
            edit("Cut", "x", .clipboardCut, #selector(NSText.cut(_:)))
            edit("Copy", "c", .clipboardCopy, #selector(NSText.copy(_:)))
            edit("Paste", "v", .clipboardPaste, #selector(NSText.paste(_:)))
            edit("Select All", "a", .markAll, #selector(NSText.selectAll(_:)))
        }

        CommandGroup(before: .toolbar) {
            toggle("Show Hidden Files", .toggleHidden, on: store.snapshot.settings.general.showHidden)
            item("Refresh", .refresh)
            Divider()
            toggle("Quick Look", .toggleQuickLook, on: store.snapshot.quickLook != nil)
            item("Swap Panes", .swapPanes)
            item("Same Folder in Other Pane", .mirrorPane)
            item("Other Pane", .switchPane)
            Divider()
            item("Next Tab", .nextTab)
            item("Previous Tab", .prevTab)
            Divider()
            toggle("Jobs", .toggleJobs, on: store.snapshot.jobsOpen)
            toggle("Console", .toggleConsole, on: store.snapshot.consoleShown)
            Divider()
        }

        CommandMenu("Commands") {
            item("Compare Directories", .compareDirs)
            item("Synchronize Directories…", .syncDirs)
            item("Multi-Rename…", .multiRename)
            Divider()
            item("Open Archive", .openArchive)
            item("Compress…", .compress)
            item("Decompress…", .decompress)
            item("Pack into Archive in Other Pane…", .packFiles)
            item("Extract Archive to Other Pane…", .extractArchive)
            Divider()
            item("Find Files…", .findFiles)
            item("Calculate Folder Size", .calcSize)
        }

        CommandMenu("Go") {
            item("Back", .goBack)
            item("Forward", .goForward)
            item("Enclosing Folder", .goParent)
            item("Open", .open)
            item("Home", .goHome)
            item("Go to Folder…", .goToPath)
            item("Find Files…", .findFiles)
            item("Locations…", .locations(side: nil))
            item("Locations for the Left Pane…", .locations(side: .left))
            item("Locations for the Right Pane…", .locations(side: .right))
            Divider()
            Menu("Saved Folders") {
                ForEach(0..<9, id: \.self) { ix in
                    let places = store.pane(store.snapshot.active).places
                    let name = ix < places.count ? places[ix].name : "Saved Folder \(ix + 1)"
                    item(name, .goToPlace(index: UInt32(ix)))
                        .disabled(ix >= places.count || !store.enabled(.pane))
                }
            }
            Divider()
            item("Open Terminal Here", .openTerminal)
            item("Command Line", .focusCommandLine)
            item("Back to the File List", .focusPane)
        }

        CommandMenu("Connection") {
            item(store.pane(store.snapshot.active).placeSaved ? "Remove This Folder" : "Save This Folder", .togglePlace)
            Divider()
            item("New Bookmark…", .newBookmark)
            item("New Bookmark for This Folder…", .bookmarkConnection)
            item("Manage Bookmarks…", .manageBookmarks)
            Divider()
            toggle("Work as Root (sudo)", .toggleSudo, on: store.pane(store.snapshot.active).elevated)
            item("Disconnect", .disconnect)
        }

        CommandMenu("Selection") {
            item("Mark / Unmark", .toggleMark)
            item("Mark and Move Up", .markUp)
            item("Mark and Move Down", .markDown)
            item("Mark All", .markAll)
            item("Unmark All", .unmarkAll)
            item("Invert Marks", .invertMarks)
            item("Mark by Mask…", .markByMask)
            item("Unmark by Mask…", .unmarkByMask)
        }

        CommandGroup(replacing: .help) {
            item("Translocator Help", .openHelp)
        }
    }

    private func item(_ title: String, _ command: Command) -> some View {
        Button(title) { store.command(command) }
            .keyboardShortcut(store.keys.menuShortcut(for: command))
            .disabled(!store.enabled(store.keys.context(of: command)))
    }

    private func toggle(_ title: String, _ command: Command, on: Bool) -> some View {
        Toggle(title, isOn: Binding(get: { on }, set: { _ in store.command(command) }))
            .keyboardShortcut(store.keys.menuShortcut(for: command))
            .disabled(!store.enabled(store.keys.context(of: command)))
    }

    private func edit(_ title: String, _ key: KeyEquivalent, _ command: Command, _ selector: Selector) -> some View {
        Button(title) { store.editAction(command, selector: selector) }
            .keyboardShortcut(key, modifiers: .command)
    }
}
