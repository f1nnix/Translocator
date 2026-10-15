//! Lets worker threads ask the user something.
//!
//! Providers and the transfer engine run on worker threads and block while
//! waiting for an answer (an SSH password, a host key confirmation, how to
//! resolve a name conflict). Requests travel over a channel to the UI
//! thread, which shows a dialog in the active window and sends the answer
//! back.

use std::sync::mpsc;

use gpui_kit::*;
use tl_core::Prompter;
use tl_core::transfer::{Conflict, ConflictAnswer, ConflictChoice, ConflictResolver};

use crate::app_state::AppState;
use crate::dialogs::{self, Reply};

pub enum UiRequest {
    Secret {
        title: String,
        message: String,
        reply: mpsc::Sender<Option<String>>,
    },
    Confirm {
        title: String,
        message: String,
        reply: mpsc::Sender<bool>,
    },
    Conflict {
        conflict: Box<Conflict>,
        reply: mpsc::Sender<ConflictAnswer>,
    },
}

#[derive(Clone)]
pub struct UiBridge {
    tx: smol::channel::Sender<UiRequest>,
}

pub fn start(cx: &mut App) -> UiBridge {
    let (tx, rx) = smol::channel::unbounded::<UiRequest>();
    cx.spawn(async move |cx| {
        while let Ok(request) = rx.recv().await {
            cx.update(|cx| present(request, cx));
        }
    })
    .detach();
    UiBridge { tx }
}

fn present(request: UiRequest, cx: &mut App) {
    let target = cx
        .active_window()
        .or_else(|| cx.try_global::<AppState>().and_then(|s| s.main_window));
    let Some(target) = target else {
        return;
    };
    // Password prompts may arrive while the app is in the background.
    cx.activate(true);
    let _ = target.update(cx, move |_, window, cx| match request {
        UiRequest::Secret {
            title,
            message,
            reply,
        } => dialogs::secret(
            window,
            cx,
            title,
            message,
            Reply::new(None, move |v| {
                let _ = reply.send(v);
            }),
        ),
        UiRequest::Confirm {
            title,
            message,
            reply,
        } => dialogs::confirm(
            window,
            cx,
            title,
            message,
            "Continue".into(),
            false,
            Reply::new(false, move |v| {
                let _ = reply.send(v);
            }),
        ),
        UiRequest::Conflict { conflict, reply } => dialogs::conflict(
            window,
            cx,
            *conflict,
            Reply::new(ConflictAnswer::new(ConflictChoice::Cancel), move |v| {
                let _ = reply.send(v);
            }),
        ),
    });
}

impl Prompter for UiBridge {
    fn secret(&self, title: &str, message: &str) -> Option<String> {
        let (reply, answer) = mpsc::channel();
        self.tx
            .send_blocking(UiRequest::Secret {
                title: title.to_string(),
                message: message.to_string(),
                reply,
            })
            .ok()?;
        answer.recv().ok().flatten()
    }

    fn confirm(&self, title: &str, message: &str) -> bool {
        let (reply, answer) = mpsc::channel();
        if self
            .tx
            .send_blocking(UiRequest::Confirm {
                title: title.to_string(),
                message: message.to_string(),
                reply,
            })
            .is_err()
        {
            return false;
        }
        answer.recv().unwrap_or(false)
    }
}

impl ConflictResolver for UiBridge {
    fn resolve(&self, conflict: &Conflict) -> ConflictAnswer {
        let (reply, answer) = mpsc::channel();
        if self
            .tx
            .send_blocking(UiRequest::Conflict {
                conflict: Box::new(conflict.clone()),
                reply,
            })
            .is_err()
        {
            return ConflictAnswer::new(ConflictChoice::Cancel);
        }
        answer
            .recv()
            .unwrap_or_else(|_| ConflictAnswer::new(ConflictChoice::Cancel))
    }
}
