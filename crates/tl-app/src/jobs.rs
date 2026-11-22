//! Background jobs: every operation that changes files or can take long.
//!
//! A job is started with the work it does and what should happen when it
//! ends ([`After`]). The shell streams its progress; the core keeps the
//! list the jobs sheet and the strip under the panes show. A "permission
//! denied" on a file system that can use sudo is offered as a retry before
//! the job's own follow-up hears about the failure.

use crux_core::Command;
use tl_model::{ProgressState, VfsError, VfsResult};

use crate::capabilities::*;
use crate::dialog::{self, ConfirmPurpose};
use crate::ids::{DialogId, EditorId, JobId};
use crate::model::{Model, Reply, shell};
use crate::{Out, editor, ops, sync};

/// What runs when a job ends.
#[derive(Clone, Debug, PartialEq)]
pub enum After {
    /// Nothing beyond the job's status line.
    Nothing,
    Ops(ops::After),
    /// A comparison for the synchronize dialog.
    SyncCompared(DialogId),
    Editor(EditorId, editor::After),
}

#[derive(Clone, Debug, PartialEq)]
pub enum JobStatus {
    Running,
    Done(String),
    Failed(String),
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub id: JobId,
    pub title: String,
    /// Listed in the jobs sheet and strip; quick operations (a rename, a
    /// new folder) are not.
    pub visible: bool,
    pub work: Work,
    pub after: After,
    /// A "permission denied" is offered as a retry with sudo.
    pub retry: bool,
    pub status: JobStatus,
    pub progress: ProgressState,
    pub elapsed_ms: u64,
    /// Bytes per second, smoothed.
    pub rate: f64,
    sample: (u64, u64),
}

impl Job {
    pub fn is_running(&self) -> bool {
        self.status == JobStatus::Running
    }
}

#[derive(Clone, Debug, Default)]
pub struct Jobs {
    pub list: Vec<Job>,
}

impl Jobs {
    pub fn get(&self, id: JobId) -> Option<&Job> {
        self.list.iter().find(|j| j.id == id)
    }

    fn get_mut(&mut self, id: JobId) -> Option<&mut Job> {
        self.list.iter_mut().find(|j| j.id == id)
    }

    pub fn running(&self) -> usize {
        self.list.iter().filter(|j| j.visible && j.is_running()).count()
    }

    pub fn visible(&self) -> impl Iterator<Item = &Job> {
        self.list.iter().filter(|j| j.visible)
    }
}

/// Input from the jobs sheet and strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobsInput {
    Cancel(JobId),
    ClearFinished,
    /// The sheet was opened or closed by the shell itself.
    SheetShown(bool),
}

pub struct Spec {
    pub title: Option<String>,
    pub work: Work,
    pub after: After,
    pub retry: bool,
}

impl Spec {
    /// A job in the jobs list, retried with sudo on "permission denied".
    pub fn listed(title: impl Into<String>, work: Work, after: After) -> Spec {
        Spec {
            title: Some(title.into()),
            work,
            after,
            retry: true,
        }
    }

    /// A quick operation that gets no row in the jobs list.
    pub fn quiet(work: Work, after: After) -> Spec {
        Spec {
            title: None,
            work,
            after,
            retry: true,
        }
    }

    /// Handles "permission denied" itself.
    pub fn no_retry(mut self) -> Spec {
        self.retry = false;
        self
    }
}

pub fn start(model: &mut Model, spec: Spec, out: &mut Out) -> JobId {
    let id = model.ids.job();
    let mut progress = ProgressState::default();
    if matches!(spec.work, Work::Connect { .. }) {
        progress.phase = "Connecting".into();
        progress.indeterminate = true;
    }
    let job = Job {
        id,
        visible: spec.title.is_some(),
        title: spec.title.unwrap_or_default(),
        work: spec.work.clone(),
        after: spec.after,
        retry: spec.retry,
        status: JobStatus::Running,
        progress,
        elapsed_ms: 0,
        rate: 0.0,
        sample: (0, 0),
    };
    model.jobs.list.push(job);
    // Keep the finished history short.
    let finished: Vec<JobId> = model.jobs.visible().filter(|j| !j.is_running()).map(|j| j.id).collect();
    if finished.len() > 20 {
        let drop = &finished[..finished.len() - 20];
        model.jobs.list.retain(|j| !drop.contains(&j.id));
    }
    let op = JobOp::Start { id, work: spec.work };
    out.push(Command::stream_from_shell(op).then_send(move |update| shell(Reply::Job { id, update })));
    id
}

pub fn input(model: &mut Model, input: JobsInput, out: &mut Out) {
    match input {
        JobsInput::Cancel(id) => {
            if model.jobs.get(id).is_some_and(Job::is_running) {
                out.push(Command::notify_shell(JobOp::Cancel { id }).into());
            }
        }
        JobsInput::ClearFinished => model.jobs.list.retain(Job::is_running),
        JobsInput::SheetShown(open) => model.jobs_open = open,
    }
}

/// Overrides the "Done" a finished job shows.
pub fn set_done_message(model: &mut Model, id: JobId, message: impl Into<String>) {
    if let Some(job) = model.jobs.get_mut(id) {
        job.status = JobStatus::Done(message.into());
    }
}

pub fn set_failed(model: &mut Model, id: JobId, message: impl Into<String>) {
    if let Some(job) = model.jobs.get_mut(id) {
        job.status = JobStatus::Failed(message.into());
    }
}

pub fn update(model: &mut Model, id: JobId, update: JobUpdate, out: &mut Out) {
    match update {
        JobUpdate::Progress { state, elapsed_ms } => {
            let Some(job) = model.jobs.get_mut(id) else { return };
            if !job.is_running() {
                return;
            }
            let since = elapsed_ms.saturating_sub(job.sample.0);
            if since >= 500 {
                let delta = state.bytes_done.saturating_sub(job.sample.1) as f64;
                let current = delta / (since as f64 / 1000.0);
                job.rate = if job.rate == 0.0 { current } else { job.rate * 0.6 + current * 0.4 };
                job.sample = (elapsed_ms, state.bytes_done);
            }
            job.progress = state;
            job.elapsed_ms = elapsed_ms;
        }
        JobUpdate::Conflict { ask, conflict } => dialog::conflict(model, id, ask, conflict),
        JobUpdate::Finished(result) => finished(model, id, result, out),
    }
}

fn finished(model: &mut Model, id: JobId, result: VfsResult<JobOutput>, out: &mut Out) {
    dialog::job_finished(model, id);
    let Some(job) = model.jobs.get_mut(id) else { return };
    job.status = match &result {
        Ok(_) => JobStatus::Done("Done".into()),
        Err(VfsError::Cancelled) => JobStatus::Cancelled,
        Err(err) => JobStatus::Failed(err.to_string()),
    };
    let job = job.clone();
    if !job.visible {
        model.jobs.list.retain(|j| j.id != id);
    }
    if let Err(VfsError::PermissionDenied(msg)) = &result
        && job.retry
        && let Some((conn, false)) = job.work.elevation_target()
        && model.conns.caps(conn).elevation
    {
        let label = model.conns.label(conn);
        dialog::ask_confirm(
            model,
            "Permission denied",
            format!("{msg}\n\nRetry as root with sudo on {label}?"),
            "Retry with sudo",
            false,
            ConfirmPurpose::Retry { job: Box::new(job) },
        );
        return;
    }
    after(model, &job, result, out);
}

fn after(model: &mut Model, job: &Job, result: VfsResult<JobOutput>, out: &mut Out) {
    match &job.after {
        After::Nothing => {}
        After::Ops(then) => ops::finished(model, job, then.clone(), result, out),
        After::SyncCompared(dialog) => sync::compared(model, *dialog, job.id, result, out),
        After::Editor(editor, then) => editor::finished(model, *editor, then.clone(), result, out),
    }
}

/// The user said yes to retrying with sudo.
pub fn retry_elevated(model: &mut Model, job: Job, out: &mut Out) {
    let spec = Spec {
        title: job.visible.then(|| job.title.clone()),
        work: job.work.with_elevation(),
        after: job.after,
        retry: false,
    };
    start(model, spec, out);
}

/// The user said no to retrying with sudo: the job's follow-up hears about
/// the failure after all.
pub fn give_up(model: &mut Model, job: Job, out: &mut Out) {
    let message = match &job.status {
        JobStatus::Failed(message) => message.clone(),
        _ => "Permission denied".into(),
    };
    let message = message.strip_prefix("Permission denied: ").unwrap_or(&message).to_string();
    after(model, &job, Err(VfsError::PermissionDenied(message)), out);
}
