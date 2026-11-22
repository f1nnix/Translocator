import AppKit
import Observation
import SwiftUI

/// A transient banner in the main window.
struct Toast: Identifiable, Equatable {
    let id = UUID()
    let kind: ToastKind
    let message: String
}

/// The app's one piece of state: the running core and its latest snapshot.
///
/// Every decision is the core's. The store sends inputs, takes in snapshots
/// when the core says something changed, and carries out the one-shot
/// requests a snapshot holds (where the keyboard goes, the locations menu,
/// the settings window). The panes are kept as separate properties so that
/// a job's progress does not invalidate the file lists.
@MainActor
@Observable
final class AppStore {
    static let shared = AppStore()

    @ObservationIgnored let core: Core
    @ObservationIgnored private let bridge = HostBridge()

    /// The latest snapshot (the panes are also in `left` and `right`).
    private(set) var snapshot: Snapshot
    private(set) var left: PaneSnapshot
    private(set) var right: PaneSnapshot
    /// The key table with the user's keys, and lookups built from it.
    private(set) var keys: KeyMap
    private(set) var consoleText = ""
    var toasts: [Toast] = []
    /// The pane whose list has the keyboard, if any.
    var focusedPane: Side?
    /// Bumped to put the keyboard in the command line.
    private(set) var commandLineFocus = 0

    /// The file lists, for focus and the keyboard locations menu.
    @ObservationIgnored private var tables: [Side: WeakTable] = [:]
    /// Set by the main window: SwiftUI's own actions, which only a view gets.
    @ObservationIgnored var openSettingsAction: (() -> Void)?
    @ObservationIgnored var openEditorAction: ((UInt64) -> Void)?
    @ObservationIgnored private var seen = Seen()
    @ObservationIgnored private var openEditors: Set<UInt64> = []

    private struct Seen {
        var focus: UInt64 = 0
        var locations: [Side: UInt64] = [:]
        var settings: UInt64 = 0
        var console: UInt64 = 0
        var appearance: AppearanceMode?
    }

    private init() {
        let core = Core.start(host: bridge)
        self.core = core
        let first = core.snapshot()
        snapshot = first
        left = first.left
        right = first.right
        keys = KeyMap(core.keyTable())
        seen.settings = first.settingsRequests
        bridge.store = self
        apply(first, initial: true)
    }

    func pane(_ side: Side) -> PaneSnapshot {
        side == .left ? left : right
    }

    // ----- input ----------------------------------------------------------

    func send(_ input: Input) {
        core.send(input: input)
    }

    /// A command acting on the active pane (or the window).
    func command(_ command: Command) {
        send(.command(command: command))
    }

    /// A command for `side`: the core acts on the active pane, so the side
    /// is made active first. A pane's menu can be used while the keyboard
    /// is in the other one.
    func command(_ command: Command, on side: Side) {
        if snapshot.active != side {
            send(.focus(side: side))
        }
        send(.command(command: command))
    }

    func pane(_ side: Side, _ input: PaneInput) {
        send(.pane(side: side, input: input))
    }

    func dialog(_ id: UInt64, _ input: DialogInput) {
        send(.dialog(id: id, input: input))
    }

    // ----- what the core sends ------------------------------------------

    func refresh() {
        let next = core.snapshot()
        guard next != snapshot else { return }
        apply(next, initial: false)
    }

    private func apply(_ next: Snapshot, initial: Bool) {
        let old = snapshot
        if initial || next.settings.keysVersion != old.settings.keysVersion {
            keys = KeyMap(core.keyTable())
        }
        if initial || next.consoleVersion != seen.console {
            seen.console = next.consoleVersion
            consoleText = core.consoleText()
        }
        snapshot = next
        if next.left != left { left = next.left }
        if next.right != right { right = next.right }

        let mode = next.settings.appearance.mode
        if seen.appearance != mode {
            seen.appearance = mode
            switch mode {
            case .system: NSApp?.appearance = nil
            case .light: NSApp?.appearance = NSAppearance(named: .aqua)
            case .dark: NSApp?.appearance = NSAppearance(named: .darkAqua)
            }
        }

        if let request = next.focusRequest, request.serial != seen.focus {
            seen.focus = request.serial
            switch request.target {
            case .pane(let side): tables[side]?.table?.takeFocus()
            case .commandLine: commandLineFocus += 1
            }
        }
        if let request = next.locationsRequest, request.serial != seen.locations[request.side] {
            seen.locations[request.side] = request.serial
            // After the layout pass, so the menu is built from this snapshot.
            let side = request.side
            DispatchQueue.main.async {
                MainActor.assumeIsolated { self.tables[side]?.table?.popUpLocations() }
            }
        }
        if next.settingsRequests != seen.settings {
            seen.settings = next.settingsRequests
            openSettingsAction?()
        }
        let editors = Set(next.editors.map(\.id))
        for id in editors.subtracting(openEditors) {
            openEditorAction?(id)
        }
        openEditors = editors
    }

    func perform(_ event: HostEvent) {
        switch event {
        case .toast(let kind, let message):
            let toast = Toast(kind: kind, message: message)
            toasts.append(toast)
            NSAccessibility.post(element: NSApp as Any, notification: .announcementRequested,
                                 userInfo: [.announcement: message, .priority: NSAccessibilityPriorityLevel.high.rawValue])
            if toasts.count > 4 { toasts.removeFirst(toasts.count - 4) }
            let seconds: Double = kind == .error ? 6 : 3.5
            DispatchQueue.main.asyncAfter(deadline: .now() + seconds) {
                MainActor.assumeIsolated { self.toasts.removeAll { $0.id == toast.id } }
            }
        case .clipboard(let text):
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(text, forType: .string)
        case .open(let path):
            NSWorkspace.shared.open(URL(fileURLWithPath: path))
        case .reveal(let path):
            NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
        case .openURL(let url):
            if let url = URL(string: url) { NSWorkspace.shared.open(url) }
        case .quit:
            NSApp.terminate(nil)
        }
    }

    // ----- the file lists ---------------------------------------------------

    func register(_ table: FileTableView, for side: Side) {
        tables[side] = WeakTable(table: table)
    }

    func table(_ side: Side) -> FileTableView? {
        tables[side]?.table
    }

    /// The pane list with the keyboard changed (nil: something else has it).
    func listFocusChanged(_ side: Side, focused: Bool) {
        if focused {
            focusedPane = side
            if snapshot.active != side { send(.focus(side: side)) }
        } else if focusedPane == side {
            focusedPane = nil
        }
    }

    /// Whether menu commands of a context can fire now: those of the file
    /// list only while it has the keyboard (so a text field keeps its
    /// keys), those of the window while no dialog is up.
    func enabled(_ context: KeyContext) -> Bool {
        switch context {
        case .pane: return focusedPane != nil && snapshot.dialogs.isEmpty
        case .workspace, .commandLine: return snapshot.dialogs.isEmpty
        case .editor, .global: return true
        }
    }

    /// The Edit menu's Cut, Copy, Paste and Select All: files in the list,
    /// text everywhere else.
    func editAction(_ command: Command, selector: Selector) {
        if focusedPane != nil && snapshot.dialogs.isEmpty {
            self.command(command)
        } else {
            NSApp.sendAction(selector, to: nil, from: nil)
        }
    }

    func focusCommandLine() {
        commandLineFocus += 1
    }

    func shutdown() {
        core.shutdown()
    }
}

private struct WeakTable {
    weak var table: FileTableView?
}
