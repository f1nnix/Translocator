import SwiftUI

/// The multi-rename dialog (`DialogKindSnapshot.rename`), after the GPUI
/// shell's `crates/translocator/src/rename.rs`.
///
/// The masks with their placeholder buttons, find / replace with Regex and
/// Match case, the counter, the case picker, and the preview ("old → new",
/// problems in red), fetched by range from `Core.renamePreview` and dropped
/// when `previewVersion` moves. Every edit is `Input.rename(dialog:input:)`;
/// the counts, notes and whether Rename can run come back in the snapshot.
/// Rename (or Return) is `RenameInput.apply`: with problems the dialog stays.
struct RenameDialog: View {
    let id: UInt64
    let dialog: RenameSnapshot
    @Environment(AppStore.self) private var store
    @FocusState private var focus: RenameField?
    @State private var pages = PagedRows<RenamePreviewRow>()

    var body: some View {
        DialogFrame(title: dialog.title, width: 900) {
            masks
            Text(dialog.help)
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            preview
                .frame(height: 300)
            footer
        }
        .onAppear { focus = .name }
    }

    // ----- the masks ----------------------------------------------------------

    private var masks: some View {
        Grid(alignment: .leading, horizontalSpacing: 8, verticalSpacing: 8) {
            GridRow {
                label("Name")
                HStack(spacing: 6) {
                    field(.name)
                    tokens(dialog.nameTokens, into: .name)
                }
            }
            GridRow {
                label("Extension")
                HStack(spacing: 6) {
                    field(.ext).frame(width: 180)
                    tokens(dialog.extTokens, into: .ext)
                    Spacer().frame(width: 16)
                    Text("Case").foregroundStyle(.secondary)
                    Picker("Case", selection: Binding(
                        get: { dialog.case },
                        set: { send(.caseChange(change: $0)) }
                    )) {
                        ForEach(dialog.cases, id: \.case) { option in
                            Text(option.label).tag(option.case)
                        }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .fixedSize()
                    Spacer(minLength: 0)
                }
            }
            GridRow {
                label("Find")
                HStack(spacing: 8) {
                    field(.find)
                    Text("Replace").foregroundStyle(.secondary)
                    field(.replace)
                    Toggle("Regex", isOn: Binding(get: { dialog.regex }, set: { send(.regex(on: $0)) }))
                        .toggleStyle(.checkbox)
                    Toggle("Match case", isOn: Binding(get: { dialog.matchCase }, set: { send(.matchCase(on: $0)) }))
                        .toggleStyle(.checkbox)
                }
            }
            GridRow {
                label("Counter")
                HStack(spacing: 6) {
                    Text("start").foregroundStyle(.secondary)
                    field(.start, label: "Counter start").frame(width: 64)
                    Text("step").foregroundStyle(.secondary).padding(.leading, 6)
                    field(.step, label: "Counter step").frame(width: 64)
                    Text("digits").foregroundStyle(.secondary).padding(.leading, 6)
                    field(.digits, label: "Counter digits").frame(width: 52)
                    Spacer(minLength: 0)
                }
            }
        }
    }

    private func label(_ text: String) -> some View {
        Text(text)
            .foregroundStyle(.secondary)
            .frame(width: 76, alignment: .leading)
            .gridColumnAlignment(.leading)
    }

    private func field(_ field: RenameField, label: String? = nil) -> some View {
        let text = dialog.fields.first { $0.field == field }
        return CoreTextField(
            placeholder: text?.placeholder ?? "",
            core: text?.text ?? "",
            send: { send(.text(field: field, text: $0)) }
        )
        .accessibilityLabel(label ?? field.label)
        .focused($focus, equals: field)
    }

    /// Placeholder buttons: each appends its token and puts the keyboard
    /// back in the field.
    private func tokens(_ tokens: [String], into field: RenameField) -> some View {
        HStack(spacing: 2) {
            ForEach(tokens, id: \.self) { token in
                Button(token) {
                    send(.insert(field: field, token: token))
                    focus = field
                }
                .buttonStyle(.borderless)
                .controlSize(.small)
                .font(.system(.callout, design: .monospaced))
                .focusable(false)
                .help("Insert \(token) into the \(field.label.lowercased())")
                .accessibilityLabel("Insert \(token)")
            }
        }
    }

    // ----- the preview --------------------------------------------------------

    @ViewBuilder
    private var preview: some View {
        if let error = dialog.error {
            ScrollView {
                Text(error)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
            }
            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(.separator))
        } else {
            let version = dialog.previewVersion
            List(0..<Int(dialog.previewCount), id: \.self) { ix in
                let row = pages.row(ix, version: version) { start, count in
                    store.core.renamePreview(dialog: id, start: start, count: count)
                }
                PreviewRow(row: row)
            }
            .listStyle(.bordered(alternatesRowBackgrounds: true))
            .environment(\.defaultMinListRowHeight, 22)
            .accessibilityLabel("Preview")
        }
    }

    // ----- the footer ---------------------------------------------------------

    private var footer: some View {
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 2) {
                Text(dialog.summary).foregroundStyle(.secondary)
                if let problems = dialog.problemsText {
                    Text(problems).foregroundStyle(.red)
                }
                if let note = dialog.elevatedNote {
                    Text(note).font(.caption).foregroundStyle(.red)
                }
            }
            .accessibilityElement(children: .combine)
            Spacer()
            Button("Cancel", role: .cancel) { store.dialog(id, .cancel) }
                .keyboardShortcut(.cancelAction)
            Button("Rename") { send(.apply) }
                .keyboardShortcut(.defaultAction)
                .disabled(!dialog.canApply)
        }
    }

    private func send(_ input: RenameInput) {
        store.send(.rename(dialog: id, input: input))
    }
}

private struct PreviewRow: View {
    let row: RenamePreviewRow?

    var body: some View {
        if let row {
            HStack(spacing: 8) {
                Text(row.old)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Text("→")
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                Text(row.new)
                    .foregroundStyle(row.problem != nil ? AnyShapeStyle(.red)
                        : row.changed ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
                    .frame(maxWidth: .infinity, alignment: .leading)
                if let problem = row.problem {
                    Text(problem)
                        .font(.caption)
                        .foregroundStyle(.red)
                        .frame(width: 230, alignment: .leading)
                        .help(problem)
                }
            }
            .font(.callout)
            .lineLimit(1)
            .truncationMode(.middle)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(row.problem.map { "\(row.old) to \(row.new), \($0)" } ?? "\(row.old) to \(row.new)")
        } else {
            Color.clear.frame(height: 16)
        }
    }
}

extension RenameField {
    /// What VoiceOver calls the field.
    var label: String {
        switch self {
        case .name: "Name mask"
        case .ext: "Extension mask"
        case .find: "Find"
        case .replace: "Replace with"
        case .start: "Counter start"
        case .step: "Counter step"
        case .digits: "Counter digits"
        }
    }
}

/// A text field whose text the core owns, as the GPUI shell's `CoreInput`:
/// every edit is sent whole, and a change the core makes of its own (a
/// placeholder button) replaces the field's text. The core's answers lag
/// the typing, so its echoes of what was sent are told apart from its own
/// changes and never overwrite what has been typed since.
struct CoreTextField: View {
    let placeholder: String
    /// The core's text.
    let core: String
    let send: (String) -> Void
    /// What the field shows while edits are in flight; nil shows `core`.
    @State private var local: String?
    /// The texts sent that the core has not shown back yet, oldest first.
    @State private var pending: [String] = []

    var body: some View {
        TextField(placeholder, text: Binding(
            get: { local ?? core },
            set: { new in
                guard new != (local ?? core) else { return }
                local = new
                pending.append(new)
                send(new)
            }
        ))
        .onChange(of: core) { _, core in
            if let ix = pending.firstIndex(of: core) {
                // An echo of an edit sent (some may have been skipped).
                pending.removeFirst(ix + 1)
                if pending.isEmpty, local == core { local = nil }
                return
            }
            // A change of the core's own.
            pending.removeAll()
            local = nil
        }
    }
}

/// Rows of a long list fetched from the core by page, dropped when the
/// list's version moves.
@MainActor
final class PagedRows<Row> {
    private var version: UInt64 = 0
    private var rows: [Int: Row] = [:]
    private let page = 200

    func row(_ ix: Int, version: UInt64, fetch: (UInt32, UInt32) -> [Row]) -> Row? {
        if version != self.version {
            self.version = version
            rows.removeAll()
        }
        if let row = rows[ix] { return row }
        let start = (ix / page) * page
        for (offset, row) in fetch(UInt32(start), UInt32(page)).enumerated() {
            rows[start + offset] = row
        }
        return rows[ix]
    }
}
