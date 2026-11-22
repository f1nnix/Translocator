import AppKit
import SwiftUI
import TLCore

extension DialogSnapshot: Identifiable {}

extension View {
    /// Presents `snapshot.dialogs[level]` as a sheet; that sheet presents
    /// the next level, so a dialog opened over another stacks on it and
    /// the one below is left as it is. A dialog never closes itself: its
    /// buttons send input, and it goes when the core drops it. A sheet
    /// dismissed any other way sends Cancel.
    func dialogSheet(level: Int) -> some View {
        modifier(DialogSheetModifier(level: level))
    }
}

private struct DialogSheetModifier: ViewModifier {
    let level: Int
    @Environment(AppStore.self) private var store

    func body(content: Content) -> some View {
        let dialogs = store.snapshot.dialogs
        let binding = Binding<DialogSnapshot?>(
            get: { level < dialogs.count ? dialogs[level] : nil },
            set: { new in
                let dialogs = store.snapshot.dialogs
                if new == nil, level < dialogs.count {
                    store.dialog(dialogs[level].id, .cancel)
                }
            }
        )
        content.sheet(item: binding) { dialog in
            DialogView(dialog: dialog)
                .environment(store)
                .dialogSheet(level: level + 1)
        }
    }
}

/// One dialog, drawn from its snapshot. The sheet stays while the id does,
/// and redraws as the core edits the dialog.
struct DialogView: View {
    let dialog: DialogSnapshot
    @Environment(AppStore.self) private var store

    var body: some View {
        // The latest state of this dialog (the sheet's item is the one it
        // was presented with).
        let current = store.snapshot.dialogs.first { $0.id == dialog.id } ?? dialog
        Group {
            switch current.kind {
            case .input(let d): InputDialog(id: dialog.id, dialog: d)
            case .confirm(let d): ConfirmDialog(id: dialog.id, dialog: d)
            case .message(let title, let message): MessageDialog(id: dialog.id, title: title, message: message)
            case .secret(let title, let message, let text):
                SecretDialog(id: dialog.id, title: title, message: message, initial: text)
            case .conflict(let d): ConflictDialog(id: dialog.id, dialog: d)
            case .findResults(let title, let count, let version):
                FindResultsDialog(id: dialog.id, title: title, count: count, version: version)
            case .about(let d): AboutDialog(id: dialog.id, dialog: d)
            case .rename(let d): RenameDialog(id: dialog.id, dialog: d)
            case .sync(let d): SyncDialog(id: dialog.id, dialog: d)
            case .properties(let d): PropertiesDialog(id: dialog.id, dialog: d)
            case .bookmarkEdit(let d): BookmarkEditDialog(id: dialog.id, dialog: d)
            case .bookmarks(let d): BookmarksManagerDialog(id: dialog.id, dialog: d)
            }
        }
        .padding(20)
    }
}

/// Title, body, and the Cancel / OK row most dialogs share.
struct DialogFrame<Content: View>: View {
    let title: String
    let width: CGFloat
    /// Lets the dialog grow from `width` up to this to fit what it holds
    /// at its ideal size (a row of buttons); nil keeps it at `width`.
    var maxWidth: CGFloat?
    @ViewBuilder let content: Content

    var body: some View {
        let stack = VStack(alignment: .leading, spacing: 14) {
            Text(title)
                .font(.headline)
                .accessibilityAddTraits(.isHeader)
            content
        }
        if let maxWidth {
            // At the ideal width (a sheet would take the least), kept
            // between the two.
            stack.frame(minWidth: width, maxWidth: maxWidth, alignment: .leading)
                .fixedSize(horizontal: true, vertical: false)
        } else {
            stack.frame(width: width, alignment: .leading)
        }
    }
}

struct DialogButtons: View {
    let ok: String
    var danger = false
    let cancel: () -> Void
    let submit: () -> Void

    var body: some View {
        HStack {
            Spacer()
            Button("Cancel", role: .cancel, action: cancel)
                .keyboardShortcut(.cancelAction)
            Button(ok, role: danger ? .destructive : nil, action: submit)
                .keyboardShortcut(.defaultAction)
                .tint(danger ? .red : nil)
        }
    }
}

struct InputDialog: View {
    let id: UInt64
    let dialog: InputDialogSnapshot
    @Environment(AppStore.self) private var store
    @State private var text: String?

    var body: some View {
        DialogFrame(title: dialog.title, width: 480) {
            if !dialog.message.isEmpty {
                Text(dialog.message).foregroundStyle(.secondary)
            }
            SelectingTextField(
                text: Binding(get: { text ?? dialog.text }, set: { new in
                    text = new
                    store.dialog(id, .text(text: new))
                }),
                select: dialog.select,
                onSubmit: submit
            )
            .accessibilityLabel(dialog.message.isEmpty ? dialog.title : dialog.message)
            DialogButtons(ok: dialog.ok, cancel: { store.dialog(id, .cancel) }, submit: submit)
        }
    }

    /// The field's text first, so the core answers with what is shown.
    private func submit() {
        store.dialog(id, .text(text: text ?? dialog.text))
        store.dialog(id, .submit)
    }
}

struct ConfirmDialog: View {
    let id: UInt64
    let dialog: ConfirmSnapshot
    @Environment(AppStore.self) private var store

    var body: some View {
        DialogFrame(title: dialog.title, width: 440) {
            Text(dialog.message)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            DialogButtons(ok: dialog.ok, danger: dialog.danger,
                          cancel: { store.dialog(id, .cancel) }, submit: { store.dialog(id, .submit) })
        }
        .onAppear { if dialog.fromProvider { NSApp.activate() } }
    }
}

struct MessageDialog: View {
    let id: UInt64
    let title: String
    let message: String
    @Environment(AppStore.self) private var store

    var body: some View {
        DialogFrame(title: title, width: 520) {
            ScrollView {
                Text(message)
                    .font(.system(.callout, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxHeight: 360)
            HStack {
                Spacer()
                Button("OK") { store.dialog(id, .submit) }
                    .keyboardShortcut(.defaultAction)
                Button("") { store.dialog(id, .cancel) }
                    .keyboardShortcut(.cancelAction)
                    .hidden()
                    .frame(width: 0)
            }
        }
    }
}

struct SecretDialog: View {
    let id: UInt64
    let title: String
    let message: String
    let initial: String
    @Environment(AppStore.self) private var store
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        DialogFrame(title: title, width: 420) {
            Text(message)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            SecureField("Password", text: $text)
                .focused($focused)
                .onSubmit(submit)
                .onChange(of: text) { store.dialog(id, .text(text: text)) }
            DialogButtons(ok: "OK", cancel: { store.dialog(id, .cancel) }, submit: submit)
        }
        .onAppear {
            text = initial
            focused = true
            // A provider's question may come while the app is in the background.
            NSApp.activate()
        }
        .interactiveDismissDisabled()
    }

    private func submit() {
        store.dialog(id, .text(text: text))
        store.dialog(id, .submit)
    }
}

struct ConflictDialog: View {
    let id: UInt64
    let dialog: ConflictSnapshot
    @Environment(AppStore.self) private var store

    var body: some View {
        // As wide as the choices need on one row, from the GPUI shell's 600.
        DialogFrame(title: dialog.title, width: 600, maxWidth: 760) {
            Text(dialog.location)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 6) {
                GridRow {
                    Text("Existing").foregroundStyle(.secondary)
                    Text(dialog.existing)
                        .foregroundStyle(dialog.newer == .existing ? .green : .primary)
                }
                GridRow {
                    Text(dialog.incomingLabel).foregroundStyle(.secondary)
                    Text(dialog.incoming)
                        .foregroundStyle(dialog.newer == .incoming ? .green : .primary)
                }
            }
            .accessibilityElement(children: .combine)
            Toggle("Apply to all remaining conflicts", isOn: Binding(
                get: { dialog.applyAll },
                set: { store.dialog(id, .applyAll(on: $0)) }
            ))
            // Six choices for folders, on one row; past the widest the
            // dialog may be, they wrap rather than being cut short, the
            // last (the default) always at the bottom right.
            TrailingFlow(spacing: 8, lineSpacing: 8) {
                ForEach(dialog.choices, id: \.self) { choice in
                    let button = Button(choice.label) { store.dialog(id, .choose(choice: choice.choice)) }
                    if choice.primary {
                        button.keyboardShortcut(.defaultAction)
                    } else if choice.choice == .cancel {
                        button.keyboardShortcut(.cancelAction)
                    } else {
                        button
                    }
                }
            }
        }
        .interactiveDismissDisabled()
        .onAppear { NSApp.activate() }
    }
}

/// Lays its views out at their ideal sizes in rows pushed to the trailing
/// edge, as a dialog's buttons are. What does not fit goes onto rows
/// above, so the last views (the default button) stay together at the
/// bottom right.
struct TrailingFlow: Layout {
    var spacing: CGFloat = 8
    var lineSpacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let rows = rows(subviews, width: proposal.width ?? .infinity)
        let widest = rows.map(\.width).max() ?? 0
        let height = rows.map(\.height).reduce(0, +) + lineSpacing * CGFloat(max(rows.count - 1, 0))
        let width = proposal.width.flatMap { $0.isFinite ? $0 : nil } ?? widest
        return CGSize(width: width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var y = bounds.minY
        for row in rows(subviews, width: bounds.width) {
            var x = bounds.maxX - row.width
            for ix in row.range {
                let size = subviews[ix].sizeThatFits(.unspecified)
                subviews[ix].place(at: CGPoint(x: x, y: y + (row.height - size.height) / 2),
                                   proposal: ProposedViewSize(size))
                x += size.width + spacing
            }
            y += row.height + lineSpacing
        }
    }

    private struct Row {
        var range: Range<Int>
        var width: CGFloat = 0
        var height: CGFloat = 0
    }

    /// Filled from the last view back, then put in reading order.
    private func rows(_ subviews: Subviews, width: CGFloat) -> [Row] {
        var rows: [Row] = []
        var current = Row(range: subviews.count..<subviews.count)
        for ix in subviews.indices.reversed() {
            let size = subviews[ix].sizeThatFits(.unspecified)
            if !current.range.isEmpty, current.width + spacing + size.width > width {
                rows.append(current)
                current = Row(range: (ix + 1)..<(ix + 1))
            }
            current.width += (current.range.isEmpty ? 0 : spacing) + size.width
            current.height = max(current.height, size.height)
            current.range = ix..<current.range.upperBound
        }
        if !current.range.isEmpty { rows.append(current) }
        return rows.reversed()
    }
}

struct FindResultsDialog: View {
    let id: UInt64
    let title: String
    let count: UInt32
    let version: UInt64
    @Environment(AppStore.self) private var store
    @State private var pages = FindPages()

    var body: some View {
        DialogFrame(title: title, width: 720) {
            List(0..<Int(count), id: \.self) { ix in
                if let hit = pages.row(ix, dialog: id, version: version, core: store.core) {
                    Button {
                        store.dialog(id, .pick(index: UInt32(ix)))
                    } label: {
                        Label(hit.rel, systemImage: hit.isDir ? "folder.fill" : "doc")
                            .labelStyle(.titleAndIcon)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            }
            .frame(height: 420)
            HStack {
                Text("\(count) found").foregroundStyle(.secondary)
                Spacer()
                Button("Close") { store.dialog(id, .submit) }
                    .keyboardShortcut(.defaultAction)
                Button("") { store.dialog(id, .cancel) }
                    .keyboardShortcut(.cancelAction)
                    .hidden()
                    .frame(width: 0)
            }
        }
    }
}

/// Search hits fetched by page, dropped when the results change.
@MainActor
final class FindPages {
    private var version: UInt64 = 0
    private var rows: [Int: FoundRow] = [:]

    func row(_ ix: Int, dialog: UInt64, version: UInt64, core: Core) -> FoundRow? {
        if version != self.version {
            self.version = version
            rows.removeAll()
        }
        if let row = rows[ix] { return row }
        let start = (ix / 200) * 200
        for (offset, row) in core.findResults(dialog: dialog, start: UInt32(start), count: 200).enumerated() {
            rows[start + offset] = row
        }
        return rows[ix]
    }
}

struct AboutDialog: View {
    let id: UInt64
    let dialog: AboutSnapshot
    @Environment(AppStore.self) private var store

    var body: some View {
        VStack(spacing: 10) {
            Image(nsImage: NSApp.applicationIconImage)
                .resizable()
                .frame(width: 96, height: 96)
                .accessibilityHidden(true)
            Text(dialog.title).font(.title2.bold())
            Text(dialog.version).foregroundStyle(.secondary)
            Text(dialog.description)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            Text(dialog.license)
                .font(.caption)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
            HStack {
                Button("Open the README") { store.dialog(id, .openReadme) }
                Button("Close") { store.dialog(id, .submit) }
                    .keyboardShortcut(.defaultAction)
                Button("") { store.dialog(id, .cancel) }
                    .keyboardShortcut(.cancelAction)
                    .hidden()
                    .frame(width: 0)
            }
            .padding(.top, 6)
        }
        .frame(width: 380)
    }
}

/// A placeholder for the dialogs of the areas still to be built: the
/// title, and Close (which cancels, so whoever waits on it hears).
struct AreaPlaceholder: View {
    let id: UInt64
    let title: String
    let detail: String
    @Environment(AppStore.self) private var store

    var body: some View {
        DialogFrame(title: title, width: 420) {
            Text(detail).foregroundStyle(.secondary)
            HStack {
                Spacer()
                Button("Close") { store.dialog(id, .cancel) }
                    .keyboardShortcut(.cancelAction)
            }
        }
    }
}

/// A text field that preselects a range when it first appears (the stem of
/// a name being renamed), which SwiftUI's own field cannot be told.
struct SelectingTextField: NSViewRepresentable {
    @Binding var text: String
    let select: Utf16Range?
    let onSubmit: () -> Void

    func makeCoordinator() -> Delegate { Delegate(self) }

    func makeNSView(context: Context) -> NSTextField {
        let field = NSTextField(string: text)
        field.delegate = context.coordinator
        field.bezelStyle = .roundedBezel
        field.lineBreakMode = .byTruncatingTail
        field.usesSingleLineMode = true
        let select = self.select
        DispatchQueue.main.async {
            MainActor.assumeIsolated {
                guard let window = field.window, window.makeFirstResponder(field),
                      let editor = field.currentEditor() else { return }
                let length = (field.stringValue as NSString).length
                if let select {
                    let start = min(Int(select.start), length)
                    editor.selectedRange = NSRange(location: start, length: min(Int(select.length), length - start))
                } else {
                    editor.selectAll(nil)
                }
            }
        }
        return field
    }

    func updateNSView(_ field: NSTextField, context: Context) {
        context.coordinator.parent = self
        if field.stringValue != text { field.stringValue = text }
    }

    @MainActor
    final class Delegate: NSObject, NSTextFieldDelegate {
        var parent: SelectingTextField

        init(_ parent: SelectingTextField) { self.parent = parent }

        func controlTextDidChange(_ note: Notification) {
            guard let field = note.object as? NSTextField else { return }
            parent.text = field.stringValue
        }

        func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
            if selector == #selector(NSResponder.insertNewline(_:)) {
                parent.onSubmit()
                return true
            }
            return false
        }
    }
}
