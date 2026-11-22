import SwiftUI
import TLCore

/// The synchronize-directories dialog (`DialogKindSnapshot.sync`), after
/// the GPUI shell's `crates/translocator/src/sync.rs`.
///
/// The two folders, Subfolders / the direction / Show identical / Compare
/// Again, the comparison state (spinner, error, empty), the plan rows with
/// their action buttons (`SyncInput.cycle`), fetched by range from
/// `Core.syncRows` and dropped when `rowsVersion` moves, the totals, and
/// Synchronize (or Return). Cancel, or closing it any other way, stops a
/// comparison still running.
struct SyncDialog: View {
    let id: UInt64
    let dialog: SyncSnapshot
    @Environment(AppStore.self) private var store
    @State private var pages = PagedRows<SyncRow>()

    var body: some View {
        DialogFrame(title: dialog.title, width: 1000) {
            ends
            options
            content
                .frame(height: 380)
            footer
        }
    }

    private var ends: some View {
        Grid(alignment: .leading, horizontalSpacing: 8, verticalSpacing: 4) {
            GridRow {
                Text("Left").foregroundStyle(.secondary)
                Text(dialog.leftDisplay).lineLimit(1).truncationMode(.middle).textSelection(.enabled)
            }
            GridRow {
                Text("Right").foregroundStyle(.secondary)
                Text(dialog.rightDisplay).lineLimit(1).truncationMode(.middle).textSelection(.enabled)
            }
        }
    }

    private var options: some View {
        HStack(spacing: 14) {
            Toggle("Subfolders", isOn: Binding(get: { dialog.recursive }, set: { send(.recursive(on: $0)) }))
                .toggleStyle(.checkbox)
            Picker("Direction", selection: Binding(
                get: { dialog.direction },
                set: { send(.direction(direction: $0)) }
            )) {
                ForEach(dialog.directions, id: \.direction) { option in
                    Text(option.label).tag(option.direction)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .fixedSize()
            Toggle("Show identical", isOn: Binding(get: { dialog.showSame }, set: { send(.showSame(on: $0)) }))
                .toggleStyle(.checkbox)
            Spacer()
            Button {
                send(.compareAgain)
            } label: {
                Label("Compare Again", systemImage: "arrow.clockwise")
            }
            .disabled(!dialog.canCompare)
        }
    }

    @ViewBuilder
    private var content: some View {
        switch dialog.body {
        case .comparing(let text):
            VStack(spacing: 10) {
                ProgressView().controlSize(.small)
                Text(text).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .accessibilityElement(children: .combine)
        case .error(let text):
            ScrollView {
                Text(text)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
            }
            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(.separator))
        case .empty(let text):
            Text(text)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .list:
            VStack(spacing: 0) {
                header
                list
            }
        }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Text("").frame(width: SyncRowView.actionWidth + SyncRowView.iconWidth + 8)
            Text("Name").frame(maxWidth: .infinity, alignment: .leading)
            Text("State").frame(width: SyncRowView.stateWidth, alignment: .leading)
            Text("Left").frame(width: SyncRowView.sideWidth, alignment: .leading)
            Text("Right").frame(width: SyncRowView.sideWidth, alignment: .leading)
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 14)
        .padding(.bottom, 4)
        .accessibilityHidden(true)
    }

    private var list: some View {
        let version = dialog.rowsVersion
        return List(0..<Int(dialog.rowCount), id: \.self) { ix in
            let row = pages.row(ix, version: version) { start, count in
                store.core.syncRows(dialog: id, start: start, count: count)
            }
            SyncRowView(row: row) { send(.cycle(index: $0)) }
        }
        .listStyle(.bordered(alternatesRowBackgrounds: true))
        .environment(\.defaultMinListRowHeight, 24)
        .accessibilityLabel("Items to synchronize")
    }

    private var footer: some View {
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 2) {
                Text(dialog.summary)
                Text(dialog.note).font(.caption).foregroundStyle(.secondary)
            }
            .accessibilityElement(children: .combine)
            Spacer()
            Button("Cancel", role: .cancel) { store.dialog(id, .cancel) }
                .keyboardShortcut(.cancelAction)
            Button("Synchronize") { send(.synchronize) }
                .keyboardShortcut(.defaultAction)
                .disabled(!dialog.canSynchronize)
        }
    }

    private func send(_ input: SyncInput) {
        store.send(.sync(dialog: id, input: input))
    }
}

private struct SyncRowView: View {
    static let actionWidth: CGFloat = 22
    static let iconWidth: CGFloat = 16
    static let stateWidth: CGFloat = 76
    static let sideWidth: CGFloat = 190

    let row: SyncRow?
    let cycle: (UInt32) -> Void

    var body: some View {
        if let row {
            HStack(spacing: 8) {
                Button {
                    cycle(row.planIndex)
                } label: {
                    Image(systemName: row.icon.symbol)
                        .foregroundStyle(row.tint.style)
                        .frame(width: Self.actionWidth)
                }
                .buttonStyle(.borderless)
                .focusable(false)
                .help(row.tooltip)
                .accessibilityLabel(row.tooltip)
                Image(systemName: row.isDir ? "folder.fill" : "doc")
                    .foregroundStyle(row.isDir ? AnyShapeStyle(.blue) : AnyShapeStyle(.secondary))
                    .frame(width: Self.iconWidth)
                    .accessibilityHidden(true)
                Text(row.rel)
                    .truncationMode(.middle)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityLabel(row.isDir ? "\(row.rel), folder" : row.rel)
                Text(row.state)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(width: Self.stateWidth, alignment: .leading)
                side(row.left, newer: row.newer == .left, label: "Left")
                side(row.right, newer: row.newer == .right, label: "Right")
            }
            .lineLimit(1)
            .font(.callout)
        } else {
            Color.clear.frame(height: 16)
        }
    }

    private func side(_ text: String?, newer: Bool, label: String) -> some View {
        Text(text ?? "")
            .font(.caption)
            .foregroundStyle(newer ? AnyShapeStyle(.green) : AnyShapeStyle(.secondary))
            .truncationMode(.tail)
            .frame(width: Self.sideWidth, alignment: .leading)
            .help(text ?? "")
            .accessibilityLabel(text.map { "\(label): \($0)\(newer ? ", newer" : "")" } ?? "\(label): none")
    }
}

extension SyncActionIcon {
    var symbol: String {
        switch self {
        case .arrowRight: "arrow.right"
        case .arrowLeft: "arrow.left"
        case .equal: "equal"
        case .triangleAlert: "exclamationmark.triangle.fill"
        case .minus: "minus"
        }
    }
}

extension SyncTint {
    var style: AnyShapeStyle {
        switch self {
        case .accent: AnyShapeStyle(.tint)
        case .muted: AnyShapeStyle(.secondary)
        case .warning: AnyShapeStyle(.orange)
        }
    }
}
