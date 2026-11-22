//! AREA: the properties dialog (`DialogKind::Properties`).
//!
//! Owns [`PropertiesSnapshot`] (carried by `DialogKindSnapshot::Properties`)
//! and [`PropertiesInput`], the full mirror of
//! `tl_app::properties::PropertiesInput`, sent as
//! `Input::Properties { dialog, input }`.
//!
//! STUB: the snapshot carries the title only. The next step adds the
//! details (dates through `crate::format::date_long`), the permission grid
//! (`tl_app::properties::ROWS` / `SPECIALS`), owner and group, and the
//! recursive box. The Swift side is
//! `macos/Sources/Translocator/Areas/PropertiesDialog.swift`.

use tl_app::properties;

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct PropertiesSnapshot {
    /// The entry's name, or "{n} items".
    pub title: String,
}

pub(crate) fn snapshot(dialog: &properties::PropertiesDialog) -> PropertiesSnapshot {
    PropertiesSnapshot {
        title: dialog.title.clone(),
    }
}

#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum PropertiesInput {
    /// A permission checkbox: one bit of the mode.
    Bit { bit: u32, on: bool },
    Owner { name: String },
    Group { name: String },
    /// The "Apply to enclosed items" box.
    Recursive { on: bool },
    /// The Apply button, or Enter.
    Apply,
}

impl From<PropertiesInput> for properties::PropertiesInput {
    fn from(input: PropertiesInput) -> Self {
        use properties::PropertiesInput as P;
        match input {
            PropertiesInput::Bit { bit, on } => P::Bit { bit, on },
            PropertiesInput::Owner { name } => P::Owner(name),
            PropertiesInput::Group { name } => P::Group(name),
            PropertiesInput::Recursive { on } => P::Recursive(on),
            PropertiesInput::Apply => P::Apply,
        }
    }
}
