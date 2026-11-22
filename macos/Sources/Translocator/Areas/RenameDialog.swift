import SwiftUI
import TLCore

/// STUB — the multi-rename dialog (`DialogKindSnapshot.rename`).
///
/// Must become the GPUI shell's `crates/translocator/src/rename.rs`: the
/// name and extension masks with their placeholder buttons
/// (`RenameInput.insert`), find / replace with the regex and match-case
/// toggles, the counter (start, step, digits), the case picker, and the
/// preview list ("old → new" with problems), fetched by range from `Core`
/// as the rows are (see `crates/tl-ffi/src/rename.rs`). Edits go out as
/// `Input.rename(dialog:input:)`; Rename / Enter is `RenameInput.apply`.
struct RenameDialog: View {
    let id: UInt64
    let dialog: RenameSnapshot

    var body: some View {
        AreaPlaceholder(id: id, title: dialog.title, detail: "Multi-rename is not available in this build yet.")
    }
}
