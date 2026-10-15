//! Background jobs (copy, move, delete, commands) with live progress.

use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress as ProgressBar;
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{prelude::*, *};
use tl_core::util::human_size;
use tl_core::{CancelToken, Progress, ProgressState};

#[derive(Clone, Debug, PartialEq)]
pub enum JobStatus {
    Running,
    Done(SharedString),
    Failed(SharedString),
    Cancelled,
}

pub struct Job {
    pub id: usize,
    pub title: SharedString,
    pub progress: Progress,
    pub cancel: CancelToken,
    pub status: JobStatus,
    started: Instant,
    /// Last (time, bytes) sample, for the transfer rate.
    sample: (Instant, u64),
    rate: f64,
}

impl Job {
    pub fn is_running(&self) -> bool {
        self.status == JobStatus::Running
    }

    fn update_rate(&mut self, snap: &ProgressState) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.sample.0).as_secs_f64();
        if elapsed >= 0.5 {
            let delta = snap.bytes_done.saturating_sub(self.sample.1) as f64;
            let current = delta / elapsed;
            self.rate = if self.rate == 0.0 { current } else { self.rate * 0.6 + current * 0.4 };
            self.sample = (now, snap.bytes_done);
        }
    }
}

pub struct Jobs {
    pub list: Vec<Job>,
    next_id: usize,
    ticker: Option<Task<()>>,
}

impl Jobs {
    pub fn new() -> Self {
        Jobs {
            list: Vec::new(),
            next_id: 1,
            ticker: None,
        }
    }

    pub fn start(&mut self, title: impl Into<SharedString>, cx: &mut Context<Self>) -> (usize, Progress, CancelToken) {
        let id = self.next_id;
        self.next_id += 1;
        let progress = Progress::new();
        let cancel = CancelToken::new();
        let now = Instant::now();
        self.list.push(Job {
            id,
            title: title.into(),
            progress: progress.clone(),
            cancel: cancel.clone(),
            status: JobStatus::Running,
            started: now,
            sample: (now, 0),
            rate: 0.0,
        });
        // Keep the finished history short.
        let finished: Vec<usize> = self
            .list
            .iter()
            .filter(|j| !j.is_running())
            .map(|j| j.id)
            .collect();
        if finished.len() > 20 {
            let drop: Vec<usize> = finished[..finished.len() - 20].to_vec();
            self.list.retain(|j| !drop.contains(&j.id));
        }
        self.ensure_ticker(cx);
        cx.notify();
        (id, progress, cancel)
    }

    fn ensure_ticker(&mut self, cx: &mut Context<Self>) {
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                let running = this
                    .update(cx, |this, cx| {
                        for job in this.list.iter_mut().filter(|j| j.is_running()) {
                            let snap = job.progress.snapshot();
                            job.update_rate(&snap);
                        }
                        cx.notify();
                        this.running() > 0
                    })
                    .unwrap_or(false);
                if !running {
                    let _ = this.update(cx, |this, _| this.ticker = None);
                    break;
                }
            }
        }));
    }

    pub fn finish(&mut self, id: usize, status: JobStatus, cx: &mut Context<Self>) {
        if let Some(job) = self.list.iter_mut().find(|j| j.id == id) {
            job.status = status;
        }
        cx.notify();
    }

    pub fn cancel(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(job) = self.list.iter().find(|j| j.id == id) {
            job.cancel.cancel();
        }
        cx.notify();
    }

    pub fn running(&self) -> usize {
        self.list.iter().filter(|j| j.is_running()).count()
    }

    pub fn clear_finished(&mut self, cx: &mut Context<Self>) {
        self.list.retain(Job::is_running);
        cx.notify();
    }

    /// One job as a compact row (title, bar, numbers, cancel).
    pub fn render_job(&self, job: &Job, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let snap = job.progress.snapshot();
        let fraction = snap.fraction();
        let id = job.id;
        let detail: SharedString = match &job.status {
            JobStatus::Running => {
                let mut parts = Vec::new();
                if !snap.phase.is_empty() {
                    parts.push(snap.phase.clone());
                }
                if snap.bytes_total > 0 {
                    parts.push(format!(
                        "{} of {}",
                        human_size(snap.bytes_done),
                        human_size(snap.bytes_total)
                    ));
                } else if snap.items_total > 0 {
                    parts.push(format!("{} of {} items", snap.items_done, snap.items_total));
                }
                if job.rate > 1024.0 {
                    parts.push(format!("{}/s", human_size(job.rate as u64)));
                }
                let elapsed = job.started.elapsed().as_secs();
                if elapsed >= 2 {
                    parts.push(format!("{}:{:02}", elapsed / 60, elapsed % 60));
                }
                parts.join(" · ").into()
            }
            JobStatus::Done(msg) => msg.clone(),
            JobStatus::Failed(msg) => msg.clone(),
            JobStatus::Cancelled => "Cancelled".into(),
        };
        let current: SharedString = snap.current.clone().into();
        let status_color = match job.status {
            JobStatus::Failed(_) => theme.danger,
            JobStatus::Cancelled => theme.warning,
            JobStatus::Done(_) => theme.success,
            JobStatus::Running => theme.muted_foreground,
        };
        v_flex()
            .gap_1()
            .py_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(job.title.clone()))
                    .when(job.is_running(), |el| {
                        el.child(
                            Button::new(("cancel-job", id))
                                .icon(IconName::Close)
                                .ghost()
                                .xsmall()
                                .tooltip("Cancel")
                                .on_click(cx.listener(move |this, _, _, cx| this.cancel(id, cx))),
                        )
                    }),
            )
            .when(job.is_running(), |el| {
                el.child(
                    ProgressBar::new(("job-progress", id))
                        .value(fraction.map(|f| f * 100.0).unwrap_or(0.0))
                        .loading(fraction.is_none())
                        .xsmall(),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(status_color)
                    .truncate()
                    .child(detail),
            )
            .when(job.is_running() && !current.is_empty(), |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(current),
                )
            })
    }
}

impl Render for Jobs {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let jobs: Vec<usize> = (0..self.list.len()).rev().collect();
        v_flex()
            .gap_1()
            .when(self.list.is_empty(), |el| {
                el.child(
                    div()
                        .py_4()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("No jobs yet. Copies, moves and deletions run here."),
                )
            })
            .children(jobs.into_iter().map(|ix| {
                let job = &self.list[ix];
                let row = self.render_job(job, cx);
                div()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .pb_1()
                    .child(row)
            }))
            .when(self.list.iter().any(|j| !j.is_running()), |el| {
                el.child(
                    h_flex().justify_end().pt_2().child(
                        Button::new("clear-jobs")
                            .label("Clear Finished")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| this.clear_finished(cx))),
                    ),
                )
            })
    }
}
