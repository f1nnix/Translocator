//! Synchronize directories (⌘⌥S): compares the two panes' folders, shows
//! what would be copied each way — each item's direction can be changed by
//! clicking it — then copies through the transfer engine as a job, so a
//! sync between two folders on one server runs on the server.
//!
//! The comparison is a quiet job started when the dialog opens (and again
//! on "Compare Again" or when "Subfolders" changes). Only the answer of the
//! latest comparison counts; closing the dialog or comparing again cancels
//! the one still running.

use std::sync::Arc;

use crux_core::Command;
use tl_model::sync::{
    SyncAction, SyncDirection, SyncItem, SyncOptions, SyncPlan, SyncSide, SyncState, SyncTotals,
};
use tl_model::util::human_size;
use tl_model::{VfsError, VfsResult};

use crate::Out;
use crate::capabilities::{JobOp, JobOutput, SyncEnd, Work};
use crate::conn::LOCAL_PLUGIN;
use crate::dialog::{self, DialogKind};
use crate::ids::{DialogId, JobId};
use crate::jobs::{self, After, Spec};
use crate::model::Model;
use crate::ops;

/// The direction buttons, in order, with their labels.
pub const DIRECTIONS: [(SyncDirection, &str); 3] = [
    (SyncDirection::Both, "Both ways"),
    (SyncDirection::LeftToRight, "Left → right"),
    (SyncDirection::RightToLeft, "Right → left"),
];

pub const TITLE: &str = "Synchronize Directories";

#[derive(Clone, Debug, PartialEq)]
pub struct SyncDialog {
    pub left: SyncEnd,
    pub right: SyncEnd,
    /// "Left {left_display}" / "Right {right_display}" in the header.
    pub left_display: String,
    pub right_display: String,
    /// Subfolders (`recursive`), the direction buttons, and each side's case
    /// sensitivity.
    pub options: SyncOptions,
    /// The last comparison's plan, with the user's changes.
    pub plan: Option<Arc<SyncPlan>>,
    /// The comparison running, if any.
    pub comparing: Option<JobId>,
    /// Why the last comparison failed.
    pub error: Option<String>,
    /// "Show identical".
    pub show_same: bool,
    /// The plan items listed, as indices into `plan.items`, in order.
    pub shown: Arc<Vec<usize>>,
    /// The plan's totals, kept up to date.
    pub totals: SyncTotals,
    /// Both folders are on one server, so copies run there.
    pub server_side: bool,
}

/// What the body of the dialog shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    /// A spinner and this text.
    Comparing(&'static str),
    /// The comparison failed (danger text).
    Error(String),
    /// Nothing to list: this text, centered.
    Empty(&'static str),
    /// The rows of `shown`.
    List,
}

/// The icon of an item's action button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionIcon {
    /// Primary color.
    ArrowRight,
    /// Primary color.
    ArrowLeft,
    /// Muted.
    Equal,
    /// Warning color.
    TriangleAlert,
    /// Muted.
    Minus,
}

impl SyncDialog {
    pub fn body(&self) -> Body {
        if self.comparing.is_some() {
            Body::Comparing(if self.options.recursive {
                "Comparing folders and subfolders…"
            } else {
                "Comparing folders…"
            })
        } else if let Some(error) = &self.error {
            Body::Error(error.clone())
        } else if self.shown.is_empty() {
            Body::Empty(if self.totals.same > 0 {
                "The folders are in sync."
            } else {
                "Both folders are empty."
            })
        } else {
            Body::List
        }
    }

    /// The `row`th listed item and its index in the plan (what
    /// [`SyncInput::Cycle`] takes).
    pub fn row(&self, row: usize) -> Option<(usize, &SyncItem)> {
        let ix = *self.shown.get(row)?;
        Some((ix, self.plan.as_ref()?.items.get(ix)?))
    }

    /// "→ 2 (1.5 KB) · ← 1 (20 B) · 1 to decide · 4 identical".
    pub fn summary(&self) -> String {
        let t = &self.totals;
        let mut summary = vec![
            format!("→ {} ({})", t.to_right, human_size(t.to_right_bytes)),
            format!("← {} ({})", t.to_left, human_size(t.to_left_bytes)),
        ];
        if t.conflicts > 0 {
            summary.push(format!("{} to decide", t.conflicts));
        }
        summary.push(format!("{} identical", t.same));
        summary.join(" · ")
    }

    /// The muted line under the summary.
    pub fn note(&self) -> &'static str {
        if self.server_side {
            "Both folders are on the same server: copies run there."
        } else {
            "Newer files win; nothing is deleted. Click an arrow to change what happens to an item."
        }
    }

    /// The Synchronize button (and Enter) can run.
    pub fn can_synchronize(&self) -> bool {
        self.comparing.is_none()
            && self.plan.is_some()
            && self.totals.to_left + self.totals.to_right > 0
    }

    /// "Compare Again" is enabled.
    pub fn can_compare(&self) -> bool {
        self.comparing.is_none()
    }

    fn filter(&mut self) {
        let show_same = self.show_same;
        self.shown = Arc::new(
            self.plan
                .as_ref()
                .map(|plan| {
                    plan.items
                        .iter()
                        .enumerate()
                        .filter(|(_, item)| show_same || item.state != SyncState::Same)
                        .map(|(ix, _)| ix)
                        .collect()
                })
                .unwrap_or_default(),
        );
    }

    fn retotal(&mut self) {
        self.totals = self
            .plan
            .as_deref()
            .map(SyncPlan::totals)
            .unwrap_or_default();
    }
}

/// The action button of an item: its icon and tooltip.
pub fn action_button(item: &SyncItem) -> (ActionIcon, &'static str) {
    match item.action {
        SyncAction::ToRight => (
            ActionIcon::ArrowRight,
            "Copy to the right (click to change)",
        ),
        SyncAction::ToLeft => (ActionIcon::ArrowLeft, "Copy to the left (click to change)"),
        SyncAction::Skip if item.state == SyncState::Same => (ActionIcon::Equal, "Identical"),
        SyncAction::Skip if item.state == SyncState::Conflict => (
            ActionIcon::TriangleAlert,
            "Differs, but neither side is newer: click to choose",
        ),
        SyncAction::Skip => (ActionIcon::Minus, "Left alone (click to copy)"),
    }
}

/// The state column of a row.
pub fn state_label(state: SyncState) -> &'static str {
    match state {
        SyncState::OnlyLeft => "only left",
        SyncState::OnlyRight => "only right",
        SyncState::LeftNewer => "left newer",
        SyncState::RightNewer => "right newer",
        SyncState::Conflict => "conflict",
        SyncState::Same => "same",
    }
}

/// An item is a folder (on whichever side has it).
pub fn is_dir(item: &SyncItem) -> bool {
    item.left
        .as_ref()
        .or(item.right.as_ref())
        .is_some_and(SyncSide::is_dir)
}

/// The text of one side of a row. A file's text is followed by
/// " · {date}" with the date written as the date-format setting says, which
/// the shell does from the returned modification time.
pub fn side_text(side: &SyncSide) -> (String, Option<i64>) {
    if side.is_dir() {
        if side.files > 0 || side.size > 0 {
            (
                format!("folder · {} files, {}", side.files, human_size(side.size)),
                None,
            )
        } else {
            ("folder".to_string(), None)
        }
    } else {
        (human_size(side.size), side.mtime)
    }
}

/// Whether a side of a row is the newer one (drawn in the success color).
pub fn is_newer(item: &SyncItem, left: bool) -> bool {
    item.state
        == if left {
            SyncState::LeftNewer
        } else {
            SyncState::RightNewer
        }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SyncInput {
    /// The "Subfolders" box: compares again.
    Recursive(bool),
    /// A direction button: resets every item's action, without comparing.
    Direction(SyncDirection),
    /// "Show identical".
    ShowSame(bool),
    CompareAgain,
    /// An item's action button, by its index in the plan (see
    /// [`SyncDialog::row`]): cycles to the next possible action.
    Cycle(usize),
    /// The Synchronize button, or Enter.
    Synchronize,
}

#[derive(Debug)]
pub enum SyncReply {}

pub fn open(model: &mut Model, left: SyncEnd, right: SyncEnd, out: &mut Out) {
    let options = SyncOptions {
        recursive: true,
        direction: SyncDirection::Both,
        left_case_insensitive: model.conns.caps(left.conn).case_insensitive,
        right_case_insensitive: model.conns.caps(right.conn).case_insensitive,
    };
    let server_side = model.conns.location_key(left.conn) == model.conns.location_key(right.conn)
        && model
            .conns
            .get(left.conn)
            .is_some_and(|c| c.plugin != LOCAL_PLUGIN);
    let dialog = SyncDialog {
        left_display: model.conns.display(left.conn, &left.dir),
        right_display: model.conns.display(right.conn, &right.dir),
        left,
        right,
        options,
        plan: None,
        comparing: None,
        error: None,
        show_same: false,
        shown: Arc::new(Vec::new()),
        totals: SyncTotals::default(),
        server_side,
    };
    let id = dialog::open(model, DialogKind::Sync(Box::new(dialog)));
    compare(model, id, out);
}

fn dialog_mut(model: &mut Model, id: DialogId) -> Option<&mut SyncDialog> {
    match &mut model.dialog_mut(id)?.kind {
        DialogKind::Sync(d) => Some(d),
        _ => None,
    }
}

fn stop(dialog: &mut SyncDialog, out: &mut Out) {
    if let Some(id) = dialog.comparing.take() {
        out.push(Command::notify_shell(JobOp::Cancel { id }).into());
    }
}

/// Starts a comparison, cancelling the one still running.
fn compare(model: &mut Model, id: DialogId, out: &mut Out) {
    let Some(d) = dialog_mut(model, id) else {
        return;
    };
    stop(d, out);
    d.error = None;
    let work = Work::SyncCompare {
        left: d.left.clone(),
        right: d.right.clone(),
        options: d.options,
    };
    let job = jobs::start(
        model,
        Spec::quiet(work, After::SyncCompared(id)).no_retry(),
        out,
    );
    if let Some(d) = dialog_mut(model, id) {
        d.comparing = Some(job);
    }
}

pub fn input(model: &mut Model, id: DialogId, input: SyncInput, out: &mut Out) {
    let Some(d) = dialog_mut(model, id) else {
        return;
    };
    match input {
        SyncInput::Recursive(on) => {
            d.options.recursive = on;
            compare(model, id, out);
        }
        SyncInput::Direction(direction) => {
            d.options.direction = direction;
            if let Some(plan) = &mut d.plan {
                Arc::make_mut(plan).set_direction(direction);
            }
            d.retotal();
        }
        SyncInput::ShowSame(on) => {
            d.show_same = on;
            d.filter();
        }
        SyncInput::CompareAgain => {
            if d.comparing.is_none() {
                compare(model, id, out);
            }
        }
        SyncInput::Cycle(ix) => {
            if let Some(item) = d
                .plan
                .as_mut()
                .and_then(|p| Arc::make_mut(p).items.get_mut(ix))
            {
                item.action = item.next_action();
                d.retotal();
            }
        }
        SyncInput::Synchronize => {
            if !d.can_synchronize() {
                return;
            }
            let Some(plan) = d.plan.clone() else { return };
            let (left, right) = (d.left.clone(), d.right.clone());
            model.close_dialog(id);
            let plan = Arc::try_unwrap(plan).unwrap_or_else(|plan| (*plan).clone());
            ops::start_sync(model, plan, left, right, out);
        }
    }
}

pub fn reply(_model: &mut Model, _id: DialogId, reply: SyncReply, _out: &mut Out) {
    match reply {}
}

/// The dialog was closed: stop a comparison still running.
pub fn closed(_model: &mut Model, mut dialog: SyncDialog, out: &mut Out) {
    stop(&mut dialog, out);
}

/// A comparison job for dialog `dialog` ended.
pub fn compared(
    model: &mut Model,
    dialog: DialogId,
    job: JobId,
    result: VfsResult<JobOutput>,
    _out: &mut Out,
) {
    let Some(d) = dialog_mut(model, dialog) else {
        return;
    };
    if d.comparing != Some(job) {
        // A comparison that was superseded or cancelled.
        return;
    }
    d.comparing = None;
    match result {
        Ok(JobOutput::Plan(plan)) => d.plan = Some(Arc::new(plan)),
        Ok(_) | Err(VfsError::Cancelled) => {}
        Err(err) => {
            d.plan = None;
            d.error = Some(err.to_string());
        }
    }
    d.filter();
    d.retotal();
}

#[cfg(test)]
mod tests {
    use tl_model::{Capabilities, EntryKind, VPath};

    use super::*;
    use crate::capabilities::{JobUpdate, Work};
    use crate::conn::ConnInfo;
    use crate::dialog::DialogInput;
    use crate::ids::{ConnId, LOCAL};
    use crate::model::{Reply, shell};
    use crate::{Cmd, Effect, Event, model};

    fn conn(id: ConnId, plugin: &str, key: &str, case_insensitive: bool) -> ConnInfo {
        ConnInfo {
            id,
            plugin: plugin.into(),
            label: if plugin == "local" {
                "local".into()
            } else {
                "deploy@web-1".into()
            },
            location_key: key.into(),
            caps: Capabilities {
                case_insensitive,
                ..Capabilities::default()
            },
            home: VPath::new("/home/me"),
            bookmark: None,
            host: None,
            server_info: None,
            local_home: Some("/home/me".into()),
        }
    }

    fn model() -> Model {
        let mut model = Model::default();
        model.conns.insert(conn(LOCAL, "local", "local", true));
        model
            .conns
            .insert(conn(ConnId(7), "ssh", "ssh:web-1", false));
        model
    }

    fn end(conn: ConnId, dir: &str) -> SyncEnd {
        SyncEnd {
            conn,
            dir: VPath::new(dir),
            elevated: false,
        }
    }

    fn effects(out: Out) -> Vec<Effect> {
        let mut cmd: Cmd = Command::all(out.cmds);
        cmd.effects().collect()
    }

    fn jobs(effects: &[Effect]) -> Vec<JobOp> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Job(req) => Some(req.operation.clone()),
                _ => None,
            })
            .collect()
    }

    fn opened(model: &mut Model) -> (DialogId, JobId, Vec<Effect>) {
        let mut out = Out::default();
        open(
            model,
            end(LOCAL, "/home/me/a"),
            end(ConnId(7), "/srv/b"),
            &mut out,
        );
        let id = model.dialogs.last().unwrap().id;
        let job = dialog(model, id).comparing.unwrap();
        (id, job, effects(out))
    }

    fn send(model: &mut Model, event: Event) -> Vec<Effect> {
        let mut out = Out::default();
        model::update(event, model, &mut out);
        effects(out)
    }

    fn dialog(model: &Model, id: DialogId) -> &SyncDialog {
        match &model.dialog(id).unwrap().kind {
            DialogKind::Sync(d) => d,
            _ => panic!("not a sync dialog"),
        }
    }

    fn side(name: &str, kind: EntryKind, size: u64, mtime: i64) -> SyncSide {
        SyncSide {
            name: name.as_bytes().to_vec(),
            kind,
            size,
            mtime: Some(mtime),
            files: 0,
        }
    }

    fn item(
        rel: &str,
        left: Option<SyncSide>,
        right: Option<SyncSide>,
        state: SyncState,
        action: SyncAction,
    ) -> SyncItem {
        SyncItem {
            rel: rel.as_bytes().to_vec(),
            left,
            right,
            state,
            action,
        }
    }

    fn plan() -> SyncPlan {
        let f = |name: &str, size, mtime| Some(side(name, EntryKind::File, size, mtime));
        SyncPlan {
            options: SyncOptions::default(),
            items: vec![
                item(
                    "only-left.txt",
                    f("only-left.txt", 10, 1),
                    None,
                    SyncState::OnlyLeft,
                    SyncAction::ToRight,
                ),
                item(
                    "only-right.txt",
                    None,
                    f("only-right.txt", 20, 1),
                    SyncState::OnlyRight,
                    SyncAction::ToLeft,
                ),
                item(
                    "same.txt",
                    f("same.txt", 5, 1),
                    f("same.txt", 5, 1),
                    SyncState::Same,
                    SyncAction::Skip,
                ),
                item(
                    "odd.txt",
                    f("odd.txt", 5, 1),
                    f("odd.txt", 6, 1),
                    SyncState::Conflict,
                    SyncAction::Skip,
                ),
            ],
        }
    }

    fn finish(model: &mut Model, job: JobId, result: VfsResult<JobOutput>) -> Vec<Effect> {
        send(
            model,
            shell(Reply::Job {
                id: job,
                update: JobUpdate::Finished(result),
            }),
        )
    }

    #[test]
    fn opening_compares_with_the_default_options() {
        let mut model = model();
        let (id, job, effects) = opened(&mut model);
        let d = dialog(&model, id);
        assert_eq!(d.left_display, "~/a");
        assert_eq!(d.right_display, "deploy@web-1:/srv/b");
        assert_eq!(
            d.body(),
            Body::Comparing("Comparing folders and subfolders…")
        );
        assert!(!d.can_synchronize() && !d.can_compare());
        let ops = jobs(&effects);
        let [
            JobOp::Start {
                id: started,
                work: Work::SyncCompare { options, .. },
            },
        ] = ops.as_slice()
        else {
            panic!("expected one comparison")
        };
        assert_eq!(*started, job);
        assert_eq!(
            *options,
            SyncOptions {
                recursive: true,
                direction: SyncDirection::Both,
                left_case_insensitive: true,
                right_case_insensitive: false,
            }
        );
        assert!(
            !model.jobs.get(job).unwrap().visible,
            "the comparison is not listed"
        );
    }

    #[test]
    fn the_plan_is_listed_without_identical_items_and_totalled() {
        let mut model = model();
        let (id, job, _) = opened(&mut model);
        finish(&mut model, job, Ok(JobOutput::Plan(plan())));
        let d = dialog(&model, id);
        assert_eq!(d.body(), Body::List);
        assert_eq!(*d.shown, vec![0, 1, 3]);
        assert_eq!(
            d.summary(),
            "→ 1 (10 B) · ← 1 (20 B) · 1 to decide · 1 identical"
        );
        assert_eq!(
            d.note(),
            "Newer files win; nothing is deleted. Click an arrow to change what happens to an item."
        );
        assert!(d.can_synchronize());
        assert_eq!(
            action_button(d.row(2).unwrap().1).0,
            ActionIcon::TriangleAlert
        );

        send(&mut model, Event::Sync(id, SyncInput::ShowSame(true)));
        assert_eq!(*dialog(&model, id).shown, vec![0, 1, 2, 3]);
    }

    #[test]
    fn items_cycle_and_directions_reset_them() {
        let mut model = model();
        let (id, job, _) = opened(&mut model);
        finish(&mut model, job, Ok(JobOutput::Plan(plan())));
        // The conflict can go either way, then back to being left alone.
        send(&mut model, Event::Sync(id, SyncInput::Cycle(3)));
        assert_eq!(
            dialog(&model, id).plan.as_ref().unwrap().items[3].action,
            SyncAction::ToRight
        );
        assert_eq!(dialog(&model, id).totals.to_right, 2);
        send(&mut model, Event::Sync(id, SyncInput::Cycle(3)));
        assert_eq!(
            dialog(&model, id).plan.as_ref().unwrap().items[3].action,
            SyncAction::ToLeft
        );
        // Only-left cannot be copied to the left.
        send(&mut model, Event::Sync(id, SyncInput::Cycle(0)));
        assert_eq!(
            dialog(&model, id).plan.as_ref().unwrap().items[0].action,
            SyncAction::Skip
        );

        let effects = send(
            &mut model,
            Event::Sync(id, SyncInput::Direction(SyncDirection::LeftToRight)),
        );
        assert!(jobs(&effects).is_empty(), "no new comparison");
        let d = dialog(&model, id);
        assert_eq!(d.options.direction, SyncDirection::LeftToRight);
        let actions: Vec<SyncAction> = d
            .plan
            .as_ref()
            .unwrap()
            .items
            .iter()
            .map(|i| i.action)
            .collect();
        assert_eq!(
            actions,
            [
                SyncAction::ToRight,
                SyncAction::Skip,
                SyncAction::Skip,
                SyncAction::Skip
            ]
        );
        assert_eq!(
            d.summary(),
            "→ 1 (10 B) · ← 0 (0 B) · 1 to decide · 1 identical"
        );
    }

    #[test]
    fn a_superseded_comparison_is_cancelled_and_ignored() {
        let mut model = model();
        let (id, first, _) = opened(&mut model);
        let effects = send(&mut model, Event::Sync(id, SyncInput::Recursive(false)));
        let ops = jobs(&effects);
        assert!(ops.contains(&JobOp::Cancel { id: first }));
        let second = dialog(&model, id).comparing.unwrap();
        assert_ne!(first, second);
        assert!(
            matches!(&ops[..], [JobOp::Cancel { .. }, JobOp::Start { work: Work::SyncCompare { options, .. }, .. }] if !options.recursive)
        );
        assert_eq!(
            dialog(&model, id).body(),
            Body::Comparing("Comparing folders…")
        );

        // The first one's answer arrives late: it changes nothing.
        finish(&mut model, first, Ok(JobOutput::Plan(plan())));
        assert!(dialog(&model, id).plan.is_none());
        assert_eq!(dialog(&model, id).comparing, Some(second));
        // "Compare Again" waits for the running one.
        assert!(jobs(&send(&mut model, Event::Sync(id, SyncInput::CompareAgain))).is_empty());

        finish(&mut model, second, Ok(JobOutput::Plan(SyncPlan::default())));
        assert_eq!(
            dialog(&model, id).body(),
            Body::Empty("Both folders are empty.")
        );
        assert!(!dialog(&model, id).can_synchronize());
        let effects = send(&mut model, Event::Sync(id, SyncInput::CompareAgain));
        assert!(matches!(&jobs(&effects)[..], [JobOp::Start { .. }]));
    }

    #[test]
    fn a_failed_comparison_shows_its_error() {
        let mut model = model();
        let (id, job, _) = opened(&mut model);
        finish(&mut model, job, Err(VfsError::NotFound("/srv/b".into())));
        let d = dialog(&model, id);
        assert!(matches!(d.body(), Body::Error(text) if text.contains("/srv/b")));
        assert!(d.plan.is_none());
    }

    #[test]
    fn closing_cancels_the_running_comparison() {
        let mut model = model();
        let (id, job, _) = opened(&mut model);
        let effects = send(&mut model, Event::Dialog(id, DialogInput::Cancel));
        assert!(model.dialog(id).is_none());
        assert_eq!(jobs(&effects), [JobOp::Cancel { id: job }]);
    }

    #[test]
    fn synchronize_closes_and_runs_the_reviewed_plan() {
        let mut model = model();
        let (id, job, _) = opened(&mut model);
        // Enter while comparing does nothing.
        send(&mut model, Event::Dialog(id, DialogInput::Submit));
        assert!(model.dialog(id).is_some());
        finish(&mut model, job, Ok(JobOutput::Plan(plan())));
        send(&mut model, Event::Sync(id, SyncInput::Cycle(3)));
        let effects = send(&mut model, Event::Dialog(id, DialogInput::Submit));
        assert!(model.dialog(id).is_none());
        let ops = jobs(&effects);
        let [
            JobOp::Start {
                work: Work::SyncApply { plan, left, right },
                ..
            },
        ] = ops.as_slice()
        else {
            panic!("expected the sync job")
        };
        assert_eq!(plan.items[3].action, SyncAction::ToRight);
        assert_eq!((left.conn, right.conn), (LOCAL, ConnId(7)));
        assert_eq!(
            model.jobs.list.last().unwrap().title,
            "Synchronize ~/a ⇄ deploy@web-1:/srv/b"
        );
    }

    #[test]
    fn two_folders_on_one_server_copy_there() {
        let mut model = model();
        let mut out = Out::default();
        open(
            &mut model,
            end(ConnId(7), "/srv/a"),
            end(ConnId(7), "/srv/b"),
            &mut out,
        );
        let id = model.dialogs.last().unwrap().id;
        assert_eq!(
            dialog(&model, id).note(),
            "Both folders are on the same server: copies run there."
        );
        let mut out = Out::default();
        open(&mut model, end(LOCAL, "/a"), end(LOCAL, "/b"), &mut out);
        let id = model.dialogs.last().unwrap().id;
        assert!(!dialog(&model, id).server_side);
    }

    #[test]
    fn rows_describe_their_sides() {
        let mut dir = side("d", EntryKind::Dir, 2048, 1);
        assert_eq!(
            side_text(&dir),
            ("folder · 0 files, 2.0 KB".to_string(), None)
        );
        dir.files = 3;
        dir.size = 0;
        assert_eq!(side_text(&dir).0, "folder · 3 files, 0 B");
        dir.files = 0;
        assert_eq!(side_text(&dir).0, "folder");
        let file = side("f", EntryKind::File, 10, 42);
        assert_eq!(side_text(&file), ("10 B".to_string(), Some(42)));
        assert_eq!(state_label(SyncState::RightNewer), "right newer");
    }
}
