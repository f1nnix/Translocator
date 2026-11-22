import AppKit
import SwiftUI

/// The settings window (⌘, through the core's `OpenSettings`), as the
/// GPUI shell's `crates/translocator/src/settings_window.rs`: General, Keys
/// and Themes as a native tabbed Settings window.
///
/// There is no OK button: every control sends its `Input.settings(edit:)`
/// at once, and what is drawn is `Snapshot.settings` as the core last said.
/// Recording a key combination, and the note about a clash, are this
/// window's own state.
struct SettingsWindow: View {
    var body: some View {
        TabView {
            Tab("General", systemImage: "gearshape") {
                GeneralPane()
            }
            Tab("Keys", systemImage: "keyboard") {
                KeysPane()
            }
            Tab("Themes", systemImage: "paintpalette") {
                ThemesPane()
            }
        }
        .tabViewStyle(.automatic)
    }
}

extension SettingsWindow {
    /// What a setting row says under its title.
    static func subtitle(_ text: String) -> some View {
        Text(text)
            .font(.callout)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}

// ----- General ---------------------------------------------------------------

private struct GeneralPane: View {
    @Environment(AppStore.self) private var store
    @State private var terminal: String?
    @FocusState private var terminalFocused: Bool
    private let ranges = settingsRanges()
    private let sorts = sortKeyChoices()
    private let dates = dateFormatChoices()

    var body: some View {
        let general = store.snapshot.settings.general
        Form {
            Section("Startup") {
                toggle(general.restoreFolders, edit: { .restoreFolders(on: $0) }) {
                    Text("Reopen the last folders")
                    Text("Start where the panes were when you quit, instead of in your home folder.")
                }
            }
            Section("File list") {
                toggle(general.showHidden, edit: { .showHidden(on: $0) }) {
                    Text("Show hidden files")
                    Text(hiddenHelp)
                }
                toggle(general.foldersFirst, edit: { .foldersFirst(on: $0) }) {
                    Text("Folders first")
                    Text("Keep folders above files whatever the list is sorted by.")
                }
                Picker(selection: Binding(
                    get: { general.sort },
                    set: { send(.sort(key: $0)) }
                )) {
                    ForEach(sorts, id: \.key) { choice in
                        Text(choice.label).tag(choice.key)
                    }
                } label: {
                    Text("Sort new tabs by")
                    Text("Clicking a column header still sorts one tab by hand.")
                }
                toggle(general.sortDescending, edit: { .sortDescending(on: $0) }) {
                    Text("Largest or newest first")
                }
                toggle(general.showIcons, edit: { .showIcons(on: $0) }) {
                    Text("File icons")
                    Text("Without them a row is all name, which fits more of a long one.")
                }
                Picker("Dates", selection: Binding(
                    get: { general.dateFormat },
                    set: { send(.dateFormat(format: $0)) }
                )) {
                    ForEach(dates, id: \.format) { choice in
                        Text(choice.label).monospacedDigit().tag(choice.format)
                    }
                }
                Stepper(
                    value: Binding(get: { Double(general.fontSize) }, set: { send(.fontSize(size: Float($0))) }),
                    in: Double(ranges.fontSizeMin)...Double(ranges.fontSizeMax),
                    step: 1
                ) {
                    LabeledContent {
                        Text("\(Int(general.fontSize)) pt").monospacedDigit()
                    } label: {
                        Text("Text size")
                        Text("The size of the file lists' text.")
                    }
                }
                .accessibilityValue("\(Int(general.fontSize)) points")
                Stepper(
                    value: Binding(get: { Double(general.rowHeight) }, set: { send(.rowHeight(height: Float($0))) }),
                    in: Double(ranges.rowHeightMin)...Double(ranges.rowHeightMax),
                    step: 1
                ) {
                    LabeledContent {
                        Text("\(Int(general.rowHeight)) pt").monospacedDigit()
                    } label: {
                        Text("Row height")
                        Text("Lower fits more files on the screen.")
                    }
                }
                .accessibilityValue("\(Int(general.rowHeight)) points")
            }
            Section("Safety") {
                toggle(general.confirmTrash, edit: { .confirmTrash(on: $0) }) {
                    Text("Ask before moving to Trash")
                    Text("Deleting permanently always asks, whatever this says.")
                }
            }
            Section("Tools") {
                VStack(alignment: .leading, spacing: 6) {
                    TextField("Terminal", text: Binding(
                        get: { terminal ?? general.terminal },
                        set: { terminal = $0 }
                    ), prompt: Text("Terminal.app"))
                    .focused($terminalFocused)
                    .onSubmit(commitTerminal)
                    SettingsWindow.subtitle(terminalHelp())
                }
            }
            Section {
                HStack {
                    Spacer()
                    Button("Restore Defaults") {
                        terminal = nil
                        send(.resetGeneral)
                    }
                    .help("Every setting on this page back to its default")
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 600)
        .frame(minHeight: 560)
        .onChange(of: terminalFocused) { _, focused in if !focused { commitTerminal() } }
        .onDisappear(perform: commitTerminal)
    }

    private var hiddenHelp: String {
        let key = store.keys.label("toggle-hidden").map { "\($0) toggles it for this session too." }
        return ["Names that begin with a dot.", key].compactMap { $0 }.joined(separator: " ")
    }

    /// The terminal goes to the core when editing ends, not per keystroke.
    private func commitTerminal() {
        guard let text = terminal else { return }
        terminal = nil
        if text != store.snapshot.settings.general.terminal {
            send(.terminal(command: text))
        }
    }

    private func toggle<Label: View>(
        _ on: Bool, edit: @escaping (Bool) -> SettingsEdit, @ViewBuilder label: () -> Label
    ) -> some View {
        Toggle(isOn: Binding(get: { on }, set: { send(edit($0)) }), label: label)
    }

    private func send(_ edit: SettingsEdit) {
        store.send(.settings(edit: edit))
    }
}

// ----- Keys ------------------------------------------------------------------

private struct KeysPane: View {
    @Environment(AppStore.self) private var store
    @State private var query = ""
    @State private var recording: String?
    /// A note under one command's row: a clash, or a key that cannot be used.
    @State private var note: (command: String, text: String)?
    @State private var monitor: Any?

    var body: some View {
        let commands = filtered
        let groups = commands.reduce(into: [String]()) { if !$0.contains($1.group) { $0.append($1.group) } }
        VStack(alignment: .leading, spacing: 10) {
            SettingsWindow.subtitle(
                "Press + to add a key combination, or the ✕ on one to take it away. Escape cancels recording."
            )
            SearchField(text: $query, prompt: "Search commands or keys")
            List {
                ForEach(groups, id: \.self) { group in
                    Section(group) {
                        ForEach(commands.filter { $0.group == group }, id: \.id) { command in
                            KeyRow(
                                command: command,
                                recording: recording == command.id,
                                note: note?.command == command.id ? note?.text : nil,
                                record: { startRecording(command) },
                                stop: stopRecording,
                                remove: { remove($0, from: command) },
                                reset: { reset(command) }
                            )
                        }
                    }
                }
            }
            .listStyle(.inset)
            .alternatingRowBackgrounds()
            .overlay {
                if commands.isEmpty {
                    Text("No command matches “\(query)”.").foregroundStyle(.secondary)
                }
            }
            HStack {
                Text(query.isEmpty ? "\(commands.count) commands" : "\(commands.count) of \(store.keys.commands.count) commands")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Spacer()
                Button("Restore Defaults") {
                    note = nil
                    for command in commands where command.changed {
                        send(.keys(command: command.id, keys: nil))
                    }
                }
                .disabled(!commands.contains(where: \.changed))
                .help(query.isEmpty ? "Every command back to its default keys" : "The commands shown back to their default keys")
            }
        }
        .padding(20)
        .frame(width: 640, height: 600)
        .onDisappear(perform: stopRecording)
    }

    /// The commands whose label, group or keys match the search.
    private var filtered: [KeyCommand] {
        let words = query.lowercased().split(separator: " ").map(String.init)
        guard !words.isEmpty else { return store.keys.commands }
        return store.keys.commands.filter { command in
            var haystack = [command.label, command.group]
            for binding in command.bindings { haystack += [binding.keys, binding.pretty] }
            for keys in command.defaults { haystack += [keys, prettyKeys(keys: keys)] }
            let text = haystack.joined(separator: " ").lowercased()
            return words.allSatisfy { text.contains($0) }
        }
    }

    // ----- recording ------------------------------------------------------

    /// Listens for the next key pressed in this window. A local monitor
    /// sees a key before the menus do, so recording ⌘Q does not quit.
    private func startRecording(_ command: KeyCommand) {
        stopRecording()
        note = nil
        recording = command.id
        let window = NSApp.keyWindow
        let id = command.id
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            guard event.window === window else { return event }
            MainActor.assumeIsolated { recorded(event, for: id) }
            return nil
        }
    }

    private func stopRecording() {
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
        recording = nil
    }

    /// A key arrived while recording (lone modifiers never come as a key).
    private func recorded(_ event: NSEvent, for id: String) {
        stopRecording()
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        if event.keyCode == 53 && flags.intersection([.command, .control, .option, .shift]).isEmpty {
            return
        }
        guard let command = store.keys.command(id), let stroke = Self.stroke(event) else {
            NSSound.beep()
            return
        }
        let keys = keystrokeSpelling(stroke: stroke)
        if let error = validateKeys(keys: keys) {
            note = (id, error)
            return
        }
        let labels = conflicts(keys, command)
        note = labels.isEmpty ? nil : (id, "\(prettyKeys(keys: keys)) is also \(labels.joined(separator: ", ")).")
        var bound = command.bindings.map(\.keys)
        if !command.bindings.contains(where: { $0.canonical == keys }) {
            bound.append(keys)
        }
        send(.keys(command: id, keys: bound))
    }

    /// The keystroke of a key event, in the core's terms: the key unshifted
    /// with ⇧ as a modifier, named keys by name.
    static func stroke(_ event: NSEvent) -> KeyStroke? {
        let flags = event.modifierFlags
        let key: String
        if let name = KeyMap.named[event.keyCode] {
            key = name
        } else if let base = event.characters(byApplyingModifiers: [])?.lowercased(), !base.isEmpty,
                  !base.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) || $0.value >= 0xF700 })
        {
            key = base
        } else {
            return nil
        }
        return KeyStroke(
            control: flags.contains(.control),
            alt: flags.contains(.option),
            shift: flags.contains(.shift),
            platform: flags.contains(.command),
            function: false,
            key: key
        )
    }

    /// What else answers to `keys` in the command's context, as the core
    /// decides it (however the modifiers are spelled).
    private func conflicts(_ keys: String, _ command: KeyCommand) -> [String] {
        store.core.keyConflicts(keys: keys, commandId: command.id).map(\.label)
    }

    private func remove(_ binding: KeyBinding, from command: KeyCommand) {
        note = nil
        send(.keys(command: command.id, keys: command.bindings.map(\.keys).filter { $0 != binding.keys }))
    }

    private func reset(_ command: KeyCommand) {
        note = nil
        send(.keys(command: command.id, keys: nil))
    }

    private func send(_ edit: SettingsEdit) {
        store.send(.settings(edit: edit))
    }
}

/// One command and its keys.
private struct KeyRow: View {
    let command: KeyCommand
    let recording: Bool
    let note: String?
    let record: () -> Void
    let stop: () -> Void
    let remove: (KeyBinding) -> Void
    let reset: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text(command.label)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if recording {
                    Text("Press a key combination…")
                        .font(.callout)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 2)
                        .background(Color.accentColor.opacity(0.15), in: .rect(cornerRadius: 5))
                        .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(Color.accentColor))
                        .accessibilityAddTraits(.updatesFrequently)
                    Button("Cancel", action: stop)
                        .controlSize(.small)
                } else {
                    ForEach(command.bindings, id: \.keys) { binding in
                        KeyChip(binding: binding, command: command.label, remove: { remove(binding) })
                    }
                    if command.bindings.isEmpty {
                        Text("no key")
                            .font(.callout)
                            .foregroundStyle(.tertiary)
                    }
                    Button(action: record) {
                        Image(systemName: "plus")
                    }
                    .buttonStyle(.borderless)
                    .help("Add a key combination")
                    .accessibilityLabel("Add a key combination for \(command.label)")
                    Button(action: reset) {
                        Image(systemName: "arrow.uturn.backward")
                    }
                    .buttonStyle(.borderless)
                    .disabled(!command.changed)
                    .help("Back to the default")
                    .accessibilityLabel("Reset the keys of \(command.label)")
                }
            }
            if let note {
                Label(note, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .accessibilityLabel("Note: \(note)")
            }
        }
        .padding(.vertical, 1)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(command.label)
    }
}

private struct KeyChip: View {
    let binding: KeyBinding
    let command: String
    let remove: () -> Void

    var body: some View {
        HStack(spacing: 2) {
            Text(binding.pretty)
                .font(.system(.callout, design: .rounded).monospacedDigit())
            Button(action: remove) {
                Image(systemName: "xmark")
                    .font(.system(size: 8, weight: .bold))
                    .frame(width: 14, height: 14)
            }
            .buttonStyle(.borderless)
            .help("Take this key away")
            .accessibilityLabel("Remove \(binding.pretty) from \(command)")
        }
        .padding(.leading, 6)
        .padding(.trailing, 2)
        .padding(.vertical, 1)
        .background(.quaternary, in: .rect(cornerRadius: 5))
        .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(.separator))
    }
}

/// A native search field (with its clear button).
private struct SearchField: NSViewRepresentable {
    @Binding var text: String
    let prompt: String

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSSearchField {
        let field = NSSearchField()
        field.placeholderString = prompt
        field.delegate = context.coordinator
        field.sendsSearchStringImmediately = true
        field.setAccessibilityLabel(prompt)
        return field
    }

    func updateNSView(_ field: NSSearchField, context: Context) {
        context.coordinator.parent = self
        if field.stringValue != text { field.stringValue = text }
    }

    @MainActor
    final class Coordinator: NSObject, NSSearchFieldDelegate {
        var parent: SearchField

        init(_ parent: SearchField) { self.parent = parent }

        func controlTextDidChange(_ note: Notification) {
            guard let field = note.object as? NSSearchField else { return }
            parent.text = field.stringValue
        }
    }
}

// ----- Themes ----------------------------------------------------------------

private struct ThemesPane: View {
    @Environment(AppStore.self) private var store
    @Environment(\.theme) private var theme
    private let modes = appearanceModeChoices()
    private let light = themeChoices(dark: false)
    private let dark = themeChoices(dark: true)

    var body: some View {
        let appearance = store.snapshot.settings.appearance
        let general = store.snapshot.settings.general
        Form {
            Section {
                Picker(selection: Binding(
                    get: { appearance.mode },
                    set: { store.send(.settings(edit: .appearanceMode(mode: $0))) }
                )) {
                    ForEach(modes, id: \.mode) { choice in
                        Text(choice.label).tag(choice.mode)
                    }
                } label: {
                    Text("Use")
                    Text("“System” follows the Mac's light and dark setting.")
                }
                .pickerStyle(.segmented)
                themePicker("Light theme", "Used while the appearance is light.", light, appearance.light) {
                    .lightTheme(name: $0)
                }
                themePicker("Dark theme", "Used while the appearance is dark.", dark, appearance.dark) {
                    .darkTheme(name: $0)
                }
            } header: {
                Text("Appearance")
            } footer: {
                SettingsWindow.subtitle(
                    "Ten colour schemes, each in a light and a dark variant. They colour the file lists, the console and the viewer; the window around them keeps the system's look. “Default” is the system's own colours."
                )
            }
            Section("Preview") {
                HStack(alignment: .top, spacing: 14) {
                    preview(appearance.light, scheme: .light, label: "Light", inUse: !theme.isDark, general: general)
                    preview(appearance.dark, scheme: .dark, label: "Dark", inUse: theme.isDark, general: general)
                }
                .padding(.vertical, 4)
            }
        }
        .formStyle(.grouped)
        .frame(width: 600)
        .frame(minHeight: 560)
    }

    private func themePicker(
        _ title: String, _ help: String, _ choices: [ThemeChoice], _ current: String,
        edit: @escaping (String) -> SettingsEdit
    ) -> some View {
        Picker(selection: Binding(
            get: { choices.contains { $0.name == current } ? current : choices.first?.name ?? current },
            set: { store.send(.settings(edit: edit($0))) }
        )) {
            ForEach(choices, id: \.name) { choice in
                Text(choice.scheme).tag(choice.name)
            }
        } label: {
            Text(title)
            Text(help)
        }
    }

    private func preview(
        _ name: String, scheme: ColorScheme, label: String, inUse: Bool, general: GeneralSettings
    ) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 4) {
                Text(label).font(.callout.weight(.medium))
                if inUse {
                    Text("· in use").font(.callout).foregroundStyle(.secondary)
                }
            }
            ThemePreview(colors: Translocator.palette(name: name).map(ContentColors.init) ?? .system, general: general)
                .environment(\.colorScheme, scheme)
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(label) preview, \(name)\(inUse ? ", in use" : "")")
    }
}

/// A few rows of a file list in a scheme, at the list's text size and row
/// height, so a change can be seen without leaving the window.
private struct ThemePreview: View {
    let colors: ContentColors
    let general: GeneralSettings

    private struct Sample: Identifiable {
        let id: String
        let size: String
        let symbol: String
        var folder = false
        var executable = false
        var cursor = false
        var marked = false
    }

    private let samples = [
        Sample(id: "..", size: "", symbol: "arrow.turn.left.up", folder: true),
        Sample(id: "translocator", size: "folder", symbol: "folder.fill", folder: true, cursor: true),
        Sample(id: "README.md", size: "2.1 kB", symbol: "doc.text"),
        Sample(id: "install.sh", size: "870 B", symbol: "terminal", executable: true, marked: true),
        Sample(id: "notes.txt", size: "14 kB", symbol: "doc.text"),
    ]

    var body: some View {
        let size = CGFloat(general.fontSize)
        let height = CGFloat(general.rowHeight)
        VStack(spacing: 0) {
            HStack {
                Text("Name")
                Spacer()
                Text("Size")
            }
            .font(.system(size: max(size - 2, 9)))
            .foregroundStyle(Color(nsColor: colors.secondary))
            .padding(.horizontal, 8)
            .frame(height: 20)
            .background(Color(nsColor: colors.chrome))
            Divider()
            ForEach(samples) { sample in
                HStack(spacing: 6) {
                    if general.showIcons {
                        Image(systemName: sample.symbol)
                            .font(.system(size: size))
                            .foregroundStyle(Color(nsColor: iconColor(sample)))
                            .frame(width: 16)
                    }
                    Text(sample.id)
                        .font(.system(size: size, weight: sample.marked ? .semibold : .regular))
                        .foregroundStyle(Color(nsColor: textColor(sample)))
                        .lineLimit(1)
                    Spacer(minLength: 4)
                    Text(sample.size)
                        .font(.system(size: size).monospacedDigit())
                        .foregroundStyle(Color(nsColor: sample.cursor ? colors.cursorSecondary : colors.secondary))
                }
                .padding(.horizontal, 8)
                .frame(height: height)
                .background(Color(nsColor: background(sample)))
            }
            Divider()
            HStack {
                Text("1 marked").foregroundStyle(Color(nsColor: colors.marked))
                Spacer()
                Text("12 GB free").foregroundStyle(Color(nsColor: colors.secondary))
            }
            .font(.system(size: max(size - 2, 9)))
            .padding(.horizontal, 8)
            .frame(height: 20)
            .background(Color(nsColor: colors.chrome))
        }
        .background(Color(nsColor: colors.background))
        .clipShape(.rect(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Color(nsColor: colors.border)))
        .accessibilityHidden(true)
    }

    private func background(_ s: Sample) -> NSColor {
        if s.cursor { return colors.cursor }
        if s.marked { return colors.markedBackground }
        return .clear
    }

    private func textColor(_ s: Sample) -> NSColor {
        if s.cursor && (colors.isSystem || !s.marked) { return colors.cursorText }
        if s.marked { return colors.marked }
        return colors.text
    }

    private func iconColor(_ s: Sample) -> NSColor {
        if s.cursor && colors.isSystem { return colors.cursorText }
        if s.folder { return colors.folder }
        if s.executable { return colors.executable }
        return colors.secondary
    }
}
