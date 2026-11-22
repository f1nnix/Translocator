import SwiftUI
import TLCore

/// STUB — the bookmark editor (`DialogKindSnapshot.bookmarkEdit`).
///
/// Must become the GPUI shell's `render_edit` in
/// `crates/translocator/src/bookmarks.rs`: the type picker for new
/// bookmarks, the name, the plugin's fields and toggles (advanced ones
/// behind "Show advanced options"), "Choose…" for path fields through an
/// `NSOpenPanel` answering `BookmarksInput.pathChosen`, the error line,
/// and Save / Save & Open. Inputs go out as `Input.bookmarks(input:)`.
struct BookmarkEditDialog: View {
    let id: UInt64
    let dialog: BookmarkEditSnapshot

    var body: some View {
        AreaPlaceholder(id: id, title: dialog.title, detail: "Editing bookmarks is not available in this build yet.")
    }
}

/// STUB — the bookmark manager (`DialogKindSnapshot.bookmarks`).
///
/// Must become the GPUI shell's `render_manage`: the bookmarks with their
/// summaries, connected state and Connect / Edit / Delete / up / down, and
/// New Bookmark… (`BookmarksInput.move/edit/delete/open/new`).
struct BookmarksManagerDialog: View {
    let id: UInt64
    let dialog: BookmarksManagerSnapshot
    @Environment(AppStore.self) private var store

    var body: some View {
        DialogFrame(title: dialog.title, width: 460) {
            // Until the manager exists, the bookmarks can at least be opened.
            List(store.snapshot.bookmarks, id: \.id) { bookmark in
                HStack {
                    Label(bookmark.name, systemImage: bookmark.local ? "folder" : "server.rack")
                    Spacer()
                    if bookmark.connected { Image(systemName: "checkmark").foregroundStyle(.secondary) }
                    Button(bookmark.connected ? "Open" : "Connect") {
                        store.send(.bookmarks(input: .open(manager: id, id: bookmark.id)))
                    }
                }
            }
            .frame(height: 240)
            HStack {
                Button("New Bookmark…") { store.send(.bookmarks(input: .new(manager: id))) }
                Spacer()
                Button("Close") { store.dialog(id, .cancel) }
                    .keyboardShortcut(.cancelAction)
            }
        }
    }
}
