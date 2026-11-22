import AppKit
import SwiftUI
import TLCore

/// An AppKit menu item that runs a closure.
@MainActor
final class ActionMenuItem: NSMenuItem {
    private let handler: @MainActor () -> Void

    init(_ title: String, symbol: String? = nil, enabled: Bool = true, handler: @escaping @MainActor () -> Void) {
        self.handler = handler
        super.init(title: title, action: enabled ? #selector(fire) : nil, keyEquivalent: "")
        target = self
        isEnabled = enabled
        if let symbol { image = symbolImage(symbol) }
    }

    required init(coder: NSCoder) { fatalError() }

    @objc private func fire() { handler() }
}

/// The location menu: this host's saved folders, this Mac, bookmarks, and
/// the connection commands. Built from the snapshot when it opens, from the
/// header's button or the keyboard (⌘K, ⌥F1, ⌥F2).
enum LocationMenu {
    @MainActor
    static func build(store: AppStore, side: Side) -> NSMenu {
        let snapshot = store.snapshot
        let pane = store.pane(side)
        let keys = store.keys
        let inArchive = pane.kind == .archive
        let menu = NSMenu(title: "Locations")
        menu.autoenablesItems = false

        if !pane.places.isEmpty {
            menu.addItem(.sectionHeader(title: "Places"))
            for (ix, place) in pane.places.enumerated() {
                let item = ActionMenuItem(place.name, symbol: "folder") {
                    store.command(.goToPlace(index: UInt32(ix)), on: side)
                }
                KeyMap.apply(keys.binding(for: .goToPlace(index: UInt32(ix))), to: item)
                item.state = place.current ? .on : .off
                menu.addItem(item)
            }
        }
        let toggle = ActionMenuItem(pane.placeSaved ? "Remove This Folder" : "Save This Folder", enabled: !inArchive) {
            store.command(.togglePlace, on: side)
        }
        KeyMap.apply(keys.binding(for: .togglePlace), to: toggle)
        menu.addItem(toggle)

        menu.addItem(.separator())
        menu.addItem(.sectionHeader(title: "This Mac"))
        for folder in snapshot.localFolders {
            menu.addItem(ActionMenuItem(folder.label, symbol: folder.icon.symbol) {
                store.pane(side, .openLocal(path: folder.path))
            })
        }

        menu.addItem(.separator())
        menu.addItem(.sectionHeader(title: "Bookmarks"))
        if snapshot.bookmarks.isEmpty {
            menu.addItem(ActionMenuItem("No bookmarks yet", enabled: false) {})
        }
        for bookmark in snapshot.bookmarks {
            let item = ActionMenuItem(bookmark.name, symbol: bookmark.local ? "folder.badge.person.crop" : "server.rack") {
                store.pane(side, .openBookmark(id: bookmark.id))
            }
            item.state = bookmark.connected ? .on : .off
            item.toolTip = bookmark.summary
            menu.addItem(item)
        }

        menu.addItem(.separator())
        menu.addItem(ActionMenuItem("New Bookmark…", symbol: "bookmark") { store.command(.newBookmark, on: side) })
        let forFolder = ActionMenuItem("New Bookmark for This Folder…", enabled: !inArchive) {
            store.command(.bookmarkConnection, on: side)
        }
        KeyMap.apply(keys.binding(for: .bookmarkConnection), to: forFolder)
        menu.addItem(forFolder)
        let manage = ActionMenuItem("Manage Bookmarks…", symbol: "books.vertical") {
            store.command(.manageBookmarks, on: side)
        }
        KeyMap.apply(keys.binding(for: .manageBookmarks), to: manage)
        menu.addItem(manage)
        if pane.kind != .local {
            menu.addItem(.separator())
            menu.addItem(ActionMenuItem("Disconnect", symbol: "powerplug") { store.pane(side, .disconnect) })
        }
        return menu
    }
}

/// The file list's context menu, the same items as the GPUI shell's.
enum ContextMenu {
    @MainActor
    static func build(store: AppStore, side: Side, pane: PaneSnapshot) -> NSMenu {
        let menu = NSMenu(title: "Files")
        menu.autoenablesItems = false
        let count = pane.selectionCount
        let one = count == 1
        func add(_ title: String, _ command: Command, enabled: Bool = true) {
            let item = ActionMenuItem(title, enabled: enabled) { store.command(command, on: side) }
            KeyMap.apply(store.keys.binding(for: command), to: item)
            menu.addItem(item)
        }
        add("Open", .open)
        add("View", .viewFile, enabled: one)
        add("Edit", .editFile, enabled: one)
        add("Open Externally", .openExternally)
        menu.addItem(.separator())
        add("Copy to Other Pane…", .copyFiles, enabled: count > 0)
        add("Move to Other Pane…", .moveFiles, enabled: count > 0)
        add("Rename…", .renameFile, enabled: one)
        add("Multi-Rename…", .multiRename, enabled: count > 0)
        menu.addItem(.separator())
        if pane.cursorOnArchive {
            add("Open Archive", .openArchive)
            add("Decompress…", .decompress)
            add("Extract to Other Pane…", .extractArchive)
        }
        add("Compress…", .compress, enabled: count > 0)
        add("Pack into Archive in Other Pane…", .packFiles, enabled: count > 0)
        menu.addItem(.separator())
        add("Copy", .clipboardCopy)
        add("Cut", .clipboardCut)
        add("Paste", .clipboardPaste)
        add("Copy Path", .copyPath)
        menu.addItem(.separator())
        add("New Folder…", .newFolder)
        add("New File…", .newFile)
        add("Calculate Size", .calcSize)
        add("Properties…", .properties)
        menu.addItem(.separator())
        if pane.hostLocal { add("Reveal in Finder", .revealInFinder) }
        add("Open Terminal Here", .openTerminal)
        menu.addItem(.separator())
        if pane.kind == .local { add("Move to Trash", .trashFiles, enabled: count > 0) }
        add("Delete Permanently…", .deleteFiles, enabled: count > 0)
        return menu
    }
}

/// The header's location button: pops the location menu under itself.
struct LocationButton: NSViewRepresentable {
    let side: Side
    let label: String
    let symbol: String
    @Environment(AppStore.self) private var store

    func makeNSView(context: Context) -> LocationNSButton {
        let button = LocationNSButton()
        button.bezelStyle = .accessoryBarAction
        button.isBordered = true
        button.imagePosition = .imageLeading
        button.setButtonType(.momentaryPushIn)
        button.controlSize = .small
        button.refusesFirstResponder = true
        return button
    }

    func updateNSView(_ button: LocationNSButton, context: Context) {
        button.title = label + " ▾"
        button.image = symbolImage(symbol, label: label)
        button.side = side
        button.store = store
        button.setAccessibilityLabel("Location: \(label)")
        button.setAccessibilityHelp("Opens the locations menu")
    }
}

final class LocationNSButton: NSButton {
    var side: Side = .left
    weak var store: AppStore?

    override func mouseDown(with event: NSEvent) {
        show()
    }

    override func accessibilityPerformPress() -> Bool {
        show()
        return true
    }

    private func show() {
        guard let store else { return }
        store.pane(side, .locationsShown)
        let menu = LocationMenu.build(store: store, side: side)
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: bounds.height + 4), in: self)
    }
}

/// The folder as a native path bar; a click on a component goes there.
struct PathBar: NSViewRepresentable {
    let side: Side
    let crumbs: [Crumb]
    let elevated: Bool
    @Environment(AppStore.self) private var store

    func makeCoordinator() -> Target { Target() }

    func makeNSView(context: Context) -> NSPathControl {
        let control = NSPathControl()
        control.pathStyle = .standard
        control.controlSize = .small
        control.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        control.backgroundColor = .clear
        control.focusRingType = .none
        control.refusesFirstResponder = true
        control.target = context.coordinator
        control.action = #selector(Target.clicked(_:))
        control.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return control
    }

    func updateNSView(_ control: NSPathControl, context: Context) {
        let coordinator = context.coordinator
        coordinator.store = store
        coordinator.side = side
        guard coordinator.crumbs != crumbs || coordinator.elevated != elevated else { return }
        coordinator.crumbs = crumbs
        coordinator.elevated = elevated
        var items: [NSPathControlItem] = []
        let root = NSPathControlItem()
        root.title = "/"
        items.append(root)
        for (ix, crumb) in crumbs.enumerated() {
            let item = NSPathControlItem()
            if ix == crumbs.count - 1 {
                item.attributedTitle = NSAttributedString(string: crumb.name, attributes: [
                    .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize, weight: .semibold),
                    .foregroundColor: elevated ? NSColor.systemRed : NSColor.labelColor,
                ])
            } else {
                item.title = crumb.name
            }
            items.append(item)
        }
        control.pathItems = items
        control.setAccessibilityLabel("Path")
    }

    @MainActor
    final class Target: NSObject {
        weak var store: AppStore?
        var side: Side = .left
        var crumbs: [Crumb] = []
        var elevated = false

        @objc func clicked(_ sender: NSPathControl) {
            guard let store, let clicked = sender.clickedPathItem,
                  let ix = sender.pathItems.firstIndex(of: clicked) else { return }
            let path = ix == 0 ? Data("/".utf8) : crumbs[ix - 1].path
            store.pane(side, .navigate(path: path))
        }
    }
}
