import SwiftUI
import TLCore

/// STUB — the synchronize-directories dialog (`DialogKindSnapshot.sync`).
///
/// Must become the GPUI shell's `crates/translocator/src/sync.rs`: the two
/// folders, the Subfolders / Show identical toggles, the direction
/// buttons, the comparison state (spinner, error, empty), the plan rows
/// with their action buttons (`SyncInput.cycle`), the totals, and
/// Compare Again / Synchronize. Rows come by range from `Core` (see
/// `crates/tl-ffi/src/sync.rs`). Closing it any other way is
/// `DialogInput.cancel`, which stops a running comparison.
struct SyncDialog: View {
    let id: UInt64
    let dialog: SyncSnapshot

    var body: some View {
        AreaPlaceholder(id: id, title: dialog.title,
                        detail: "Left \(dialog.leftDisplay)\nRight \(dialog.rightDisplay)\n\nSynchronizing is not available in this build yet.")
    }
}
