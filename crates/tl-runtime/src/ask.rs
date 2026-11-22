//! Questions from worker threads to the user: provider prompts (passwords,
//! host keys) and transfer conflicts. The worker sends the question to the
//! runtime thread, which hands it to the core, and blocks until the answer
//! comes back the same way. Shutdown, a cancelled job or a runtime that is
//! gone all unblock it with the "no" answer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;

use tl_app::{JobId, PromptAnswer, PromptId, PromptKind, PromptRequest};
use tl_core::transfer::{Conflict, ConflictAnswer, ConflictChoice, ConflictResolver};
use tl_core::{CancelToken, Prompter};

use crate::exec::Inbox;

/// The prompter handed to every plugin (and to the askpass server).
pub(crate) struct RuntimePrompter {
    inbox: Inbox,
    next: AtomicU64,
    closed: Arc<AtomicBool>,
}

impl RuntimePrompter {
    pub fn new(inbox: Inbox, closed: Arc<AtomicBool>) -> Self {
        RuntimePrompter {
            inbox,
            next: AtomicU64::new(1),
            closed,
        }
    }

    fn ask(&self, kind: PromptKind) -> Option<PromptAnswer> {
        if self.closed.load(Ordering::SeqCst) {
            return None;
        }
        let id = PromptId(self.next.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = mpsc::channel();
        if !self.inbox.task(move |rt| rt.prompt_ask(PromptRequest { id, kind }, tx)) {
            return None;
        }
        let _waiting = self.inbox.activity().waiting();
        rx.recv().ok()
    }
}

impl Prompter for RuntimePrompter {
    fn secret(&self, title: &str, message: &str) -> Option<String> {
        match self.ask(PromptKind::Secret {
            title: title.into(),
            message: message.into(),
        }) {
            Some(PromptAnswer::Secret(answer)) => answer,
            _ => None,
        }
    }

    fn confirm(&self, title: &str, message: &str) -> bool {
        matches!(
            self.ask(PromptKind::Confirm {
                title: title.into(),
                message: message.into(),
            }),
            Some(PromptAnswer::Confirm(true))
        )
    }
}

/// Asks the core about each name clash of one job.
pub(crate) struct JobResolver {
    job: JobId,
    inbox: Inbox,
    cancel: CancelToken,
    next: AtomicU64,
}

impl JobResolver {
    pub fn new(job: JobId, inbox: Inbox, cancel: CancelToken) -> Self {
        JobResolver {
            job,
            inbox,
            cancel,
            next: AtomicU64::new(1),
        }
    }
}

fn cancelled() -> ConflictAnswer {
    ConflictAnswer::new(ConflictChoice::Cancel)
}

impl ConflictResolver for JobResolver {
    fn resolve(&self, conflict: &Conflict) -> ConflictAnswer {
        if self.cancel.is_cancelled() {
            return cancelled();
        }
        let ask = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let (job, conflict) = (self.job, conflict.clone());
        if !self.inbox.task(move |rt| rt.job_conflict(job, ask, conflict, tx)) {
            return cancelled();
        }
        let _waiting = self.inbox.activity().waiting();
        rx.recv().unwrap_or_else(|_| cancelled())
    }
}
