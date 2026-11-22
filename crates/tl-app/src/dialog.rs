//! Modal dialogs. A dialog is state in the model: the shell draws the top
//! one, sends its edits back as [`DialogInput`]s, and the dialog's purpose
//! says what its answer does. Closing a dialog any other way than with an
//! answer counts as cancelling it, so a provider waiting on a password is
//! never left hanging.

use std::sync::Arc;

use crux_core::Command;
use tl_model::transfer::{Conflict, ConflictAnswer, ConflictChoice, TransferMode};
use tl_model::{Entry, VPath};

use crate::capabilities::*;
use crate::ids::{ConnId, DialogId, JobId, PromptId, TabId};
use crate::jobs::{self, Job};
use crate::model::Model;
use crate::pane::{self, Loc, Selection, Side};
use crate::{Out, bookmarks, editor, ops, properties, rename, sync};

/// What the answer to an input dialog is for.
#[derive(Clone, Debug, PartialEq)]
pub enum InputPurpose {
    GoTo { side: Side },
    Mask { side: Side, mark: bool },
    RenamePlace { id: String },
    Transfer { mode: TransferMode, sel: Selection, dst: Loc, dst_elevated: bool },
    NewFolder { loc: Loc, elevated: bool },
    NewFile { loc: Loc, elevated: bool },
    Rename { loc: Loc, elevated: bool, name: Vec<u8>, old: String },
    Find { side: Side, loc: Loc, elevated: bool, place: String },
    Extract { sel: Selection, archives: Vec<Entry>, dst: Loc, dst_elevated: bool },
    Pack { sel: Selection, dst: Loc, dst_elevated: bool },
}

/// What a yes to a confirmation does.
#[derive(Clone, Debug, PartialEq)]
pub enum ConfirmPurpose {
    /// List the tab again with sudo.
    SudoListing { tab: TabId },
    /// Run a failed job again, elevated.
    Retry { job: Box<Job> },
    /// Run a failed transfer again with the ends that can be elevated.
    RetryTransfer { req: Box<TransferReq> },
    Delete { sel: Selection, trash: bool },
    DeleteBookmark { id: String },
    /// A provider's question.
    Prompt { id: PromptId },
    Editor { editor: crate::ids::EditorId, action: editor::Confirm },
}

#[derive(Clone, Debug, PartialEq)]
pub struct InputDialog {
    pub title: String,
    pub message: String,
    pub text: String,
    /// Byte range to preselect; `None` selects everything.
    pub select: Option<(usize, usize)>,
    pub ok: String,
    pub purpose: InputPurpose,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConfirmDialog {
    pub title: String,
    pub message: String,
    pub ok: String,
    pub danger: bool,
    pub purpose: ConfirmPurpose,
}

/// One hit of a search.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// Below the search root, as text.
    pub rel: String,
    pub path: VPath,
    pub is_dir: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DialogKind {
    Input(InputDialog),
    Confirm(ConfirmDialog),
    /// An error that needs reading, rather than a toast.
    Message { title: String, message: String },
    /// A provider asks for a password.
    Secret { id: PromptId, title: String, message: String, text: String },
    /// A transfer asks how to settle a name clash.
    Conflict { job: JobId, ask: u64, conflict: Box<Conflict>, apply_all: bool },
    FindResults { title: String, side: Side, conn: ConnId, rows: Arc<Vec<Found>> },
    About,
    Rename(Box<rename::RenameDialog>),
    Sync(Box<sync::SyncDialog>),
    Properties(Box<properties::PropertiesDialog>),
    BookmarkEdit(Box<bookmarks::EditDialog>),
    Bookmarks(bookmarks::ManageDialog),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dialog {
    pub id: DialogId,
    pub kind: DialogKind,
}

/// Edits and answers in a dialog.
#[derive(Clone, Debug, PartialEq)]
pub enum DialogInput {
    /// The text field changed.
    Text(String),
    /// The conflict dialog's "apply to all" box.
    ApplyAll(bool),
    /// OK, Enter.
    Submit,
    /// Cancel, Escape, or the dialog closed some other way.
    Cancel,
    /// A conflict dialog's button.
    Choose(ConflictChoice),
    /// A search hit was clicked.
    Pick(usize),
    /// The about box's README button.
    OpenReadme,
}

pub const HELP_URL: &str = "https://github.com/f1nnix/Translocator";

fn push(model: &mut Model, kind: DialogKind) -> DialogId {
    let id = model.ids.dialog();
    model.dialogs.push(Dialog { id, kind });
    id
}

pub fn open(model: &mut Model, kind: DialogKind) -> DialogId {
    push(model, kind)
}

pub fn ask_input(
    model: &mut Model,
    title: impl Into<String>,
    message: impl Into<String>,
    text: impl Into<String>,
    select: Option<(usize, usize)>,
    ok: impl Into<String>,
    purpose: InputPurpose,
) -> DialogId {
    push(
        model,
        DialogKind::Input(InputDialog {
            title: title.into(),
            message: message.into(),
            text: text.into(),
            select,
            ok: ok.into(),
            purpose,
        }),
    )
}

pub fn ask_confirm(
    model: &mut Model,
    title: impl Into<String>,
    message: impl Into<String>,
    ok: impl Into<String>,
    danger: bool,
    purpose: ConfirmPurpose,
) -> DialogId {
    push(
        model,
        DialogKind::Confirm(ConfirmDialog {
            title: title.into(),
            message: message.into(),
            ok: ok.into(),
            danger,
            purpose,
        }),
    )
}

pub fn error(model: &mut Model, title: impl Into<String>, message: impl Into<String>) {
    push(
        model,
        DialogKind::Message {
            title: title.into(),
            message: message.into(),
        },
    );
}

/// The byte range of a name's stem, for preselection when renaming: the
/// whole name without a dot, or for a dotfile.
pub fn stem_range(name: &str) -> (usize, usize) {
    match name.rfind('.') {
        Some(0) | None => (0, name.len()),
        Some(dot) => (0, dot),
    }
}

/// A question from a provider becomes a dialog.
pub fn prompt(model: &mut Model, output: PromptOutput, _out: &mut Out) {
    let PromptOutput::Request(PromptRequest { id, kind }) = output else { return };
    match kind {
        PromptKind::Secret { title, message } => {
            push(
                model,
                DialogKind::Secret {
                    id,
                    title,
                    message,
                    text: String::new(),
                },
            );
        }
        PromptKind::Confirm { title, message } => {
            ask_confirm(model, title, message, "Continue", false, ConfirmPurpose::Prompt { id });
        }
    }
}

fn answer_prompt(id: PromptId, answer: PromptAnswer, out: &mut Out) {
    out.push(Command::notify_shell(PromptOp::Answer { id, answer }).into());
}

pub fn conflict(model: &mut Model, job: JobId, ask: u64, conflict: Conflict) {
    push(
        model,
        DialogKind::Conflict {
            job,
            ask,
            conflict: Box::new(conflict),
            apply_all: false,
        },
    );
}

/// A job ended: its open questions go with it.
pub fn job_finished(model: &mut Model, job: JobId) {
    model
        .dialogs
        .retain(|d| !matches!(&d.kind, DialogKind::Conflict { job: j, .. } if *j == job));
}

fn answer_conflict(job: JobId, ask: u64, choice: ConflictChoice, apply_all: bool, out: &mut Out) {
    let answer = ConflictAnswer {
        choice,
        apply_to_all: apply_all,
        rename_to: None,
    };
    out.push(Command::notify_shell(JobOp::Answer { id: job, ask, answer }).into());
}

pub fn input(model: &mut Model, id: DialogId, input: DialogInput, out: &mut Out) {
    let Some(dialog) = model.dialog_mut(id) else { return };
    match (&mut dialog.kind, input) {
        (DialogKind::Input(d), DialogInput::Text(text)) => d.text = text,
        (DialogKind::Secret { text, .. }, DialogInput::Text(t)) => *text = t,
        (DialogKind::Conflict { apply_all, .. }, DialogInput::ApplyAll(on)) => *apply_all = on,
        (DialogKind::Conflict { job, ask, apply_all, .. }, DialogInput::Choose(choice)) => {
            let (job, ask, all) = (*job, *ask, *apply_all);
            model.close_dialog(id);
            answer_conflict(job, ask, choice, all, out);
        }
        (DialogKind::Conflict { job, ask, apply_all, .. }, DialogInput::Submit) => {
            let (job, ask, all) = (*job, *ask, *apply_all);
            model.close_dialog(id);
            answer_conflict(job, ask, ConflictChoice::Overwrite, all, out);
        }
        (DialogKind::FindResults { side, conn, rows, .. }, DialogInput::Pick(ix)) => {
            let (side, conn) = (*side, *conn);
            let hit = rows.get(ix).cloned();
            model.close_dialog(id);
            if let Some(hit) = hit {
                ops::reveal(model, side, conn, hit.path, out);
            }
        }
        (DialogKind::About, DialogInput::OpenReadme) => out.platform(PlatformOp::OpenUrl(HELP_URL.into())),
        (_, DialogInput::Submit) => submit(model, id, out),
        (_, DialogInput::Cancel) => cancel(model, id, out),
        _ => {}
    }
}

fn submit(model: &mut Model, id: DialogId, out: &mut Out) {
    let Some(dialog) = model.dialog(id).cloned() else { return };
    match dialog.kind {
        DialogKind::Input(d) => {
            model.close_dialog(id);
            input_answered(model, d.purpose, d.text, out);
        }
        DialogKind::Confirm(d) => {
            model.close_dialog(id);
            confirmed(model, d.purpose, true, out);
        }
        DialogKind::Secret { id: prompt, text, .. } => {
            model.close_dialog(id);
            answer_prompt(prompt, PromptAnswer::Secret(Some(text)), out);
        }
        DialogKind::Message { .. } | DialogKind::About | DialogKind::FindResults { .. } => {
            model.close_dialog(id);
        }
        DialogKind::Rename(_) => rename::input(model, id, rename::RenameInput::Apply, out),
        DialogKind::Sync(_) => sync::input(model, id, sync::SyncInput::Synchronize, out),
        DialogKind::Properties(_) => properties::input(model, id, properties::PropertiesInput::Apply, out),
        DialogKind::BookmarkEdit(_) => bookmarks::input(
            model,
            bookmarks::BookmarksInput::Save {
                dialog: id,
                open: true,
            },
            out,
        ),
        DialogKind::Bookmarks(_) => {
            model.close_dialog(id);
        }
        DialogKind::Conflict { .. } => {}
    }
}

/// Closes a dialog without its answer, giving whoever waits on it the
/// default one.
pub fn cancel(model: &mut Model, id: DialogId, out: &mut Out) {
    let Some(dialog) = model.close_dialog(id) else { return };
    match dialog.kind {
        DialogKind::Confirm(d) => confirmed(model, d.purpose, false, out),
        DialogKind::Secret { id: prompt, .. } => answer_prompt(prompt, PromptAnswer::Secret(None), out),
        DialogKind::Conflict { job, ask, .. } => answer_conflict(job, ask, ConflictChoice::Cancel, false, out),
        DialogKind::Sync(d) => sync::closed(model, *d, out),
        _ => {}
    }
}

fn confirmed(model: &mut Model, purpose: ConfirmPurpose, yes: bool, out: &mut Out) {
    match purpose {
        ConfirmPurpose::Prompt { id } => answer_prompt(id, PromptAnswer::Confirm(yes), out),
        ConfirmPurpose::Retry { job } => {
            if yes {
                jobs::retry_elevated(model, *job, out);
            } else {
                jobs::give_up(model, *job, out);
            }
        }
        _ if !yes => {}
        ConfirmPurpose::SudoListing { tab } => pane::elevate(model, tab, out),
        ConfirmPurpose::RetryTransfer { req } => ops::retry_transfer(model, *req, out),
        ConfirmPurpose::Delete { sel, trash } => ops::delete_confirmed(model, sel, trash, out),
        ConfirmPurpose::DeleteBookmark { id } => bookmarks::delete_confirmed(model, id, out),
        ConfirmPurpose::Editor { editor, action } => editor::confirmed(model, editor, action, out),
    }
}

fn input_answered(model: &mut Model, purpose: InputPurpose, text: String, out: &mut Out) {
    match purpose {
        InputPurpose::GoTo { side } => ops::go_to(model, side, &text, out),
        InputPurpose::Mask { side, mark } => pane::apply_mask(model, side, mark, &text),
        InputPurpose::RenamePlace { id } => pane::rename_place(model, &id, text, out),
        InputPurpose::Transfer { mode, sel, dst, dst_elevated } => {
            ops::transfer_answered(model, mode, sel, dst, dst_elevated, text, out)
        }
        InputPurpose::NewFolder { loc, elevated } => ops::new_folder_answered(model, loc, elevated, text, out),
        InputPurpose::NewFile { loc, elevated } => ops::new_file_answered(model, loc, elevated, text, out),
        InputPurpose::Rename { loc, elevated, name, old } => {
            ops::rename_answered(model, loc, elevated, name, old, text, out)
        }
        InputPurpose::Find { side, loc, elevated, place } => ops::find_answered(model, side, loc, elevated, place, text, out),
        InputPurpose::Extract { sel, archives, dst, dst_elevated } => {
            ops::extract_answered(model, sel, archives, dst, dst_elevated, text, out)
        }
        InputPurpose::Pack { sel, dst, dst_elevated } => ops::pack_answered(model, sel, dst, dst_elevated, text, out),
    }
}
