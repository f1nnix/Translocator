import AppKit
import SwiftUI
import TLCore

extension BookmarkField: Identifiable {
    public var id: String { key }
}

extension BookmarkRow: Identifiable {}

/// The bookmark editor (`DialogKindSnapshot.bookmarkEdit`), as the GPUI
/// shell's `render_edit` in `crates/translocator/src/bookmarks.rs`.
///
/// Generic, like the core: the type picker (new bookmarks only), the name,
/// then whatever fields the selected plugin declares, the advanced ones
/// behind a disclosure. Validation, summaries and saving are the core's; the
/// sheet only asks the system for a local path ("Choose…"). Every edit goes
/// out as `Input.bookmarks(input:)` at once, and the sheet closes when the
/// core drops the dialog.
struct BookmarkEditDialog: View {
    let id: UInt64
    let dialog: BookmarkEditSnapshot
    @Environment(AppStore.self) private var store
    /// What was typed, by `plugin/key` ("name" for the name), shown until
    /// the core's echo catches up so a fast typist's text is never reset.
    @State private var typed: [String: String] = [:]
    @State private var revealed: Set<String> = []
    @FocusState private var focus: String?

    var body: some View {
        DialogFrame(title: dialog.title, width: 560) {
            if dialog.isNew && dialog.plugins.count > 1 {
                Picker("Type", selection: Binding(
                    get: { dialog.plugin },
                    set: { send(.plugin(dialog: id, plugin: $0)) }
                )) {
                    ForEach(dialog.plugins, id: \.id) { plugin in
                        Text(plugin.name).tag(plugin.id)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .accessibilityLabel("Connection type")
            }
            if !dialog.description.isEmpty {
                Text(dialog.description)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Form {
                TextField("Name:", text: text("name", dialog.name) { send(.name(dialog: id, text: $0)) },
                          prompt: Text(dialog.namePlaceholder))
                    .focused($focus, equals: "name")
                ForEach(dialog.fields) { field in
                    row(field)
                }
            }
            .formStyle(.columns)
            .frame(maxWidth: .infinity, alignment: .leading)
            if dialog.hasAdvanced {
                Button {
                    send(.toggleAdvanced(dialog: id))
                } label: {
                    Label(dialog.advancedLabel, systemImage: dialog.advanced ? "chevron.down" : "chevron.right")
                        .font(.callout)
                }
                .buttonStyle(.borderless)
                .accessibilityValue(dialog.advanced ? "shown" : "hidden")
            }
            if let error = dialog.error {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.red)
                    .font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityLabel("Error: \(error)")
            }
            HStack {
                if dialog.saving {
                    ProgressView()
                        .controlSize(.small)
                        .accessibilityLabel("Checking the bookmark")
                }
                Spacer()
                Button("Cancel", role: .cancel) { store.dialog(id, .cancel) }
                    .keyboardShortcut(.cancelAction)
                Button("Save") { send(.save(dialog: id, open: false)) }
                    .disabled(dialog.saving)
                Button("Save & Open") { send(.save(dialog: id, open: true)) }
                    .keyboardShortcut(.defaultAction)
                    .disabled(dialog.saving)
            }
            .padding(.top, 4)
        }
        .onAppear { focus = dialog.autofocus ?? "name" }
    }

    // ----- one field ----------------------------------------------------------

    @ViewBuilder
    private func row(_ field: BookmarkField) -> some View {
        let key = field.key
        LabeledContent {
            VStack(alignment: .leading, spacing: 3) {
                control(field)
                if !field.help.isEmpty {
                    Text(field.help)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        } label: {
            Text("\(field.label):")
        }
        .id("\(dialog.plugin)/\(key)")
    }

    @ViewBuilder
    private func control(_ field: BookmarkField) -> some View {
        let key = field.key
        let value = text("\(dialog.plugin)/\(key)", field.value) { send(.field(dialog: id, key: key, value: $0)) }
        let prompt = field.placeholder.isEmpty ? nil : Text(field.placeholder)
        switch field.kind {
        case .toggle:
            Toggle(field.label, isOn: Binding(
                get: { field.on },
                set: { send(.toggle(dialog: id, key: key, on: $0)) }
            ))
            .labelsHidden()
            .toggleStyle(.switch)
            .controlSize(.small)
        case .choice:
            Picker(field.label, selection: Binding(
                get: { field.value },
                set: { send(.field(dialog: id, key: key, value: $0)) }
            )) {
                ForEach(field.choices, id: \.value) { choice in
                    Text(choice.label).tag(choice.value)
                }
            }
            .labelsHidden()
            .fixedSize()
        case .secret:
            HStack(spacing: 4) {
                Group {
                    if revealed.contains(key) {
                        TextField(field.label, text: value, prompt: prompt)
                    } else {
                        SecureField(field.label, text: value, prompt: prompt)
                    }
                }
                .labelsHidden()
                .focused($focus, equals: key)
                Button {
                    if revealed.contains(key) { revealed.remove(key) } else { revealed.insert(key) }
                } label: {
                    Image(systemName: revealed.contains(key) ? "eye.slash" : "eye")
                }
                .buttonStyle(.borderless)
                .help(revealed.contains(key) ? "Hide" : "Show")
                .accessibilityLabel(revealed.contains(key) ? "Hide \(field.label)" : "Show \(field.label)")
            }
        case .localPath:
            HStack(spacing: 6) {
                TextField(field.label, text: value, prompt: prompt)
                    .labelsHidden()
                    .focused($focus, equals: key)
                Button("Choose…") { choose(field) }
                    .accessibilityLabel("Choose \(field.label)")
            }
        case .number:
            TextField(field.label, text: value, prompt: prompt)
                .labelsHidden()
                .monospacedDigit()
                .frame(maxWidth: 120)
                .focused($focus, equals: key)
        case .text:
            TextField(field.label, text: value, prompt: prompt)
                .labelsHidden()
                .focused($focus, equals: key)
        }
    }

    /// A text binding: what was typed here, else the core's value.
    private func text(_ slot: String, _ current: String, _ send: @escaping (String) -> Void) -> Binding<String> {
        Binding(
            get: { typed[slot] ?? current },
            set: { new in
                typed[slot] = new
                send(new)
            }
        )
    }

    private func send(_ input: BookmarksInput) {
        store.send(.bookmarks(input: input))
    }

    /// "Choose…": the system's file (or folder) picker; its answer goes to
    /// the core, which fills the field in.
    private func choose(_ field: BookmarkField) {
        let panel = NSOpenPanel()
        panel.canChooseFiles = !dialog.picksDirectories
        panel.canChooseDirectories = dialog.picksDirectories
        panel.allowsMultipleSelection = false
        panel.showsHiddenFiles = true
        panel.message = field.label
        let current = (typed["\(dialog.plugin)/\(field.key)"] ?? field.value) as NSString
        if current.length > 0 {
            let url = URL(fileURLWithPath: current.expandingTildeInPath)
            panel.directoryURL = dialog.picksDirectories ? url : url.deletingLastPathComponent()
        }
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let bytes = url.withUnsafeFileSystemRepresentation { pointer in
            pointer.map { Data(bytes: $0, count: strlen($0)) } ?? Data(url.path.utf8)
        }
        typed["\(dialog.plugin)/\(field.key)"] = nil
        send(.pathChosen(dialog: id, key: field.key, path: bytes))
    }
}

/// The bookmark manager (`DialogKindSnapshot.bookmarks`), as the GPUI
/// shell's `render_manage`: every bookmark with its summary and whether it
/// is connected, and Connect / Open, Edit, Delete and the arrows that order
/// them; New Bookmark… and Close below.
struct BookmarksManagerDialog: View {
    let id: UInt64
    let dialog: BookmarksManagerSnapshot
    @Environment(AppStore.self) private var store

    var body: some View {
        DialogFrame(title: dialog.title, width: 620) {
            Group {
                if dialog.rows.isEmpty {
                    VStack(spacing: 8) {
                        Image(systemName: "bookmark")
                            .font(.largeTitle)
                            .foregroundStyle(.tertiary)
                            .accessibilityHidden(true)
                        Text(dialog.emptyText)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    List(dialog.rows) { row in
                        BookmarkManagerRow(manager: id, row: row)
                    }
                    .listStyle(.bordered)
                    .alternatingRowBackgrounds()
                }
            }
            .frame(height: 320)
            HStack {
                Button {
                    send(.new(manager: id))
                } label: {
                    Label("New Bookmark…", systemImage: "plus")
                }
                Spacer()
                Button("Close") { store.dialog(id, .submit) }
                    .keyboardShortcut(.defaultAction)
                Button("") { store.dialog(id, .cancel) }
                    .keyboardShortcut(.cancelAction)
                    .hidden()
                    .frame(width: 0)
                    .accessibilityHidden(true)
            }
        }
    }

    private func send(_ input: BookmarksInput) {
        store.send(.bookmarks(input: input))
    }
}

private struct BookmarkManagerRow: View {
    let manager: UInt64
    let row: BookmarkRow
    @Environment(AppStore.self) private var store

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: row.icon == .local ? "folder.fill" : "server.rack")
                .foregroundStyle(row.connected ? Color.green : Color.secondary)
                .frame(width: 18)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                Text(row.name)
                    .lineLimit(1)
                    .truncationMode(.tail)
                if !row.summary.isEmpty {
                    Text(row.summary)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .combine)
            .accessibilityValue(row.connected ? "connected" : "")
            HStack(spacing: 2) {
                icon("arrow.up", "Move \(row.name) up", help: "Move up") {
                    send(.move(id: row.id, delta: -1))
                }
                .disabled(!row.canMoveUp)
                icon("arrow.down", "Move \(row.name) down", help: "Move down") {
                    send(.move(id: row.id, delta: 1))
                }
                .disabled(!row.canMoveDown)
                icon("pencil", "Edit \(row.name)", help: "Edit") {
                    send(.edit(manager: manager, id: row.id))
                }
                icon("trash", "Delete \(row.name)", help: "Delete") {
                    send(.delete(id: row.id))
                }
            }
            Button(row.openLabel) { send(.open(manager: manager, id: row.id)) }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                .accessibilityLabel("\(row.openLabel) \(row.name)")
        }
        .padding(.vertical, 3)
        .contentShape(Rectangle())
        .onTapGesture(count: 2) { send(.open(manager: manager, id: row.id)) }
        .contextMenu {
            Button(row.openLabel) { send(.open(manager: manager, id: row.id)) }
            Button("Edit…") { send(.edit(manager: manager, id: row.id)) }
            Divider()
            Button("Move Up") { send(.move(id: row.id, delta: -1)) }.disabled(!row.canMoveUp)
            Button("Move Down") { send(.move(id: row.id, delta: 1)) }.disabled(!row.canMoveDown)
            Divider()
            Button("Delete…", role: .destructive) { send(.delete(id: row.id)) }
        }
    }

    private func icon(_ symbol: String, _ label: String, help: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .frame(width: 20, height: 20)
        }
        .buttonStyle(.borderless)
        .help(help)
        .accessibilityLabel(label)
    }

    private func send(_ input: BookmarksInput) {
        store.send(.bookmarks(input: input))
    }
}
