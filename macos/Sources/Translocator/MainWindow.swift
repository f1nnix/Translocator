import AppKit
import SwiftUI

/// The main window: two panes, the console and command line, the jobs
/// strip and the function bar, with the toolbar, dialogs and toasts.
struct MainWindow: View {
    @Environment(AppStore.self) private var store
    @Environment(\.openSettings) private var openSettings
    @Environment(\.openWindow) private var openWindow
    @Environment(\.colorScheme) private var colorScheme
    @State private var jobsShown = false

    var body: some View {
        VStack(spacing: 0) {
            HSplitView {
                side(.left)
                side(.right)
            }
            .frame(maxHeight: .infinity)
            if store.snapshot.consoleShown {
                Divider()
                ConsoleView(text: store.consoleText)
                    .frame(height: 200)
            }
            Divider()
            CommandLineBar()
            JobsStrip(jobsShown: $jobsShown)
            Divider()
            FunctionBar()
        }
        .frame(minWidth: 760, minHeight: 480)
        .navigationTitle(store.snapshot.title)
        .toolbar { toolbar }
        .overlay(alignment: .top) { ToastStack() }
        .dialogSheet(level: 0)
        .onAppear {
            store.openSettingsAction = { openSettings() }
            store.openEditorAction = { id in openWindow(id: "editor", value: id) }
        }
        .onChange(of: store.snapshot.jobsOpen, initial: true) { _, open in
            if jobsShown != open { jobsShown = open }
        }
        .onChange(of: jobsShown) { _, shown in
            // Opened or closed by the popover itself (Escape, a click
            // outside): the core hears of it, so ⌘J opens it again.
            if shown != store.snapshot.jobsOpen {
                store.send(.jobs(input: .sheetShown(shown: shown)))
            }
        }
    }

    /// A side's file list, or the preview when quick look is on and the
    /// keyboard is on the other side.
    ///
    /// The swap happens inside one container that stays the split view's
    /// pane: with the condition at the top (a `Group` is no container),
    /// the split view would be handed a new pane, sized to its minimum,
    /// and the divider would jump.
    private func side(_ side: Side) -> some View {
        ZStack {
            if let preview = store.snapshot.quickLook, preview.side == side {
                QuickLookView(preview: preview)
            } else {
                PaneView(side: side)
            }
        }
        .frame(minWidth: 280, maxWidth: .infinity, maxHeight: .infinity)
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItemGroup(placement: .primaryAction) {
            Button {
                store.command(.toggleJobs)
            } label: {
                Label(store.snapshot.running > 0 ? "\(store.snapshot.running) Running" : "Jobs",
                      systemImage: store.snapshot.running > 0 ? "arrow.triangle.2.circlepath" : "tray")
            }
            .help(tooltip("Jobs", "toggle-jobs"))
            .popover(isPresented: $jobsShown, arrowEdge: .bottom) {
                JobsList()
                    .environment(store)
            }
            Button {
                store.command(.manageBookmarks)
            } label: {
                Label("Bookmarks", systemImage: "bookmark")
            }
            .help(tooltip("Bookmarks", "manage-bookmarks"))
        }
        ToolbarSpacer(.fixed, placement: .primaryAction)
        ToolbarItemGroup(placement: .primaryAction) {
            let dark = colorScheme == .dark
            Button {
                // Picking a side is a decision: it is remembered instead of
                // following the system at the next change.
                store.send(.settings(edit: .appearanceMode(mode: dark ? .light : .dark)))
            } label: {
                Label(dark ? "Switch to Light" : "Switch to Dark", systemImage: dark ? "sun.max" : "moon")
            }
            .help(dark ? "Switch to light" : "Switch to dark")
            Button {
                store.command(.openSettings)
            } label: {
                Label("Settings", systemImage: "gearshape")
            }
            .help(tooltip("Settings", "open-settings"))
        }
    }

    private func tooltip(_ label: String, _ id: String) -> String {
        if let keys = store.keys.label(id) { "\(label) (\(keys))" } else { label }
    }
}

/// Transient notifications at the top of the window.
struct ToastStack: View {
    @Environment(AppStore.self) private var store

    var body: some View {
        VStack(spacing: 6) {
            ForEach(store.toasts) { toast in
                HStack(spacing: 8) {
                    Image(systemName: toast.kind.symbol)
                        .foregroundStyle(color(toast.kind))
                    Text(toast.message)
                        .lineLimit(3)
                        .textSelection(.enabled)
                }
                .font(.callout)
                .padding(.horizontal, 14)
                .padding(.vertical, 8)
                .glassEffect(.regular, in: .capsule)
                .onTapGesture { store.toasts.removeAll { $0.id == toast.id } }
                .transition(.move(edge: .top).combined(with: .opacity))
                .accessibilityElement(children: .combine)
                .accessibilityAddTraits(.updatesFrequently)
            }
        }
        .padding(.top, 10)
        .animation(.snappy, value: store.toasts)
    }

    private func color(_ kind: ToastKind) -> Color {
        switch kind {
        case .info: .accentColor
        case .success: .green
        case .error: .red
        }
    }
}
