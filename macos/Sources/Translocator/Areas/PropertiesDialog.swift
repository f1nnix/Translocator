import SwiftUI
import TLCore

/// STUB — the properties dialog (`DialogKindSnapshot.properties`).
///
/// Must become the GPUI shell's `crates/translocator/src/properties.rs`:
/// the details list, the permission grid (owner / group / others ×
/// read / write / execute, plus setuid, setgid, sticky) as checkboxes
/// sending `PropertiesInput.bit`, owner and group fields, "Apply to
/// enclosed items", and Apply (`PropertiesInput.apply`). See
/// `crates/tl-ffi/src/properties.rs` for what the snapshot must carry.
struct PropertiesDialog: View {
    let id: UInt64
    let dialog: PropertiesSnapshot

    var body: some View {
        AreaPlaceholder(id: id, title: dialog.title, detail: "Properties are not available in this build yet.")
    }
}
