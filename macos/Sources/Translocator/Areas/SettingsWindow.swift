import SwiftUI
import TLCore

/// STUB — the settings window (⌘, through the core's `OpenSettings`).
///
/// Must become the GPUI shell's `crates/translocator/src/settings_window.rs`
/// as a native tabbed Settings scene: General (restore folders, hidden
/// files, folders first, sort, icons, confirm trash, font size, row
/// height, date format, terminal, Reset), Keys (the key table grouped as
/// `KeyCommand.group`, recording keys, `validateKeys`, `Core.keyConflicts`,
/// reset per command) and Appearance (System / Light / Dark). Every change
/// is an `Input.settings(edit:)`; values come from `Snapshot.settings`.
struct SettingsWindow: View {
    @Environment(AppStore.self) private var store

    var body: some View {
        let general = store.snapshot.settings.general
        Form {
            Toggle("Show hidden files", isOn: Binding(
                get: { general.showHidden },
                set: { store.send(.settings(edit: .showHidden(on: $0))) }
            ))
            Picker("Appearance", selection: Binding(
                get: { store.snapshot.settings.appearance.mode },
                set: { store.send(.settings(edit: .appearanceMode(mode: $0))) }
            )) {
                Text("System").tag(AppearanceMode.system)
                Text("Light").tag(AppearanceMode.light)
                Text("Dark").tag(AppearanceMode.dark)
            }
            Text("The full settings are not available in this build yet.")
                .foregroundStyle(.secondary)
        }
        .formStyle(.grouped)
        .frame(width: 460)
        .padding()
    }
}
