import SwiftUI
import TLCore

/// The properties dialog (`DialogKindSnapshot.properties`), after the GPUI
/// shell's `crates/translocator/src/properties.rs`.
///
/// The details, the permission grid (owner / group / others × read /
/// write / execute, then setuid, setgid, sticky) as checkboxes sending
/// `PropertiesInput.bit`, the mode in octal and symbolic form, owner and
/// group (when the file system can chown), "Apply to enclosed items" (when
/// a folder is selected), and Apply (or Return), `PropertiesInput.apply`.
struct PropertiesDialog: View {
    let id: UInt64
    let dialog: PropertiesSnapshot
    @Environment(AppStore.self) private var store

    var body: some View {
        DialogFrame(title: dialog.title, width: 520) {
            details
            Divider()
            permissions
            if dialog.canChown {
                ownership
            }
            if dialog.anyDir {
                Toggle(dialog.recursiveLabel, isOn: Binding(
                    get: { dialog.recursive },
                    set: { send(.recursive(on: $0)) }
                ))
                .toggleStyle(.checkbox)
            }
            if let note = dialog.elevatedNote {
                Text(note).font(.caption).foregroundStyle(.red)
            }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { store.dialog(id, .cancel) }
                    .keyboardShortcut(.cancelAction)
                Button("Apply") { send(.apply) }
                    .keyboardShortcut(.defaultAction)
            }
        }
    }

    private var details: some View {
        Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: 8, verticalSpacing: 4) {
            ForEach(dialog.details, id: \.self) { detail in
                GridRow {
                    Text(detail.label)
                        .foregroundStyle(.secondary)
                        .frame(width: 90, alignment: .leading)
                    Text(detail.value)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .accessibilityElement(children: .combine)
            }
        }
    }

    private var permissions: some View {
        VStack(alignment: .leading, spacing: 8) {
            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 6) {
                GridRow {
                    Color.clear.frame(width: 70, height: 1)
                    ForEach(dialog.columns, id: \.self) { column in
                        Text(column)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .frame(width: 50, alignment: .leading)
                    }
                }
                .accessibilityHidden(true)
                ForEach(dialog.rows, id: \.label) { row in
                    GridRow {
                        Text(row.label).frame(width: 70, alignment: .leading)
                        ForEach(row.bits, id: \.bit) { bit in
                            checkbox(bit)
                                .labelsHidden()
                                .accessibilityLabel("\(row.label) \(bit.label.lowercased())")
                                .frame(width: 50, alignment: .leading)
                        }
                    }
                }
            }
            HStack(spacing: 16) {
                ForEach(dialog.specials, id: \.bit) { bit in
                    checkbox(bit)
                }
            }
            Text(dialog.modeText)
                .font(.system(.body, design: .monospaced))
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
                .accessibilityLabel("Mode \(dialog.modeText)")
        }
        .disabled(!dialog.canChmod)
    }

    private func checkbox(_ bit: PermissionBit) -> some View {
        Toggle(bit.label, isOn: Binding(
            get: { bit.on },
            set: { send(.bit(bit: bit.bit, on: $0)) }
        ))
        .toggleStyle(.checkbox)
    }

    private var ownership: some View {
        HStack(spacing: 8) {
            Text("Owner").frame(width: 50, alignment: .leading)
            CoreTextField(placeholder: "", core: dialog.owner) { send(.owner(name: $0)) }
                .accessibilityLabel("Owner")
                .frame(width: 150)
            Text("Group").frame(width: 50, alignment: .leading).padding(.leading, 8)
            CoreTextField(placeholder: "", core: dialog.group) { send(.group(name: $0)) }
                .accessibilityLabel("Group")
                .frame(width: 150)
        }
    }

    private func send(_ input: PropertiesInput) {
        store.send(.properties(dialog: id, input: input))
    }
}
