import SwiftUI
import TLCore

/// STUB — quick look (⌘Y), drawn in place of the pane opposite the active
/// one while `Snapshot.quick_look` is set.
///
/// Must become the GPUI shell's `crates/translocator/src/quick_look.rs`:
/// a header with the entry's icon and name, the body (nothing, reading,
/// a note, an image decoded from bytes, read-only text with line numbers,
/// an error) and the footer (kind, date, owner, mode, how much is shown).
/// The body comes from `Core` keyed by `revision` (see
/// `crates/tl-ffi/src/quick_look.rs`).
struct QuickLookView: View {
    let preview: QuickLookSnapshot

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: "eye")
                .font(.largeTitle)
                .foregroundStyle(.secondary)
            Text(preview.name.isEmpty ? "Quick Look" : preview.name)
                .font(.headline)
                .lineLimit(1)
                .truncationMode(.middle)
            Text("Previews are not available in this build yet.")
                .foregroundStyle(.secondary)
        }
        .padding()
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color(nsColor: .textBackgroundColor))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Quick Look \(preview.name)")
    }
}
