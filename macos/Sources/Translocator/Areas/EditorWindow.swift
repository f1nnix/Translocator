import SwiftUI
import TLCore

/// STUB — a viewer / editor window (F3, F4), one per `Snapshot.editors`.
///
/// The store opens this window when a new editor id appears. Must become
/// the GPUI shell's `crates/translocator/src/editor.rs`: the header (full
/// path, sudo / read-only / modified tags), the notice bar, a text view
/// (an `NSTextView` with line numbers, read-only for the viewer) loaded
/// when `content_version` moves, Save (⌘S, `EditorInput.save(text:)`),
/// ⌘W / the close button as `EditorInput.close` with the core's close
/// prompt as an alert, and the window's own notifications. The window
/// closes itself when its id leaves `Snapshot.editors`.
struct EditorWindow: View {
    let id: UInt64
    @Environment(AppStore.self) private var store
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let editor = store.snapshot.editors.first { $0.id == id }
        VStack(spacing: 12) {
            Image(systemName: "doc.text")
                .font(.largeTitle)
                .foregroundStyle(.secondary)
            Text(editor?.title ?? "Editor").font(.headline)
            Text("The viewer and editor are not available in this build yet.")
                .foregroundStyle(.secondary)
            Button("Close") { store.send(.editor(editor: id, input: .closeNow)) }
                .keyboardShortcut(.cancelAction)
        }
        .padding(40)
        .frame(minWidth: 480, minHeight: 300)
        .navigationTitle(editor?.title ?? "Editor")
        .onChange(of: editor == nil) { _, gone in if gone { dismiss() } }
        .onDisappear {
            // Closed by the window's own button: the core lets it go.
            if store.snapshot.editors.contains(where: { $0.id == id }) {
                store.send(.editor(editor: id, input: .closed))
            }
        }
    }
}
