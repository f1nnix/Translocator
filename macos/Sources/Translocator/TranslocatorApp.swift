import AppKit
import SwiftUI
import TLCore

struct TranslocatorApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    private let store = AppStore.shared

    var body: some Scene {
        Window("Translocator", id: "main") {
            MainWindow()
                .environment(store)
        }
        .defaultSize(width: 1280, height: 820)
        .windowResizability(.contentMinSize)
        // Opens at launch even when the system has saved state for this
        // bundle id with no window of this scene in it.
        .defaultLaunchBehavior(.presented)
        .commands { AppCommands(store: store) }

        // STUB windows: see Areas/EditorWindow.swift and Areas/SettingsWindow.swift.
        WindowGroup("Editor", id: "editor", for: UInt64.self) { $id in
            if let id {
                EditorWindow(id: id).environment(store)
            }
        }
        .defaultSize(width: 960, height: 720)

        // The scene's own "Settings…" item would open the window directly,
        // with a fixed ⌘,: the app menu has the core's (AppCommands), with
        // the user's key, and the core asks for the window.
        Settings {
            SettingsWindow().environment(store)
        }
        .commandsRemoved()
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    /// Lets the core write the settings and bookmarks and close its
    /// connections before the process goes.
    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated { AppStore.shared.shutdown() }
    }
}
