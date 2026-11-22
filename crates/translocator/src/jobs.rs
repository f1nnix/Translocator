//! The jobs sheet: background jobs (copy, move, delete, commands) with
//! their live progress, as the core lists them.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress as ProgressBar;
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{prelude::*, *};
use tl_app::Event;
use tl_app::jobs::{JobStatus, JobsInput};
use tl_app::view::JobView;

use crate::app_state::{Shell, dispatch, view};

/// The list in the jobs sheet.
pub struct JobsList {
    jobs: Vec<JobView>,
    _sub: Subscription,
}

impl JobsList {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let sub = cx.observe_global::<Shell>(|this, cx| {
            let view = view(cx);
            if view.jobs != this.jobs {
                this.jobs = view.jobs.clone();
                cx.notify();
            }
        });
        JobsList {
            jobs: view(cx).jobs.clone(),
            _sub: sub,
        }
    }
}

/// Asks the core to stop a job.
pub fn cancel(job: &JobView, cx: &App) {
    dispatch(Event::Jobs(JobsInput::Cancel(job.id)), cx);
}

/// One job as a compact row (title, bar, numbers, cancel).
pub fn render_job(job: &JobView, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme().clone();
    let running = job.is_running();
    let status_color = match job.status {
        JobStatus::Failed(_) => theme.danger,
        JobStatus::Cancelled => theme.warning,
        JobStatus::Done(_) => theme.success,
        JobStatus::Running => theme.muted_foreground,
    };
    let id = job.id;
    v_flex()
        .gap_1()
        .py_1()
        .child(
            h_flex()
                .gap_2()
                .child(div().flex_1().min_w_0().truncate().text_sm().child(job.title.clone()))
                .when(running, |el| {
                    el.child(
                        Button::new(("cancel-job", id.0 as usize))
                            .icon(IconName::Close)
                            .ghost()
                            .xsmall()
                            .tooltip("Cancel")
                            .on_click(move |_, _, cx| dispatch(Event::Jobs(JobsInput::Cancel(id)), cx)),
                    )
                }),
        )
        .when(running, |el| {
            el.child(
                ProgressBar::new(("job-progress", id.0 as usize))
                    .value(job.fraction.map(|f| f * 100.0).unwrap_or(0.0))
                    .loading(job.fraction.is_none())
                    .xsmall(),
            )
        })
        .child(
            div()
                .text_xs()
                .text_color(status_color)
                .truncate()
                .child(job.detail.clone()),
        )
        .when(running && !job.current.is_empty(), |el| {
            el.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(job.current.clone()),
            )
        })
}

impl Render for JobsList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .gap_1()
            .when(self.jobs.is_empty(), |el| {
                el.child(
                    div()
                        .py_4()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("No jobs yet. Copies, moves and deletions run here."),
                )
            })
            // Newest first.
            .children(self.jobs.iter().rev().map(|job| {
                div()
                    .border_b_1()
                    .border_color(theme.border)
                    .pb_1()
                    .child(render_job(job, cx))
            }))
            .when(self.jobs.iter().any(|j| !j.is_running()), |el| {
                el.child(
                    h_flex().justify_end().pt_2().child(
                        Button::new("clear-jobs")
                            .label("Clear Finished")
                            .ghost()
                            .small()
                            .on_click(|_, _, cx| dispatch(Event::Jobs(JobsInput::ClearFinished), cx)),
                    ),
                )
            })
    }
}
