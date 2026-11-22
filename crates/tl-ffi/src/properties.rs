//! AREA: the properties dialog (`DialogKind::Properties`).
//!
//! Owns [`PropertiesSnapshot`] (carried by `DialogKindSnapshot::Properties`)
//! and [`PropertiesInput`], the full mirror of
//! `tl_app::properties::PropertiesInput`, sent as
//! `Input::Properties { dialog, input }`.
//!
//! The snapshot carries the details (the modification time written here,
//! in local time, through [`crate::format::date_long`]), the permission
//! grid (`tl_app::properties::ROWS` / `SPECIALS`) with each box's state,
//! owner and group, and the recursive box. The Swift side is
//! `macos/Sources/Translocator/Areas/PropertiesDialog.swift`.

use tl_app::properties::{self, DetailValue};

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct PropertiesSnapshot {
    /// The entry's name, or "{n} items".
    pub title: String,
    /// Label and value lines at the top.
    pub details: Vec<PropertyDetail>,
    /// The grid's column headings: "Read", "Write", "Execute".
    pub columns: Vec<String>,
    /// Owner, Group, Others, each with its read, write and execute boxes.
    pub rows: Vec<PermissionRow>,
    /// setuid, setgid, sticky, under the grid.
    pub specials: Vec<PermissionBit>,
    /// The permission bits as edited (`& 0o7777`).
    pub mode: u32,
    /// "0644  -rw-r--r--" (monospaced, under the grid).
    pub mode_text: String,
    pub owner: String,
    pub group: String,
    /// The boxes can be changed (disabled otherwise).
    pub can_chmod: bool,
    /// The owner and group fields are shown.
    pub can_chown: bool,
    /// The recursive box is shown (a folder is selected).
    pub any_dir: bool,
    pub recursive: bool,
    /// "Apply to enclosed items (folders keep search permission)".
    pub recursive_label: String,
    /// "Changes run as root (sudo)." (danger colour).
    pub elevated_note: Option<String>,
}

/// One line of the details.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct PropertyDetail {
    /// "Location", "Kind", "Size", "Modified", "Points to", "Files".
    pub label: String,
    pub value: String,
}

/// A row of the permission grid.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct PermissionRow {
    /// "Owner", "Group", "Others".
    pub label: String,
    /// Read, write, execute.
    pub bits: Vec<PermissionBit>,
}

/// One permission box: `PropertiesInput::Bit { bit, on }` when clicked.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct PermissionBit {
    pub bit: u32,
    /// The column heading in the grid ("Read"), the name for a special
    /// bit ("setuid").
    pub label: String,
    pub on: bool,
}

pub(crate) fn snapshot(dialog: &properties::PropertiesDialog) -> PropertiesSnapshot {
    let bit = |bit: u32, label: &str| PermissionBit {
        bit,
        label: label.to_string(),
        on: dialog.has(bit),
    };
    PropertiesSnapshot {
        title: dialog.title.clone(),
        details: dialog
            .details
            .iter()
            .map(|d| PropertyDetail {
                label: d.label.to_string(),
                value: match &d.value {
                    DetailValue::Text(text) => text.clone(),
                    DetailValue::Date(mtime) => crate::format::date_long(*mtime),
                },
            })
            .collect(),
        columns: properties::COLUMNS
            .iter()
            .map(|c| (*c).to_string())
            .collect(),
        rows: properties::ROWS
            .iter()
            .map(|(label, bits)| PermissionRow {
                label: (*label).to_string(),
                bits: bits
                    .iter()
                    .zip(properties::COLUMNS)
                    .map(|(b, column)| bit(*b, column))
                    .collect(),
            })
            .collect(),
        specials: properties::SPECIALS
            .iter()
            .map(|(b, label)| bit(*b, label))
            .collect(),
        mode: dialog.mode,
        mode_text: dialog.mode_text(),
        owner: dialog.owner.clone(),
        group: dialog.group.clone(),
        can_chmod: dialog.can_chmod,
        can_chown: dialog.can_chown,
        any_dir: dialog.any_dir,
        recursive: dialog.recursive,
        recursive_label: properties::RECURSIVE_LABEL.to_string(),
        elevated_note: dialog.elevated_note().map(str::to_string),
    }
}

#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum PropertiesInput {
    /// A permission checkbox: one bit of the mode.
    Bit {
        bit: u32,
        on: bool,
    },
    Owner {
        name: String,
    },
    Group {
        name: String,
    },
    /// The "Apply to enclosed items" box.
    Recursive {
        on: bool,
    },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{Command, Input};
    use crate::rename::tests::Fixture;
    use crate::snapshot::{DialogKindSnapshot, Snapshot};

    fn properties_dialog(snap: &Snapshot) -> (u64, PropertiesSnapshot) {
        snap.dialogs
            .iter()
            .find_map(|d| match &d.kind {
                DialogKindSnapshot::Properties { dialog } => Some((d.id, dialog.clone())),
                _ => None,
            })
            .expect("a properties dialog")
    }

    #[test]
    fn the_properties_dialog_mirrors_the_core() {
        let f = Fixture::start();
        // "..", "dir", "a.txt": the file.
        f.command(Command::CursorDown);
        f.command(Command::CursorDown);
        let snap = f.command(Command::Properties);
        let (id, d) = properties_dialog(&snap);
        assert_eq!(d.title, "a.txt");
        let labels: Vec<&str> = d.details.iter().map(|d| d.label.as_str()).collect();
        assert_eq!(labels, ["Location", "Kind", "Size", "Modified"]);
        assert_eq!(d.details[1].value, "file");
        assert_eq!(d.details[2].value, "3 B (3 bytes)");
        assert!(
            d.details[3].value.starts_with("today "),
            "{}",
            d.details[3].value
        );
        assert_eq!(d.columns, ["Read", "Write", "Execute"]);
        assert_eq!(
            d.rows.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
            ["Owner", "Group", "Others"]
        );
        let owner_read = &d.rows[0].bits[0];
        assert_eq!(
            (owner_read.bit, owner_read.label.as_str(), owner_read.on),
            (0o400, "Read", true)
        );
        assert_eq!(
            d.specials
                .iter()
                .map(|b| b.label.as_str())
                .collect::<Vec<_>>(),
            ["setuid", "setgid", "sticky"]
        );
        assert!(d.specials.iter().all(|b| !b.on));
        assert_eq!(
            d.mode_text,
            format!("{:04o}  -{}", d.mode, &d.mode_text[7..])
        );
        assert!(d.can_chmod && !d.any_dir && !d.recursive);
        assert!(!d.owner.is_empty());
        assert!(d.elevated_note.is_none());
        assert!(d.recursive_label.starts_with("Apply to enclosed items"));

        let snap = f.send(Input::Properties {
            dialog: id,
            input: PropertiesInput::Bit {
                bit: 0o100,
                on: true,
            },
        });
        let (_, after) = properties_dialog(&snap);
        assert!(after.rows[0].bits[2].on);
        assert_eq!(after.mode, d.mode | 0o100);
        assert!(after.mode_text.contains("-rwx"), "{}", after.mode_text);

        f.send(Input::Dialog {
            id,
            input: crate::input::DialogInput::Cancel,
        });
        f.wait(|s| s.dialogs.is_empty());
        f.rt.shutdown();
    }
}
