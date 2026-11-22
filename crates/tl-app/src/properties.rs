//! The properties dialog (⌘I): details, permissions (chmod) and ownership
//! (chown) of the selected items.

use std::sync::Arc;

use tl_model::EntryKind;
use tl_model::entry::mode_string;
use tl_model::util::{group_digits, human_size};

use crate::Out;
use crate::dialog::{self, DialogKind};
use crate::ids::DialogId;
use crate::model::Model;
use crate::ops;
use crate::pane::Selection;

/// The permission grid's column headings.
pub const COLUMNS: [&str; 3] = ["Read", "Write", "Execute"];

/// The permission grid's rows: label, and the read, write and execute bits.
pub const ROWS: [(&str, [u32; 3]); 3] = [
    ("Owner", [0o400, 0o200, 0o100]),
    ("Group", [0o040, 0o020, 0o010]),
    ("Others", [0o004, 0o002, 0o001]),
];

/// The special bits under the grid.
pub const SPECIALS: [(u32, &str); 3] = [(0o4000, "setuid"), (0o2000, "setgid"), (0o1000, "sticky")];

pub const RECURSIVE_LABEL: &str = "Apply to enclosed items (folders keep search permission)";

/// One line of the details at the top.
#[derive(Clone, Debug, PartialEq)]
pub struct Detail {
    pub label: &'static str,
    pub value: DetailValue,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DetailValue {
    Text(String),
    /// A modification time, written by the shell in the long form ("today
    /// 14:05", "Mar 9 10:00", "2023-03-09 10:00"; empty when unknown).
    Date(Option<i64>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PropertiesDialog {
    /// The entry's name, or "{n} items".
    pub title: String,
    pub sel: Arc<Selection>,
    pub details: Vec<Detail>,
    /// The first entry's kind, for the mode string.
    pub kind: EntryKind,
    /// The first entry's permission bits when the dialog opened.
    pub original_mode: u32,
    /// The permission bits as edited (`& 0o7777`).
    pub mode: u32,
    /// The checkboxes can be changed.
    pub can_chmod: bool,
    /// The owner and group fields are shown.
    pub can_chown: bool,
    pub owner: String,
    pub group: String,
    pub original_owner: String,
    pub original_group: String,
    /// The recursive box is shown (a folder is selected).
    pub any_dir: bool,
    pub recursive: bool,
}

impl PropertiesDialog {
    pub fn has(&self, bit: u32) -> bool {
        self.mode & bit != 0
    }

    /// "0644  -rw-r--r--", under the grid.
    pub fn mode_text(&self) -> String {
        format!("{:04o}  {}", self.mode, mode_string(self.kind, self.mode))
    }

    /// The note shown when the changes run with sudo.
    pub fn elevated_note(&self) -> Option<&'static str> {
        self.sel.elevated.then_some("Changes run as root (sudo).")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PropertiesInput {
    /// A permission checkbox (one of the [`ROWS`] or [`SPECIALS`] bits).
    Bit {
        bit: u32,
        on: bool,
    },
    Owner(String),
    Group(String),
    /// The "Apply to enclosed items" box.
    Recursive(bool),
    /// The Apply button, or Enter.
    Apply,
}

pub fn open(model: &mut Model, sel: Selection, _out: &mut Out) {
    let Some(first) = sel.entries.first().cloned() else {
        return;
    };
    let caps = model.conns.caps(sel.conn);
    let location = model.conns.full_path(sel.conn, &sel.dir);
    let text = |label, value: String| Detail {
        label,
        value: DetailValue::Text(value),
    };
    let mut details = vec![text("Location", location)];
    let title = if sel.entries.len() == 1 {
        let kind = if first.is_symlink() {
            "symbolic link"
        } else if first.is_dir() {
            "folder"
        } else {
            "file"
        };
        details.push(text("Kind", kind.into()));
        if !first.is_dir() {
            details.push(text(
                "Size",
                format!(
                    "{} ({} bytes)",
                    human_size(first.size),
                    group_digits(first.size)
                ),
            ));
        }
        details.push(Detail {
            label: "Modified",
            value: DetailValue::Date(first.mtime),
        });
        if let Some(target) = &first.link_target {
            details.push(text(
                "Points to",
                String::from_utf8_lossy(target).into_owned(),
            ));
        }
        first.name_lossy()
    } else {
        let bytes: u64 = sel
            .entries
            .iter()
            .filter(|e| !e.is_dir())
            .map(|e| e.size)
            .sum();
        details.push(text(
            "Files",
            format!("{} selected, {}", sel.entries.len(), human_size(bytes)),
        ));
        format!("{} items", sel.entries.len())
    };
    let owner = first.user.clone().unwrap_or_default();
    let group = first.group.clone().unwrap_or_default();
    let mode = first.mode & 0o7777;
    let dialog = PropertiesDialog {
        title,
        details,
        kind: first.kind,
        original_mode: mode,
        mode,
        can_chmod: caps.chmod,
        can_chown: caps.chown,
        owner: owner.clone(),
        group: group.clone(),
        original_owner: owner,
        original_group: group,
        any_dir: sel.entries.iter().any(|e| e.is_dir()),
        recursive: false,
        sel: Arc::new(sel),
    };
    dialog::open(model, DialogKind::Properties(Box::new(dialog)));
}

pub fn input(model: &mut Model, id: DialogId, input: PropertiesInput, out: &mut Out) {
    let Some(DialogKind::Properties(d)) = model.dialog_mut(id).map(|d| &mut d.kind) else {
        return;
    };
    match input {
        PropertiesInput::Bit { bit, on } => {
            if d.can_chmod {
                let bit = bit & 0o7777;
                d.mode = if on { d.mode | bit } else { d.mode & !bit };
            }
        }
        PropertiesInput::Owner(text) => {
            if d.can_chown {
                d.owner = text;
            }
        }
        PropertiesInput::Group(text) => {
            if d.can_chown {
                d.group = text;
            }
        }
        PropertiesInput::Recursive(on) => {
            if d.any_dir {
                d.recursive = on;
            }
        }
        PropertiesInput::Apply => {
            let d = (**d).clone();
            model.close_dialog(id);
            apply(model, d, out);
        }
    }
}

/// Sends what changed: the mode when it changed or goes to enclosed items,
/// the owner and group when they are filled in and changed.
fn apply(model: &mut Model, d: PropertiesDialog, out: &mut Out) {
    let owner = d.owner.trim().to_string();
    let group = d.group.trim().to_string();
    let mode = (d.mode != d.original_mode || d.recursive).then_some(d.mode);
    let owner = (owner != d.original_owner && !owner.is_empty()).then_some(owner);
    let group = (group != d.original_group && !group.is_empty()).then_some(group);
    if mode.is_none() && owner.is_none() && group.is_none() {
        return;
    }
    let sel = Arc::try_unwrap(d.sel).unwrap_or_else(|sel| (*sel).clone());
    ops::apply_attributes(model, sel, mode, owner, group, d.recursive, out);
}

#[cfg(test)]
mod tests {
    use crux_core::Command;
    use tl_model::{Capabilities, Entry, VPath};

    use super::*;
    use crate::capabilities::{JobOp, Work};
    use crate::conn::ConnInfo;
    use crate::dialog::DialogInput;
    use crate::ids::{ConnId, LOCAL};
    use crate::{Cmd, Effect, Event, model};

    fn model(chmod: bool, chown: bool) -> Model {
        let mut model = Model::default();
        for (id, plugin, label) in [(LOCAL, "local", "local"), (ConnId(3), "ssh", "root@db")] {
            model.conns.insert(ConnInfo {
                id,
                plugin: plugin.into(),
                label: label.into(),
                location_key: plugin.into(),
                caps: Capabilities {
                    chmod,
                    chown,
                    ..Capabilities::default()
                },
                home: VPath::new("/home/me"),
                bookmark: None,
                host: None,
                server_info: None,
                local_home: Some("/home/me".into()),
                machine: None,
            });
        }
        model
    }

    fn entry(name: &str, kind: EntryKind, mode: u32) -> Entry {
        let mut e = Entry::new(name, kind);
        e.mode = mode;
        e.size = 1536;
        e.mtime = Some(1_700_000_000);
        e.user = Some("me".into());
        e.group = Some("staff".into());
        e
    }

    fn opened(model: &mut Model, conn: ConnId, entries: Vec<Entry>) -> DialogId {
        let sel = Selection {
            conn,
            dir: VPath::new("/etc"),
            entries,
            elevated: false,
        };
        open(model, sel, &mut Out::default());
        model.dialogs.last().unwrap().id
    }

    fn dialog(model: &Model, id: DialogId) -> &PropertiesDialog {
        match &model.dialog(id).unwrap().kind {
            DialogKind::Properties(d) => d,
            _ => panic!("not a properties dialog"),
        }
    }

    fn send(model: &mut Model, id: DialogId, input: PropertiesInput) -> Vec<Work> {
        let mut out = Out::default();
        model::update(Event::Properties(id, input), model, &mut out);
        let mut cmd: Cmd = Command::all(out.cmds);
        cmd.effects()
            .filter_map(|e| match e {
                Effect::Job(req) => match req.operation {
                    JobOp::Start { work, .. } => Some(work),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    #[test]
    fn one_file_shows_its_details() {
        let mut model = model(true, true);
        let mut link = entry("hosts", EntryKind::Symlink, 0o100644);
        link.link_target = Some(b"/private/etc/hosts".to_vec());
        let id = opened(&mut model, ConnId(3), vec![link]);
        let d = dialog(&model, id);
        assert_eq!(d.title, "hosts");
        let text = |label: &str| {
            d.details
                .iter()
                .find(|x| x.label == label)
                .map(|x| x.value.clone())
        };
        assert_eq!(
            text("Location"),
            Some(DetailValue::Text("root@db:/etc".into()))
        );
        assert_eq!(
            text("Kind"),
            Some(DetailValue::Text("symbolic link".into()))
        );
        assert_eq!(
            text("Size"),
            Some(DetailValue::Text("1.5 KB (1\u{2009}536 bytes)".into()))
        );
        assert_eq!(
            text("Modified"),
            Some(DetailValue::Date(Some(1_700_000_000)))
        );
        assert_eq!(
            text("Points to"),
            Some(DetailValue::Text("/private/etc/hosts".into()))
        );
        assert_eq!(d.mode, 0o644);
        assert_eq!(d.mode_text(), "0644  lrw-r--r--");
        assert!(!d.any_dir);
    }

    #[test]
    fn several_items_show_their_count_and_size() {
        let mut model = model(true, true);
        let id = opened(
            &mut model,
            LOCAL,
            vec![
                entry("a", EntryKind::File, 0o644),
                entry("d", EntryKind::Dir, 0o755),
            ],
        );
        let d = dialog(&model, id);
        assert_eq!(d.title, "2 items");
        let labels: Vec<&str> = d.details.iter().map(|x| x.label).collect();
        assert_eq!(labels, ["Location", "Files"]);
        assert_eq!(
            d.details[1].value,
            DetailValue::Text("2 selected, 1.5 KB".into())
        );
        assert!(d.any_dir);
    }

    #[test]
    fn nothing_changed_just_closes() {
        let mut model = model(true, true);
        let id = opened(&mut model, LOCAL, vec![entry("a", EntryKind::File, 0o644)]);
        send(&mut model, id, PropertiesInput::Owner("  me ".into()));
        send(&mut model, id, PropertiesInput::Group("".into()));
        let work = send(&mut model, id, PropertiesInput::Apply);
        assert!(model.dialog(id).is_none());
        assert!(work.is_empty());
    }

    #[test]
    fn apply_sends_what_changed() {
        let mut model = model(true, true);
        let id = opened(&mut model, LOCAL, vec![entry("a", EntryKind::File, 0o644)]);
        send(
            &mut model,
            id,
            PropertiesInput::Bit {
                bit: 0o100,
                on: true,
            },
        );
        send(
            &mut model,
            id,
            PropertiesInput::Bit {
                bit: 0o004,
                on: false,
            },
        );
        send(
            &mut model,
            id,
            PropertiesInput::Bit {
                bit: 0o4000,
                on: true,
            },
        );
        assert_eq!(dialog(&model, id).mode_text(), "4740  -rwsr-----");
        send(&mut model, id, PropertiesInput::Group(" wheel ".into()));
        let work = send(&mut model, id, PropertiesInput::Apply);
        let [
            Work::Attributes {
                mode,
                owner,
                group,
                recursive,
                paths,
                ..
            },
        ] = work.as_slice()
        else {
            panic!("expected one attributes job")
        };
        assert_eq!(*mode, Some(0o4740));
        assert_eq!(*owner, None);
        assert_eq!(group.as_deref(), Some("wheel"));
        assert!(!recursive);
        assert_eq!(paths, &[VPath::new("/etc/a")]);
    }

    #[test]
    fn recursive_sends_the_mode_even_unchanged() {
        let mut model = model(true, true);
        let id = opened(&mut model, LOCAL, vec![entry("d", EntryKind::Dir, 0o755)]);
        send(&mut model, id, PropertiesInput::Recursive(true));
        // Enter applies too.
        let mut out = Out::default();
        model::update(Event::Dialog(id, DialogInput::Submit), &mut model, &mut out);
        let mut cmd: Cmd = Command::all(out.cmds);
        let started = cmd.effects().any(|e| {
            matches!(&e, Effect::Job(req) if matches!(&req.operation, JobOp::Start { work: Work::Attributes { mode: Some(0o755), recursive: true, .. }, .. }))
        });
        assert!(started);
    }

    #[test]
    fn what_the_file_system_cannot_do_cannot_be_edited() {
        let mut model = model(false, false);
        let id = opened(&mut model, LOCAL, vec![entry("a", EntryKind::File, 0o644)]);
        send(
            &mut model,
            id,
            PropertiesInput::Bit {
                bit: 0o100,
                on: true,
            },
        );
        send(&mut model, id, PropertiesInput::Owner("root".into()));
        send(&mut model, id, PropertiesInput::Recursive(true));
        let d = dialog(&model, id);
        assert!(!d.can_chmod && !d.can_chown);
        assert_eq!(
            (d.mode, d.owner.as_str(), d.recursive),
            (0o644, "me", false)
        );
        assert!(send(&mut model, id, PropertiesInput::Apply).is_empty());
    }
}
