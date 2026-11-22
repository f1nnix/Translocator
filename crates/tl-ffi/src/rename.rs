//! AREA: the multi-rename dialog (`DialogKind::Rename` in the core).
//!
//! Owns [`RenameSnapshot`] (carried by `DialogKindSnapshot::Rename`) and
//! [`RenameInput`], the full mirror of `tl_app::rename::RenameInput`, sent
//! as `Input::Rename { dialog, input }`.
//!
//! The snapshot carries every field's text, the toggles, and the counts and
//! notes the core computes. The preview is a long list, so it follows the
//! rows pattern: `preview_count` and a `preview_version` (moved by a
//! [`Tracker`] keyed by the preview `Arc`'s address) in the snapshot, the
//! rows by range from [`Core::rename_preview`]. The Swift side is
//! `macos/Sources/Translocator/Areas/RenameDialog.swift`.

use std::collections::HashMap;

use tl_app::dialog::{Dialog, DialogKind};
use tl_app::rename;
use tl_app::tl_model::multirename;

use crate::Core;
use crate::types::usize32;
use crate::versions::{Tracker, addr};

/// What the multi-rename dialog shows.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct RenameSnapshot {
    /// "Multi-Rename “a.txt”" / "Multi-Rename 3 items".
    pub title: String,
    /// The text fields, in [`RenameField`] order (name, extension, find,
    /// replace, counter start, step, digits).
    pub fields: Vec<RenameFieldText>,
    pub regex: bool,
    pub match_case: bool,
    pub case: CaseChange,
    /// The case buttons, in order.
    pub cases: Vec<CaseOption>,
    /// The placeholder buttons beside the name mask.
    pub name_tokens: Vec<String>,
    /// The placeholder buttons beside the extension mask.
    pub ext_tokens: Vec<String>,
    /// The line under the masks.
    pub help: String,
    /// Rows that get a new name and have no problem.
    pub to_rename: u32,
    /// Rows that keep their name and have no problem.
    pub unchanged: u32,
    /// Rows with a problem.
    pub problems: u32,
    /// "2 to rename · 1 unchanged".
    pub summary: String,
    /// "1 cannot be renamed as shown" (danger colour).
    pub problems_text: Option<String>,
    /// "Runs as root (sudo)." (danger colour).
    pub elevated_note: Option<String>,
    /// The Rename button (and Return) can run.
    pub can_apply: bool,
    /// Why the masks cannot be used: shown (danger colour) in place of the
    /// preview list.
    pub error: Option<String>,
    /// Rows in the preview (0 when there is an error).
    pub preview_count: u32,
    /// Moves exactly when the preview rows changed: drop cached rows.
    pub preview_version: u64,
}

impl RenameSnapshot {
    /// The text of `field`.
    pub fn text(&self, field: RenameField) -> &str {
        self.fields
            .iter()
            .find(|f| f.field == field)
            .map_or("", |f| f.text.as_str())
    }
}

/// One text field of the dialog.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct RenameFieldText {
    pub field: RenameField,
    pub text: String,
    /// What the empty field shows.
    pub placeholder: String,
}

/// A case button.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct CaseOption {
    pub case: CaseChange,
    /// "Unchanged", "lower", "UPPER", "First", "Title Case".
    pub label: String,
}

/// One row of the preview: "old → new", and why it cannot be renamed.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct RenamePreviewRow {
    /// The row's position in the preview.
    pub index: u32,
    pub old: String,
    pub new: String,
    /// The new name is drawn in the danger colour, with this beside it.
    pub problem: Option<String>,
    /// The name changes: the new name in the primary colour (muted when
    /// it stays the same).
    pub changed: bool,
}

pub(crate) fn snapshot(dialog: &rename::RenameDialog, preview_version: u64) -> RenameSnapshot {
    RenameSnapshot {
        title: dialog.title.clone(),
        fields: rename::Field::ALL
            .iter()
            .map(|field| RenameFieldText {
                field: (*field).into(),
                text: dialog.text(*field).to_string(),
                placeholder: field.placeholder().to_string(),
            })
            .collect(),
        regex: dialog.regex,
        match_case: dialog.match_case,
        case: dialog.case.into(),
        cases: rename::CASES
            .iter()
            .map(|(case, label)| CaseOption {
                case: (*case).into(),
                label: (*label).to_string(),
            })
            .collect(),
        name_tokens: rename::NAME_TOKENS
            .iter()
            .map(|t| (*t).to_string())
            .collect(),
        ext_tokens: rename::EXT_TOKENS
            .iter()
            .map(|t| (*t).to_string())
            .collect(),
        help: rename::HELP.to_string(),
        to_rename: usize32(dialog.to_rename),
        unchanged: usize32(dialog.unchanged),
        problems: usize32(dialog.problems),
        summary: dialog.summary(),
        problems_text: dialog.problems_text(),
        elevated_note: dialog.elevated_note().map(str::to_string),
        can_apply: dialog.can_apply(),
        error: dialog.error().map(str::to_string),
        preview_count: usize32(dialog.rows().len()),
        preview_version,
    }
}

/// The preview counters, one per open rename dialog; kept by `Core`
/// between snapshots.
#[derive(Default)]
pub(crate) struct Previews(HashMap<u64, Tracker<usize>>);

impl Previews {
    /// The snapshot of dialog `id`, with its preview's version.
    pub(crate) fn snapshot(&mut self, id: u64, dialog: &rename::RenameDialog) -> RenameSnapshot {
        let version = self.0.entry(id).or_default().version(addr(&dialog.preview));
        snapshot(dialog, version)
    }

    /// Forgets the dialogs that are gone.
    pub(crate) fn retain(&mut self, dialogs: &[Dialog]) {
        self.0.retain(|id, _| dialogs.iter().any(|d| d.id.0 == *id));
    }
}

/// Preview rows `start..start + count` of `dialog` (fewer at the end).
pub(crate) fn preview_rows(
    dialog: &rename::RenameDialog,
    start: u32,
    count: u32,
) -> Vec<RenamePreviewRow> {
    let rows = dialog.rows();
    let start = (start as usize).min(rows.len());
    let end = start.saturating_add(count as usize).min(rows.len());
    rows[start..end]
        .iter()
        .enumerate()
        .map(|(offset, p)| RenamePreviewRow {
            index: usize32(start + offset),
            old: String::from_utf8_lossy(&p.old).into_owned(),
            new: String::from_utf8_lossy(&p.new).into_owned(),
            problem: p.problem.clone(),
            changed: p.changed(),
        })
        .collect()
}

#[uniffi::export]
impl Core {
    /// Preview rows `start..start + count` of a multi-rename dialog (fetch
    /// again when `RenameSnapshot.preview_version` moves).
    pub fn rename_preview(&self, dialog: u64, start: u32, count: u32) -> Vec<RenamePreviewRow> {
        self.view()
            .dialogs
            .iter()
            .find_map(|d| match &d.kind {
                DialogKind::Rename(r) if d.id.0 == dialog => Some(preview_rows(r, start, count)),
                _ => None,
            })
            .unwrap_or_default()
    }
}

/// The dialog's text fields.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RenameField {
    Name,
    Ext,
    Find,
    Replace,
    Start,
    Step,
    Digits,
}

impl From<rename::Field> for RenameField {
    fn from(field: rename::Field) -> Self {
        match field {
            rename::Field::Name => RenameField::Name,
            rename::Field::Ext => RenameField::Ext,
            rename::Field::Find => RenameField::Find,
            rename::Field::Replace => RenameField::Replace,
            rename::Field::Start => RenameField::Start,
            rename::Field::Step => RenameField::Step,
            rename::Field::Digits => RenameField::Digits,
        }
    }
}

impl From<RenameField> for rename::Field {
    fn from(field: RenameField) -> Self {
        match field {
            RenameField::Name => rename::Field::Name,
            RenameField::Ext => rename::Field::Ext,
            RenameField::Find => rename::Field::Find,
            RenameField::Replace => rename::Field::Replace,
            RenameField::Start => rename::Field::Start,
            RenameField::Step => rename::Field::Step,
            RenameField::Digits => rename::Field::Digits,
        }
    }
}

/// Letter case applied to new names.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CaseChange {
    Keep,
    Lower,
    Upper,
    /// First letter upper case, the rest lower case.
    FirstUpper,
    /// Every word starts with an upper-case letter.
    Title,
}

impl From<multirename::CaseChange> for CaseChange {
    fn from(case: multirename::CaseChange) -> Self {
        use multirename::CaseChange as C;
        match case {
            C::Keep => CaseChange::Keep,
            C::Lower => CaseChange::Lower,
            C::Upper => CaseChange::Upper,
            C::FirstUpper => CaseChange::FirstUpper,
            C::Title => CaseChange::Title,
        }
    }
}

impl From<CaseChange> for multirename::CaseChange {
    fn from(case: CaseChange) -> Self {
        use multirename::CaseChange as C;
        match case {
            CaseChange::Keep => C::Keep,
            CaseChange::Lower => C::Lower,
            CaseChange::Upper => C::Upper,
            CaseChange::FirstUpper => C::FirstUpper,
            CaseChange::Title => C::Title,
        }
    }
}

/// Edits in the multi-rename dialog.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum RenameInput {
    Text {
        field: RenameField,
        text: String,
    },
    /// A placeholder button: appends `token` to the field.
    Insert {
        field: RenameField,
        token: String,
    },
    Regex {
        on: bool,
    },
    MatchCase {
        on: bool,
    },
    CaseChange {
        change: CaseChange,
    },
    /// The Rename button, or Enter: with problems the dialog stays open.
    Apply,
}

impl From<RenameInput> for rename::RenameInput {
    fn from(input: RenameInput) -> Self {
        use rename::RenameInput as R;
        match input {
            RenameInput::Text { field, text } => R::Text(field.into(), text),
            RenameInput::Insert { field, token } => R::Insert(field.into(), token),
            RenameInput::Regex { on } => R::Regex(on),
            RenameInput::MatchCase { on } => R::MatchCase(on),
            RenameInput::CaseChange { change } => R::Case(change.into()),
            RenameInput::Apply => R::Apply,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! The multi-rename dialog through a real runtime; also the fixture the
    //! sync and properties tests use.

    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use tl_runtime::{Config, Runtime, Ui};

    use super::*;
    use crate::input::{Command, Input};
    use crate::snapshot::{DialogKindSnapshot, Snapshot};

    struct Quiet;

    impl Ui for Quiet {
        fn render(&self) {}
        fn toast(&self, _: tl_runtime::ToastKind, _: String) {}
        fn write_clipboard(&self, _: String) {}
        fn open(&self, _: &Path) {}
        fn reveal(&self, _: &Path) {}
        fn open_url(&self, _: &str) {}
        fn quit(&self) {}
    }

    const WAIT: Duration = Duration::from_secs(15);

    /// A core on temporary folders: the left one holds "dir", "a.txt" and
    /// "b.rs", the right one "a.txt" with other contents.
    pub(crate) struct Fixture {
        _tmp: tempfile::TempDir,
        pub rt: Runtime,
        pub core: Arc<Core>,
    }

    impl Fixture {
        pub fn start() -> Fixture {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().canonicalize().unwrap();
            let (data, left, right) = (root.join("data"), root.join("left"), root.join("right"));
            for dir in [&data, &left, &right, &left.join("dir")] {
                std::fs::create_dir_all(dir).unwrap();
            }
            std::fs::write(left.join("a.txt"), b"abc").unwrap();
            std::fs::write(left.join("b.rs"), b"fn main() {}").unwrap();
            std::fs::write(right.join("a.txt"), b"other").unwrap();
            let settings = serde_json::json!({
                "session": { "left_path": left.to_str().unwrap(), "right_path": right.to_str().unwrap() }
            });
            std::fs::write(data.join("settings.json"), settings.to_string()).unwrap();
            let config = Config {
                data_dir: data,
                cache_dir: root.join("cache"),
                runtime_dir: Some(root.join("run")),
                askpass_exe: None,
                plugins: None,
            };
            let rt = Runtime::start(config, Arc::new(Quiet));
            let core = Core::with_runtime(rt.clone());
            let f = Fixture {
                _tmp: tmp,
                rt,
                core,
            };
            f.wait(|s| s.ready && !s.left.loading && !s.right.loading && s.left.row_count == 4);
            f
        }

        pub fn wait(&self, done: impl Fn(&Snapshot) -> bool) -> Snapshot {
            let deadline = Instant::now() + WAIT;
            loop {
                assert!(self.rt.settle(WAIT));
                let snap = self.core.snapshot();
                if done(&snap) {
                    return snap;
                }
                assert!(Instant::now() < deadline, "timed out: {snap:#?}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        pub fn command(&self, command: Command) -> Snapshot {
            self.core.send(Input::Command { command });
            assert!(self.rt.settle(WAIT));
            self.core.snapshot()
        }

        pub fn send(&self, input: Input) -> Snapshot {
            self.core.send(input);
            assert!(self.rt.settle(WAIT));
            self.core.snapshot()
        }
    }

    fn rename_dialog(snap: &Snapshot) -> (u64, RenameSnapshot) {
        snap.dialogs
            .iter()
            .find_map(|d| match &d.kind {
                DialogKindSnapshot::Rename { dialog } => Some((d.id, dialog.clone())),
                _ => None,
            })
            .expect("a rename dialog")
    }

    #[test]
    fn the_rename_dialog_mirrors_the_core_and_its_preview_comes_by_range() {
        let f = Fixture::start();
        f.command(Command::MarkAll);
        let snap = f.command(Command::MultiRename);
        let (id, d) = rename_dialog(&snap);
        assert_eq!(d.title, "Multi-Rename 3 items");
        assert_eq!(
            (d.text(RenameField::Name), d.text(RenameField::Ext)),
            ("[N]", "[E]")
        );
        assert_eq!(d.fields.len(), 7);
        assert_eq!(d.fields[0].placeholder, "[N]");
        assert_eq!(d.fields[1].placeholder, "none");
        assert_eq!(d.case, CaseChange::Keep);
        assert_eq!(d.cases.len(), 5);
        assert_eq!(d.cases[2].label, "UPPER");
        assert_eq!(d.name_tokens[0], "[N]");
        assert_eq!(d.ext_tokens, ["[E]"]);
        assert!(d.help.starts_with("[N] name"));
        assert_eq!((d.to_rename, d.unchanged, d.problems), (0, 3, 0));
        assert_eq!(d.summary, "0 to rename · 3 unchanged");
        assert!(!d.can_apply && d.error.is_none() && d.elevated_note.is_none());
        assert_eq!(d.preview_count, 3);

        // (The platform's local times have arrived: `command` settles.)
        let version = d.preview_version;
        assert_eq!(
            rename_dialog(&f.core.snapshot()).1.preview_version,
            version,
            "unchanged: same version"
        );

        let send = |input: RenameInput| f.send(Input::Rename { dialog: id, input });
        send(RenameInput::Text {
            field: RenameField::Name,
            text: "x-".into(),
        });
        let snap = send(RenameInput::Insert {
            field: RenameField::Name,
            token: "[C]".into(),
        });
        let (_, d) = rename_dialog(&snap);
        assert_eq!(d.text(RenameField::Name), "x-[C]");
        assert_ne!(d.preview_version, version);
        assert_eq!(d.to_rename, 3);
        assert!(d.can_apply);
        let rows = f.core.rename_preview(id, 0, 100);
        let pairs: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.old.as_str(), r.new.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [("dir", "x-1"), ("a.txt", "x-2.txt"), ("b.rs", "x-3.rs")]
        );
        assert!(rows.iter().all(|r| r.changed && r.problem.is_none()));
        assert_eq!(rows[1].index, 1);
        // A range is cut at the end.
        let tail = f.core.rename_preview(id, 2, 10);
        assert_eq!((tail.len(), tail[0].index), (1, 2));
        assert!(f.core.rename_preview(id, 5, 10).is_empty());
        assert!(
            f.core.rename_preview(id + 1000, 0, 10).is_empty(),
            "another dialog"
        );

        // Clashes: every row gets a problem.
        send(RenameInput::Text {
            field: RenameField::Ext,
            text: String::new(),
        });
        let snap = send(RenameInput::Text {
            field: RenameField::Name,
            text: "same".into(),
        });
        let (_, d) = rename_dialog(&snap);
        assert_eq!(d.problems, 3);
        assert_eq!(
            d.problems_text.as_deref(),
            Some("3 cannot be renamed as shown")
        );
        assert!(!d.can_apply);
        assert!(
            f.core
                .rename_preview(id, 0, 3)
                .iter()
                .all(|r| r.problem.is_some())
        );

        // Toggles and case.
        send(RenameInput::Regex { on: true });
        send(RenameInput::MatchCase { on: true });
        let snap = send(RenameInput::CaseChange {
            change: CaseChange::Upper,
        });
        let (_, d) = rename_dialog(&snap);
        assert!(d.regex && d.match_case);
        assert_eq!(d.case, CaseChange::Upper);

        // A bad counter is the error, with no rows.
        let snap = send(RenameInput::Text {
            field: RenameField::Step,
            text: "x".into(),
        });
        let (_, d) = rename_dialog(&snap);
        assert_eq!(
            d.error.as_deref(),
            Some("The counter step must be a whole number")
        );
        assert_eq!(d.preview_count, 0);
        assert!(f.core.rename_preview(id, 0, 10).is_empty());

        f.send(Input::Dialog {
            id,
            input: crate::input::DialogInput::Cancel,
        });
        f.wait(|s| s.dialogs.is_empty());
        f.rt.shutdown();
    }
}
