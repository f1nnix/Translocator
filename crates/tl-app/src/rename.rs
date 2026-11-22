//! The multi-rename dialog (Ctrl+M / ⌘⇧M): new names for the selected files
//! from a name and an extension mask, a counter, find & replace and case
//! conversion, with a live preview. Names that would clash are flagged
//! before anything is renamed.
//!
//! The masks' date placeholders need local calendar fields, which only the
//! platform knows: the dialog asks for them when it opens
//! ([`PlatformOp::LocalTimes`]) and the preview shows without dates until
//! they arrive.

use std::sync::Arc;

use crux_core::Command;
use tl_model::multirename::{self, CaseChange, Preview, RenameSource, RenameSpec};

use crate::Out;
use crate::capabilities::{PlatformOp, PlatformOutput};
use crate::dialog::{self, DialogKind};
use crate::ids::DialogId;
use crate::model::{Model, Reply, shell};
use crate::ops;
use crate::pane::Selection;

/// The case buttons, in order, with their labels.
pub const CASES: [(CaseChange, &str); 5] = [
    (CaseChange::Keep, "Unchanged"),
    (CaseChange::Lower, "lower"),
    (CaseChange::Upper, "UPPER"),
    (CaseChange::FirstUpper, "First"),
    (CaseChange::Title, "Title Case"),
];

/// The placeholder buttons beside the name mask; a click appends one.
pub const NAME_TOKENS: [&str; 6] = ["[N]", "[N1-3]", "[C]", "[P]", "[YMD]", "[hms]"];
/// The placeholder buttons beside the extension mask.
pub const EXT_TOKENS: [&str; 1] = ["[E]"];

/// The line under the masks.
pub const HELP: &str = "[N] name · [E] extension · [N2-5] characters 2–5, [N3,2] two from the third, [N-3-] the last three · \
     [C] counter, [C10+5:3] with its own start, step and digits · [P] folder · [YMD] [hms] [Y] [M] [D] [h] [m] [s] \
     modification time · [[ and ]] for brackets. Regex replacements may use $1.";

/// The text fields of the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Name,
    Ext,
    Find,
    Replace,
    Start,
    Step,
    Digits,
}

impl Field {
    pub const ALL: [Field; 7] = [
        Field::Name,
        Field::Ext,
        Field::Find,
        Field::Replace,
        Field::Start,
        Field::Step,
        Field::Digits,
    ];

    /// What the empty field shows.
    pub fn placeholder(self) -> &'static str {
        match self {
            Field::Name => "[N]",
            Field::Ext => "none",
            Field::Find => "text to find",
            Field::Replace => "replace with",
            Field::Start | Field::Step | Field::Digits => "1",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenameDialog {
    /// "Multi-Rename “a.txt”" / "Multi-Rename 3 items".
    pub title: String,
    pub sel: Arc<Selection>,
    /// Every name in the folder, to catch clashes with unselected files.
    pub existing: Arc<Vec<Vec<u8>>>,
    /// Names differing only in case are the same file here.
    pub case_insensitive: bool,
    /// The name mask (focused, all selected, when the dialog opens).
    pub name: String,
    pub ext: String,
    pub find: String,
    pub replace: String,
    /// Counter start, step and digits, as typed.
    pub start: String,
    pub step: String,
    pub digits: String,
    pub regex: bool,
    pub match_case: bool,
    pub case: CaseChange,
    /// Local calendar fields of each selected entry's modification time, in
    /// the selection's order; empty until the platform has answered.
    pub times: Arc<Vec<Option<[i64; 6]>>>,
    /// One row per selected entry ("old → new", and its problem), or why
    /// the masks cannot be used: shown in place of the list.
    pub preview: Arc<Result<Vec<Preview>, String>>,
    /// Rows that get a new name and have no problem.
    pub to_rename: usize,
    /// Rows that keep their name and have no problem.
    pub unchanged: usize,
    /// Rows with a problem.
    pub problems: usize,
}

impl RenameDialog {
    pub fn text(&self, field: Field) -> &str {
        match field {
            Field::Name => &self.name,
            Field::Ext => &self.ext,
            Field::Find => &self.find,
            Field::Replace => &self.replace,
            Field::Start => &self.start,
            Field::Step => &self.step,
            Field::Digits => &self.digits,
        }
    }

    fn text_mut(&mut self, field: Field) -> &mut String {
        match field {
            Field::Name => &mut self.name,
            Field::Ext => &mut self.ext,
            Field::Find => &mut self.find,
            Field::Replace => &mut self.replace,
            Field::Start => &mut self.start,
            Field::Step => &mut self.step,
            Field::Digits => &mut self.digits,
        }
    }

    /// The masks, or what is wrong with the counter fields.
    pub fn spec(&self) -> Result<RenameSpec, String> {
        let number = |text: &str, what: &str| {
            text.trim()
                .parse::<i64>()
                .map_err(|_| format!("{what} must be a whole number"))
        };
        Ok(RenameSpec {
            name_mask: self.name.clone(),
            ext_mask: self.ext.clone(),
            find: self.find.clone(),
            replace: self.replace.clone(),
            regex: self.regex,
            match_case: self.match_case,
            case: self.case,
            counter_start: number(&self.start, "The counter start")?,
            counter_step: number(&self.step, "The counter step")?,
            counter_width: number(&self.digits, "The number of digits")?.clamp(0, 12) as usize,
        })
    }

    /// The error shown instead of the list.
    pub fn error(&self) -> Option<&str> {
        self.preview.as_ref().as_ref().err().map(String::as_str)
    }

    /// The preview rows (none when there is an error).
    pub fn rows(&self) -> &[Preview] {
        match self.preview.as_ref() {
            Ok(rows) => rows,
            Err(_) => &[],
        }
    }

    /// "2 to rename · 1 unchanged".
    pub fn summary(&self) -> String {
        let mut summary = format!("{} to rename", self.to_rename);
        if self.unchanged > 0 {
            summary.push_str(&format!(" · {} unchanged", self.unchanged));
        }
        summary
    }

    /// The warning under the summary, when some rows have problems.
    pub fn problems_text(&self) -> Option<String> {
        (self.problems > 0).then(|| format!("{} cannot be renamed as shown", self.problems))
    }

    /// The note shown when the renames run with sudo.
    pub fn elevated_note(&self) -> Option<&'static str> {
        self.sel.elevated.then_some("Runs as root (sudo).")
    }

    /// The Rename button is enabled.
    pub fn can_apply(&self) -> bool {
        self.to_rename > 0 && self.problems == 0
    }

    /// Recomputes the preview and its counts after any change.
    fn refresh(&mut self) {
        let files: Vec<RenameSource> = self
            .sel
            .entries
            .iter()
            .enumerate()
            .map(|(ix, e)| RenameSource {
                name: &e.name,
                is_dir: e.is_dir(),
                time: self.times.get(ix).copied().flatten(),
            })
            .collect();
        let parent = self.sel.dir.file_name_lossy();
        let preview = self.spec().and_then(|spec| {
            multirename::preview(
                &spec,
                &files,
                &parent,
                &self.existing,
                self.case_insensitive,
            )
        });
        self.set_preview(preview);
    }

    fn set_preview(&mut self, preview: Result<Vec<Preview>, String>) {
        (self.to_rename, self.unchanged, self.problems) = match &preview {
            Ok(rows) => (
                rows.iter()
                    .filter(|p| p.problem.is_none() && p.changed())
                    .count(),
                rows.iter()
                    .filter(|p| p.problem.is_none() && !p.changed())
                    .count(),
                rows.iter().filter(|p| p.problem.is_some()).count(),
            ),
            Err(_) => (0, 0, 0),
        };
        self.preview = Arc::new(preview);
    }
}

/// Edits in the dialog. Text fields send their whole new value.
#[derive(Clone, Debug, PartialEq)]
pub enum RenameInput {
    Text(Field, String),
    /// A placeholder button: appends `token` to the field (the shell puts
    /// the keyboard there).
    Insert(Field, String),
    Regex(bool),
    MatchCase(bool),
    Case(CaseChange),
    /// The Rename button, or Enter: with problems the dialog stays open.
    Apply,
}

#[derive(Debug)]
pub enum RenameReply {
    /// The local calendar fields of the selection's modification times.
    Times(PlatformOutput),
}

/// Opens the dialog for `sel`; `existing` is every name listed in the folder.
pub fn open(model: &mut Model, sel: Selection, existing: Vec<Vec<u8>>, out: &mut Out) {
    let mtimes: Vec<Option<i64>> = sel.entries.iter().map(|e| e.mtime).collect();
    let case_insensitive = model.conns.caps(sel.conn).case_insensitive;
    let mut dialog = RenameDialog {
        title: format!("Multi-Rename {}", sel.describe()),
        sel: Arc::new(sel),
        existing: Arc::new(existing),
        case_insensitive,
        name: "[N]".into(),
        ext: "[E]".into(),
        find: String::new(),
        replace: String::new(),
        start: "1".into(),
        step: "1".into(),
        digits: "1".into(),
        regex: false,
        match_case: false,
        case: CaseChange::Keep,
        times: Arc::new(Vec::new()),
        preview: Arc::new(Ok(Vec::new())),
        to_rename: 0,
        unchanged: 0,
        problems: 0,
    };
    dialog.refresh();
    let id = dialog::open(model, DialogKind::Rename(Box::new(dialog)));
    if mtimes.iter().any(Option::is_some) {
        out.push(
            Command::request_from_shell(PlatformOp::LocalTimes(mtimes))
                .then_send(move |times| shell(Reply::Rename(id, RenameReply::Times(times)))),
        );
    }
}

fn dialog_mut(model: &mut Model, id: DialogId) -> Option<&mut RenameDialog> {
    match &mut model.dialog_mut(id)?.kind {
        DialogKind::Rename(d) => Some(d),
        _ => None,
    }
}

pub fn input(model: &mut Model, id: DialogId, input: RenameInput, out: &mut Out) {
    let Some(d) = dialog_mut(model, id) else {
        return;
    };
    match input {
        RenameInput::Text(field, text) => *d.text_mut(field) = text,
        RenameInput::Insert(field, token) => d.text_mut(field).push_str(&token),
        RenameInput::Regex(on) => d.regex = on,
        RenameInput::MatchCase(on) => d.match_case = on,
        RenameInput::Case(case) => d.case = case,
        RenameInput::Apply => return apply(model, id, out),
    }
    d.refresh();
}

fn apply(model: &mut Model, id: DialogId, out: &mut Out) {
    let Some(d) = dialog_mut(model, id) else {
        return;
    };
    if !d.can_apply() {
        return;
    }
    let Ok(rows) = d.preview.as_ref() else { return };
    let renames: Vec<multirename::Rename> = rows
        .iter()
        .filter(|p| p.changed())
        .map(|p| (p.old.clone(), p.new.clone()))
        .collect();
    let steps = match multirename::plan_steps(&renames, &d.existing, d.case_insensitive) {
        Ok(steps) => steps,
        Err(err) => {
            d.set_preview(Err(err));
            return;
        }
    };
    let select = renames[0].1.clone();
    let sel = (*d.sel).clone();
    model.close_dialog(id);
    ops::apply_renames(model, sel, steps, renames.len(), select, out);
}

pub fn reply(model: &mut Model, id: DialogId, reply: RenameReply, _out: &mut Out) {
    let RenameReply::Times(PlatformOutput::Times(times)) = reply else {
        return;
    };
    let Some(d) = dialog_mut(model, id) else {
        return;
    };
    if times.len() != d.sel.entries.len() {
        return;
    }
    d.times = Arc::new(times);
    d.refresh();
}

#[cfg(test)]
mod tests {
    use tl_model::{Capabilities, Entry, EntryKind, VPath};

    use super::*;
    use crate::capabilities::{JobOp, Work};
    use crate::conn::ConnInfo;
    use crate::ids::LOCAL;
    use crate::{Cmd, Effect, Event, model};

    fn model(case_insensitive: bool) -> Model {
        let mut model = Model::default();
        model.conns.insert(ConnInfo {
            id: LOCAL,
            plugin: "local".into(),
            label: "local".into(),
            location_key: "local".into(),
            caps: Capabilities {
                case_insensitive,
                ..Capabilities::default()
            },
            home: VPath::new("/home/me"),
            bookmark: None,
            host: None,
            server_info: None,
            local_home: Some("/home/me".into()),
        });
        model
    }

    fn file(name: &str, mtime: Option<i64>) -> Entry {
        let mut e = Entry::new(name, EntryKind::File);
        e.mtime = mtime;
        e
    }

    fn selection(names: &[&str]) -> Selection {
        Selection {
            conn: LOCAL,
            dir: VPath::new("/home/me/photos"),
            entries: names.iter().map(|n| file(n, Some(1_700_000_000))).collect(),
            elevated: false,
        }
    }

    fn opened(model: &mut Model, names: &[&str], existing: &[&str]) -> (DialogId, Vec<Effect>) {
        let mut out = Out::default();
        let existing = existing.iter().map(|n| n.as_bytes().to_vec()).collect();
        open(model, selection(names), existing, &mut out);
        let id = model.dialogs.last().unwrap().id;
        (id, effects(out))
    }

    fn effects(out: Out) -> Vec<Effect> {
        let mut cmd: Cmd = Command::all(out.cmds);
        cmd.effects().collect()
    }

    fn send(model: &mut Model, id: DialogId, input: RenameInput) -> Vec<Effect> {
        let mut out = Out::default();
        model::update(Event::Rename(id, input), model, &mut out);
        effects(out)
    }

    fn dialog(model: &Model, id: DialogId) -> &RenameDialog {
        match &model.dialog(id).unwrap().kind {
            DialogKind::Rename(d) => d,
            _ => panic!("not a rename dialog"),
        }
    }

    fn new_names(model: &Model, id: DialogId) -> Vec<String> {
        dialog(model, id)
            .rows()
            .iter()
            .map(|p| String::from_utf8_lossy(&p.new).into_owned())
            .collect()
    }

    #[test]
    fn opens_with_the_defaults_and_asks_for_local_times() {
        let mut model = model(false);
        let (id, effects) = opened(&mut model, &["a.JPG", "b.JPG"], &["a.JPG", "b.JPG"]);
        let d = dialog(&model, id);
        assert_eq!(d.title, "Multi-Rename 2 items");
        assert_eq!((d.name.as_str(), d.ext.as_str()), ("[N]", "[E]"));
        assert_eq!(
            (d.start.as_str(), d.step.as_str(), d.digits.as_str()),
            ("1", "1", "1")
        );
        assert_eq!(d.case, CaseChange::Keep);
        assert_eq!(d.summary(), "0 to rename · 2 unchanged");
        assert!(!d.can_apply());
        let [Effect::Platform(req)] = effects.as_slice() else {
            panic!("expected one platform effect")
        };
        assert_eq!(
            req.operation,
            PlatformOp::LocalTimes(vec![Some(1_700_000_000); 2])
        );
    }

    #[test]
    fn the_preview_follows_every_edit_and_dates_arrive_later() {
        let mut model = model(false);
        let mut out = Out::default();
        let existing = vec![b"a.JPG".to_vec(), b"b.JPG".to_vec()];
        open(
            &mut model,
            selection(&["a.JPG", "b.JPG"]),
            existing,
            &mut out,
        );
        let id = model.dialogs.last().unwrap().id;
        let mut cmd: Cmd = Command::all(out.cmds);
        let Some(Effect::Platform(mut times)) = cmd.effects().next() else {
            panic!()
        };

        send(
            &mut model,
            id,
            RenameInput::Text(Field::Name, "photo-".into()),
        );
        send(
            &mut model,
            id,
            RenameInput::Insert(Field::Name, "[C]".into()),
        );
        assert_eq!(new_names(&model, id), ["photo-1.JPG", "photo-2.JPG"]);
        send(&mut model, id, RenameInput::Case(CaseChange::Lower));
        assert_eq!(new_names(&model, id), ["photo-1.jpg", "photo-2.jpg"]);
        assert_eq!(dialog(&model, id).summary(), "2 to rename");

        send(
            &mut model,
            id,
            RenameInput::Text(Field::Name, "[Y]-[C]".into()),
        );
        assert_eq!(new_names(&model, id), ["-1.jpg", "-2.jpg"], "no dates yet");
        times
            .resolve(PlatformOutput::Times(vec![Some([2024, 3, 9, 10, 0, 0]); 2]))
            .unwrap();
        for event in cmd.events().collect::<Vec<_>>() {
            model::update(event, &mut model, &mut Out::default());
        }
        assert_eq!(new_names(&model, id), ["2024-1.jpg", "2024-2.jpg"]);
    }

    #[test]
    fn counter_fields_must_be_whole_numbers() {
        let mut model = model(false);
        let (id, _) = opened(&mut model, &["a.txt"], &["a.txt"]);
        send(&mut model, id, RenameInput::Text(Field::Step, "x".into()));
        assert_eq!(
            dialog(&model, id).error(),
            Some("The counter step must be a whole number")
        );
        assert!(dialog(&model, id).rows().is_empty());
        send(&mut model, id, RenameInput::Text(Field::Step, " 2 ".into()));
        assert_eq!(dialog(&model, id).error(), None);
        send(
            &mut model,
            id,
            RenameInput::Text(Field::Digits, "1.5".into()),
        );
        assert_eq!(
            dialog(&model, id).error(),
            Some("The number of digits must be a whole number")
        );
    }

    #[test]
    fn clashes_keep_the_dialog_open() {
        let mut model = model(false);
        let (id, _) = opened(&mut model, &["a.txt", "b.txt"], &["a.txt", "b.txt"]);
        send(
            &mut model,
            id,
            RenameInput::Text(Field::Name, "same".into()),
        );
        let d = dialog(&model, id);
        assert_eq!(d.problems, 2);
        assert_eq!(
            d.problems_text().as_deref(),
            Some("2 cannot be renamed as shown")
        );
        assert!(!d.can_apply());
        let effects = send(&mut model, id, RenameInput::Apply);
        assert!(model.dialog(id).is_some(), "the dialog stays open");
        assert!(effects.iter().all(|e| !matches!(e, Effect::Job(_))));
        // Enter goes the same way.
        model::update(
            Event::Dialog(id, crate::dialog::DialogInput::Submit),
            &mut model,
            &mut Out::default(),
        );
        assert!(model.dialog(id).is_some());
    }

    #[test]
    fn case_insensitivity_comes_from_the_file_system() {
        let mut model = model(true);
        let (id, _) = opened(&mut model, &["a.txt"], &["a.txt", "B.txt"]);
        send(&mut model, id, RenameInput::Text(Field::Name, "b".into()));
        assert_eq!(dialog(&model, id).problems, 1, "b.txt clashes with B.txt");
        let mut model = self::model(false);
        let (id, _) = opened(&mut model, &["a.txt"], &["a.txt", "B.txt"]);
        send(&mut model, id, RenameInput::Text(Field::Name, "b".into()));
        assert_eq!(dialog(&model, id).problems, 0);
    }

    #[test]
    fn apply_closes_the_dialog_and_starts_the_renames() {
        let mut model = model(false);
        let (id, _) = opened(
            &mut model,
            &["a.JPG", "b.JPG", "keep.txt"],
            &["a.JPG", "b.JPG", "keep.txt"],
        );
        send(
            &mut model,
            id,
            RenameInput::Text(Field::Name, "photo-[C]".into()),
        );
        send(&mut model, id, RenameInput::Text(Field::Ext, "[E]".into()));
        let effects = send(&mut model, id, RenameInput::Apply);
        assert!(model.dialog(id).is_none());
        let job = effects
            .iter()
            .find_map(|e| match e {
                Effect::Job(req) => Some(req.operation.clone()),
                _ => None,
            })
            .expect("a job");
        let JobOp::Start {
            work: Work::Renames { steps, dir, .. },
            ..
        } = job
        else {
            panic!("{job:?}")
        };
        assert_eq!(dir, VPath::new("/home/me/photos"));
        assert_eq!(steps.len(), 3);
        assert_eq!(model.jobs.list.last().unwrap().title, "Rename 3 items");
    }

    #[test]
    fn a_plan_that_cannot_be_ordered_is_shown_as_the_error() {
        let mut model = model(false);
        let (id, _) = opened(&mut model, &["a"], &["a"]);
        // Force a preview that `plan_steps` rejects: a target that exists
        // but is not being renamed away.
        let d = dialog_mut(&mut model, id).unwrap();
        d.existing = Arc::new(vec![b"a".to_vec(), b"b".to_vec()]);
        d.set_preview(Ok(vec![Preview {
            old: b"a".to_vec(),
            new: b"b".to_vec(),
            problem: None,
        }]));
        send(&mut model, id, RenameInput::Apply);
        assert!(model.dialog(id).is_some());
        assert!(dialog(&model, id).error().is_some());
    }
}
