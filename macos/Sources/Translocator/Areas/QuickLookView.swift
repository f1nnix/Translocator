import AppKit
import ImageIO
import SwiftUI

/// Quick look (⌘Y), drawn in place of the pane opposite the active one
/// while `Snapshot.quick_look` is set: the GPUI shell's
/// `crates/translocator/src/quick_look.rs`.
///
/// A header with the entry's icon and name, the body (a centred message,
/// an image scaled to fit, or read-only coloured text with line numbers)
/// and the footer (kind, date, owner, mode; how much is shown). The core
/// follows the cursor and waits for it to settle before reading; the body
/// is fetched from `Core` only when `revision` moves, and images are
/// decoded off the main thread, so walking the list stays quick.
struct QuickLookView: View {
    let preview: QuickLookSnapshot
    @Environment(AppStore.self) private var store
    /// The decoded image of a revision (nil inside: it could not be read).
    @State private var image: (revision: UInt64, image: NSImage?)?

    var body: some View {
        let colors = Theme.shared.colors
        VStack(spacing: 0) {
            header
                .background(Color(nsColor: colors.chrome))
            Divider()
            content(SyntaxPalette(colors))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            Divider()
            footer
                .background(Color(nsColor: colors.chrome))
        }
        .background(Color(nsColor: colors.background))
        .task(id: imageKey) { await decode() }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Image(systemName: preview.icon?.symbol ?? "eye")
                .foregroundStyle(.secondary)
                .frame(width: 16)
            Text(preview.name)
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .leading)
            Text(store.keys.label("quick-look").map { "Quick Look · \($0)" } ?? "Quick Look")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize()
        }
        .font(.callout)
        .padding(.horizontal, 10)
        .frame(height: 32)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(preview.name.isEmpty ? "Quick Look" : "Quick Look: \(preview.name)")
    }

    @ViewBuilder
    private func content(_ palette: SyntaxPalette) -> some View {
        switch preview.body {
        case .nothing: message("Nothing to preview")
        case .reading: message("Reading…")
        case .note(let note): message(note)
        case .error(let error): message(error, error: true)
        case .text(let language):
            CodeView(
                revision: preview.revision,
                text: { store.core.quickLookText() },
                language: language,
                editable: false,
                palette: palette
            )
        case .image:
            if let image, image.revision == preview.revision {
                if let picture = image.image {
                    Image(nsImage: picture)
                        .resizable()
                        .interpolation(.high)
                        .scaledToFit()
                        // Scaled down to fit, never up.
                        .frame(maxWidth: picture.size.width, maxHeight: picture.size.height)
                        .padding(8)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .accessibilityLabel("Image \(preview.name)")
                } else {
                    message("This image cannot be shown")
                }
            } else {
                Color.clear
            }
        }
    }

    private func message(_ text: String, error: Bool = false) -> some View {
        Text(text)
            .font(.callout)
            .foregroundStyle(error ? Color.red : Color.secondary)
            .multilineTextAlignment(.center)
            .textSelection(.enabled)
            .padding()
    }

    private var footer: some View {
        HStack(spacing: 8) {
            Text(preview.footer)
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: .infinity, alignment: .leading)
            if let shown = preview.shown {
                Text(shown).fixedSize()
            }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 10)
        .frame(height: 22)
    }

    // ----- images ------------------------------------------------------------

    /// The revision to decode, when the body is an image.
    private var imageKey: UInt64? {
        if case .image = preview.body { preview.revision } else { nil }
    }

    private func decode() async {
        guard let revision = imageKey, image?.revision != revision else { return }
        let data = store.core.quickLookImage()
        let decoded = await Task.detached(priority: .userInitiated) { Decoded(data) }.value
        if Task.isCancelled { return }
        image = (revision, decoded.image)
    }
}

/// An image decoded off the main thread: ImageIO's thumbnail (at most
/// 2048 pixels on a side, the right way up), or `NSImage` for what ImageIO
/// does not read (SVG).
private struct Decoded: @unchecked Sendable {
    let image: NSImage?

    init(_ data: Data) {
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: 2048,
            kCGImageSourceShouldCacheImmediately: true,
        ]
        if let source = CGImageSourceCreateWithData(data as CFData, nil),
           CGImageSourceGetCount(source) > 0,
           let cg = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) {
            image = NSImage(cgImage: cg, size: NSSize(width: cg.width, height: cg.height))
        } else {
            image = NSImage(data: data)
        }
    }
}
