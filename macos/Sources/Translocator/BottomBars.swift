import AppKit
import SwiftUI
import TLCore

/// The command line: the prompt, the field, and the console toggle.
struct CommandLineBar: View {
    @Environment(AppStore.self) private var store
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        HStack(spacing: 8) {
            Text(store.snapshot.prompt)
                .font(.system(size: 12, design: .monospaced))
                .foregroundStyle(store.snapshot.promptElevated ? .red : .secondary)
                .lineLimit(1)
                .truncationMode(.head)
                .frame(maxWidth: 420, alignment: .leading)
                .fixedSize(horizontal: true, vertical: false)
                .accessibilityLabel("Prompt \(store.snapshot.prompt)")
            TextField("Run a command in the active folder…", text: $text)
                .textFieldStyle(.plain)
                .font(.system(size: 12, design: .monospaced))
                .focused($focused)
                .onSubmit {
                    store.send(.commandLine(input: .run(text: text)))
                }
                .onExitCommand {
                    // The command line's own keys: Escape goes back to the list.
                    if case .command(let command)? = store.keys.target(spelling: "escape", in: .commandLine) {
                        store.command(command)
                    }
                }
                .accessibilityLabel("Command line")
            Button {
                store.command(.toggleConsole)
            } label: {
                Image(systemName: "apple.terminal")
                    .symbolVariant(store.snapshot.consoleShown ? .fill : .none)
            }
            .buttonStyle(.borderless)
            .help("Console output\(store.keys.label("toggle-console").map { " (\($0))" } ?? "")")
            .accessibilityLabel(store.snapshot.consoleShown ? "Hide the console" : "Show the console")
        }
        .padding(.horizontal, 8)
        .frame(height: 30)
        .onChange(of: store.commandLineFocus) { focused = true }
        .onChange(of: store.snapshot.cmdlineClears) { text = "" }
    }
}

/// What the command line printed: read-only, monospaced, kept scrolled to
/// the end.
struct ConsoleView: NSViewRepresentable {
    let text: String
    /// The content layer's scheme (`Theme`), read here so a change redraws.
    var colors = Theme.shared.colors

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSTextView.scrollableTextView()
        if let view = scroll.documentView as? NSTextView {
            view.isEditable = false
            view.isSelectable = true
            view.isRichText = false
            view.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
            view.textContainerInset = NSSize(width: 6, height: 6)
            view.setAccessibilityLabel("Console")
        }
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let view = scroll.documentView as? NSTextView else { return }
        if view.backgroundColor != colors.background {
            view.backgroundColor = colors.background
            view.textColor = colors.text
            view.insertionPointColor = colors.text
        }
        guard view.string != text else { return }
        view.string = text
        view.scrollToEndOfDocument(nil)
    }
}

/// The first running job under the panes, with the count of the others.
struct JobsStrip: View {
    @Environment(AppStore.self) private var store
    @Binding var jobsShown: Bool

    var body: some View {
        let running = store.snapshot.jobs.filter { $0.state == .running }
        if let job = running.first {
            VStack(spacing: 0) {
                Divider()
                HStack(spacing: 12) {
                    Text(job.title)
                        .lineLimit(1)
                        .frame(width: 260, alignment: .leading)
                    JobProgress(job: job)
                    Text(job.short)
                        .foregroundStyle(.secondary)
                        .monospacedDigit()
                    if running.count > 1 {
                        Button("+\(running.count - 1)") { store.command(.toggleJobs) }
                            .buttonStyle(.borderless)
                            .accessibilityLabel("\(running.count - 1) more jobs")
                    }
                    Button {
                        store.send(.jobs(input: .cancel(job: job.id)))
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                    }
                    .buttonStyle(.borderless)
                    .help("Cancel")
                    .accessibilityLabel("Cancel \(job.title)")
                }
                .font(.caption)
                .padding(.horizontal, 8)
                .frame(height: 28)
            }
        }
    }
}

struct JobProgress: View {
    let job: JobSnapshot

    var body: some View {
        if let fraction = job.fraction {
            ProgressView(value: Double(fraction))
                .controlSize(.small)
        } else {
            ProgressView()
                .progressViewStyle(.linear)
                .controlSize(.small)
        }
    }
}

/// Every listed job, newest first, in the toolbar's popover (⌘J).
struct JobsList: View {
    @Environment(AppStore.self) private var store

    var body: some View {
        let jobs = store.snapshot.jobs
        VStack(alignment: .leading, spacing: 8) {
            Text("Jobs").font(.headline)
            if jobs.isEmpty {
                Text("No jobs yet. Copies, moves and deletions run here.")
                    .foregroundStyle(.secondary)
                    .padding(.vertical, 12)
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(jobs.reversed(), id: \.id) { job in
                            JobRow(job: job)
                            Divider()
                        }
                    }
                }
                .frame(maxHeight: 420)
            }
            if jobs.contains(where: { $0.state != .running }) {
                HStack {
                    Spacer()
                    Button("Clear Finished") { store.send(.jobs(input: .clearFinished)) }
                }
            }
        }
        .padding(14)
        .frame(width: 400)
    }
}

struct JobRow: View {
    let job: JobSnapshot
    @Environment(AppStore.self) private var store

    var body: some View {
        let running = job.state == .running
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(job.title).lineLimit(1)
                Spacer()
                if running {
                    Button {
                        store.send(.jobs(input: .cancel(job: job.id)))
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                    }
                    .buttonStyle(.borderless)
                    .help("Cancel")
                    .accessibilityLabel("Cancel \(job.title)")
                }
            }
            if running { JobProgress(job: job) }
            Text(job.detail)
                .font(.caption)
                .foregroundStyle(color)
                .lineLimit(2)
            if running && !job.current.isEmpty {
                Text(job.current)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
        }
        .padding(.vertical, 6)
        .accessibilityElement(children: .combine)
    }

    private var color: Color {
        switch job.state {
        case .failed: .red
        case .cancelled: .orange
        case .done: .green
        case .running: .secondary
        }
    }
}

/// F2…F9 with the keys they answer to now.
struct FunctionBar: View {
    @Environment(AppStore.self) private var store

    private static let keys: [(String, String, Command)] = [
        ("rename-file", "Rename", .renameFile),
        ("view-file", "View", .viewFile),
        ("edit-file", "Edit", .editFile),
        ("copy-files", "Copy", .copyFiles),
        ("move-files", "Move", .moveFiles),
        ("new-folder", "New Folder", .newFolder),
        ("trash-files", "Delete", .trashFiles),
        ("focus-command-line", "Command", .focusCommandLine),
    ]

    var body: some View {
        HStack(spacing: 2) {
            ForEach(Self.keys, id: \.0) { id, label, command in
                let key = store.keys.label(id)
                Button {
                    store.command(command)
                } label: {
                    HStack(spacing: 4) {
                        if let key {
                            Text(key)
                                .font(.system(size: 10, weight: .semibold))
                                .foregroundStyle(.secondary)
                        }
                        Text(label).font(.system(size: 12))
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.borderless)
                .accessibilityLabel(key.map { "\(label), \($0)" } ?? label)
            }
        }
        .padding(.horizontal, 4)
        .frame(height: 26)
        .background(.bar)
    }
}
