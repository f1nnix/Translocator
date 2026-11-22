import AppKit
import SwiftUI
import TLCore

/// A viewer / editor window (F3, F4), one per `Snapshot.editors`: the GPUI
/// shell's `crates/translocator/src/editor.rs`.
///
/// The store opens it when a new editor id appears; it closes itself when
/// the id leaves the snapshot. Every decision is the core's: ⌘W and the
/// close button send `Close` and the window stays (the core may ask about
/// unsaved changes first, through `close_prompt`, shown as a native alert);
/// ⌘S and the Save button send the text. The text view owns the live text,
/// loaded when `content_version` moves; the first edit after the core last
/// reported the file clean sends `Changed`.
struct EditorWindow: View {
    let id: UInt64
    @Environment(AppStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var handle = CodeHandle()
    @State private var window = WindowGuard()
    @State private var lines = 0
    /// `Changed` went to the core since it last said the file was clean.
    @State private var sentChanged = false
    @State private var prompting = false
    @State private var banner: EditorNotification?
    @State private var bannerTask: Task<Void, Never>?

    private var editor: EditorSnapshot? { store.snapshot.editors.first { $0.id == id } }

    var body: some View {
        let editor = editor
        let palette = SyntaxPalette(Theme.shared.colors)
        VStack(spacing: 0) {
            if let editor {
                header(editor)
                Divider()
                if let notice = editor.notice { noticeBar(notice) }
                CodeView(
                    revision: editor.contentVersion,
                    text: { store.core.editorContent(id: id) },
                    language: editor.language,
                    editable: !editor.readonly,
                    palette: palette,
                    handle: handle,
                    onEdit: edited,
                    onLines: { lines = $0 },
                    takesFocus: true
                )
                Divider()
                footer(editor)
            } else {
                Color.clear
            }
        }
        .frame(minWidth: 420, minHeight: 280)
        .background(Color(nsColor: palette.background))
        .overlay(alignment: .top) { bannerView }
        .navigationTitle(editor?.title ?? "Editor")
        .background(WindowBridge(windowGuard: window, onClose: requestClose, onKey: key))
        .onAppear {
            // A window restored from an earlier run has no editor behind it.
            if editor == nil { dismiss() }
        }
        .onChange(of: editor == nil) { _, gone in
            if gone, !window.close() { dismiss() }
        }
        .onChange(of: editor?.dirty) { _, dirty in
            if dirty != true { sentChanged = false }
        }
        .onChange(of: editor?.closePrompt, initial: true) { _, prompt in
            if let prompt {
                if !prompting { ask(prompt) }
            } else {
                prompting = false
            }
        }
        .onChange(of: editor?.notification?.seq, initial: false) { _, _ in
            if let note = editor?.notification { show(note) }
        }
        .onDisappear {
            bannerTask?.cancel()
            // Closed some other way (the app quits): the core lets it go.
            if store.snapshot.editors.contains(where: { $0.id == id }) {
                store.send(.editor(editor: id, input: .closed))
            }
        }
    }

    // ----- parts -------------------------------------------------------------

    private func header(_ editor: EditorSnapshot) -> some View {
        HStack(spacing: 8) {
            Text(editor.fullPath)
                .font(.system(size: 12, design: .monospaced))
                .lineLimit(1)
                .truncationMode(.middle)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .help(editor.fullPath)
            if editor.elevated { tag("sudo", .red) }
            if editor.readonly { tag("read-only", .secondary) }
            if editor.dirty { tag("modified", .orange) }
            if !editor.readonly {
                saveButton(editor)
            }
        }
        .padding(.horizontal, 10)
        .frame(height: 34)
    }

    /// Save, as the GPUI shell has it: disabled unless the core says there
    /// is something to save (`can_save`), and only then the prominent
    /// button. A disabled `.borderedProminent` button keeps its accent fill
    /// in a key window, so it would still look like the thing to press.
    @ViewBuilder
    private func saveButton(_ editor: EditorSnapshot) -> some View {
        let button = Button(editor.saveLabel, action: save)
            .controlSize(.small)
            .disabled(!editor.canSave)
            .help(store.keys.label("save-file").map { "Save (\($0))" } ?? "Save")
        if editor.canSave {
            button.buttonStyle(.borderedProminent)
        } else {
            button.buttonStyle(.bordered)
        }
    }

    private func tag(_ label: String, _ color: Color) -> some View {
        Text(label)
            .font(.caption.weight(.medium))
            .foregroundStyle(color)
            .padding(.horizontal, 7)
            .padding(.vertical, 1)
            .background(color.opacity(0.14), in: .capsule)
            .accessibilityLabel(label)
    }

    private func noticeBar(_ notice: String) -> some View {
        Label(notice, systemImage: "exclamationmark.triangle.fill")
            .font(.callout)
            .symbolRenderingMode(.multicolor)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(Color.yellow.opacity(0.14))
            .overlay(alignment: .bottom) { Divider() }
    }

    private func footer(_ editor: EditorSnapshot) -> some View {
        HStack(spacing: 12) {
            Text(lines == 1 ? "1 line" : "\(lines) lines")
            Text(editor.language)
            Spacer()
            Text(editor.hint)
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .lineLimit(1)
        .padding(.horizontal, 10)
        .frame(height: 22)
    }

    @ViewBuilder
    private var bannerView: some View {
        if let banner {
            Label(banner.message, systemImage: banner.kind.symbol)
                .font(.callout)
                .foregroundStyle(.primary)
                .padding(.horizontal, 14)
                .padding(.vertical, 7)
                .glassEffect(.regular, in: .capsule)
                .padding(.top, 44)
                .transition(.move(edge: .top).combined(with: .opacity))
                .onTapGesture { self.banner = nil }
                .accessibilityAddTraits(.updatesFrequently)
        }
    }

    // ----- what the user does ------------------------------------------------

    /// The first edit of an editable file makes it modified; only the core
    /// decides that.
    private func edited() {
        guard let editor, !editor.readonly, !editor.dirty, !sentChanged else { return }
        sentChanged = true
        store.send(.editor(editor: id, input: .changed))
    }

    private func save() {
        guard let editor, !editor.readonly else { return }
        store.send(.editor(editor: id, input: .save(text: handle.text)))
    }

    private func requestClose() {
        store.send(.editor(editor: id, input: .close))
    }

    /// ⌘S and ⌘W, as the key table has them.
    private func key(_ event: NSEvent) -> Bool {
        switch store.keys.target(for: event, in: [.editor]) {
        case .editor(key: .save):
            save()
            return true
        case .editor(key: .close):
            requestClose()
            return true
        default:
            return false
        }
    }

    /// The native "Save changes?" alert; the answer goes to the core.
    private func ask(_ prompt: ClosePrompt) {
        prompting = true
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = prompt.message
        alert.informativeText = prompt.detail
        for title in prompt.buttons { alert.addButton(withTitle: title) }
        if alert.buttons.count > 1 {
            alert.buttons[1].keyEquivalent = "d"
            alert.buttons[1].keyEquivalentModifierMask = .command
        }
        let answer: (NSApplication.ModalResponse) -> Void = { [store, handle, id] response in
            let choice: CloseChoice = switch response {
            case .alertFirstButtonReturn: .save(text: handle.text)
            case .alertSecondButtonReturn: .dontSave
            default: .cancel
            }
            store.send(.editor(editor: id, input: .closeAnswer(choice: choice)))
        }
        if let host = window.window {
            alert.beginSheetModal(for: host, completionHandler: answer)
        } else {
            answer(alert.runModal())
        }
    }

    private func show(_ note: EditorNotification) {
        withAnimation(.snappy) { banner = note }
        NSAccessibility.post(element: NSApp as Any, notification: .announcementRequested,
                             userInfo: [.announcement: note.message])
        bannerTask?.cancel()
        let seconds: Double = note.kind == .error ? 6 : 2.5
        bannerTask = Task { @MainActor in
            try? await Task.sleep(for: .seconds(seconds))
            if !Task.isCancelled { withAnimation(.snappy) { banner = nil } }
        }
    }
}

// MARK: - the window

/// Reaches the editor's `NSWindow`: its close button and the other ways to
/// close it ask the core instead (`windowShouldClose`), ⌘S / ⌘W are taken
/// before any menu sees them, and the window is not restored at launch.
struct WindowBridge: NSViewRepresentable {
    let windowGuard: WindowGuard
    let onClose: () -> Void
    let onKey: (NSEvent) -> Bool

    func makeNSView(context: Context) -> BridgeView {
        let view = BridgeView()
        view.onWindow = { [weak windowGuard] window in windowGuard?.attach(window) }
        return view
    }

    func updateNSView(_ view: BridgeView, context: Context) {
        windowGuard.onClose = onClose
        windowGuard.onKey = onKey
        if let window = view.window { windowGuard.attach(window) }
    }
}

final class BridgeView: NSView {
    var onWindow: ((NSWindow) -> Void)?

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if let window { onWindow?(window) }
    }
}

/// Stands in as the window's delegate, passing everything on to SwiftUI's
/// own but the question whether the window may close.
@MainActor
final class WindowGuard: NSObject, NSWindowDelegate {
    private(set) weak var window: NSWindow?
    nonisolated(unsafe) private weak var original: NSWindowDelegate?
    var onClose: () -> Void = {}
    var onKey: (NSEvent) -> Bool = { _ in false }
    private var closing = false
    private var monitor: Any?

    func attach(_ window: NSWindow) {
        if self.window !== window {
            self.window = window
            window.isRestorable = false
            if monitor == nil {
                monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
                    let handled = MainActor.assumeIsolated { self?.key(event) ?? false }
                    return handled ? nil : event
                }
            }
        }
        if window.delegate !== self {
            original = window.delegate
            window.delegate = self
        }
    }

    private func key(_ event: NSEvent) -> Bool {
        guard let window, event.window === window, window.attachedSheet == nil, !closing else { return false }
        return onKey(event)
    }

    /// Closes the window for good (the core let the editor go); false
    /// when there is no window yet.
    @discardableResult
    func close() -> Bool {
        guard let window else { return false }
        closing = true
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
        window.close()
        return true
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        if closing { return original?.windowShouldClose?(sender) ?? true }
        onClose()
        return false
    }

    func windowWillClose(_ notification: Notification) {
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
        original?.windowWillClose?(notification)
    }

    override nonisolated func responds(to selector: Selector!) -> Bool {
        super.responds(to: selector) || (original?.responds(to: selector) ?? false)
    }

    override nonisolated func forwardingTarget(for selector: Selector!) -> Any? {
        if let original, original.responds(to: selector) { return original }
        return super.forwardingTarget(for: selector)
    }
}
