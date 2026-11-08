//! Synchronize directories (⌘⌥S): compares the two panes' folders, shows
//! what would be copied each way — each item's direction can be changed by
//! clicking it — then copies through the transfer engine as a job, so a
//! sync between two folders on one server runs on the server.

use std::ops::Range;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::{prelude::*, *};
use tl_core::sync::{self, SyncAction, SyncDirection, SyncEnd, SyncItem, SyncOptions, SyncPlan, SyncSide, SyncState};
use tl_core::util::human_size;
use tl_core::{CancelToken, VfsError};

use crate::pane::Location;
use crate::util::{blocking, format_date};
use crate::workspace::Workspace;

const DIRECTIONS: [(SyncDirection, &str); 3] = [
    (SyncDirection::Both, "Both ways"),
    (SyncDirection::LeftToRight, "Left → right"),
    (SyncDirection::RightToLeft, "Right → left"),
];

pub fn open(left: SyncEnd, right: SyncEnd, workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| {
        let mut view = SyncView::new(left, right, workspace);
        view.compare(cx);
        view
    });
    window.open_dialog(cx, move |dialog, _window, _cx| {
        let (closing, ok) = (view.clone(), view.clone());
        dialog
            .title("Synchronize Directories")
            .width(px(1000.))
            .child(view.clone())
            // Enter synchronizes once the comparison is done.
            .on_ok(move |_, window, cx| {
                ok.update(cx, |v, cx| v.synchronize(window, cx));
                false
            })
            .on_close(move |_, _, cx| closing.update(cx, |v, _| v.stop()))
    });
}

fn display(end: &SyncEnd) -> String {
    Location {
        vfs: end.vfs.clone(),
        path: end.dir.clone(),
    }
    .display()
}

struct SyncView {
    left: SyncEnd,
    right: SyncEnd,
    options: SyncOptions,
    plan: Option<SyncPlan>,
    /// Set while a comparison runs.
    comparing: Option<CancelToken>,
    error: Option<SharedString>,
    show_same: bool,
    /// Indices into the plan's items that are shown.
    shown: Vec<usize>,
    workspace: WeakEntity<Workspace>,
    scroll: UniformListScrollHandle,
}

impl SyncView {
    fn new(left: SyncEnd, right: SyncEnd, workspace: WeakEntity<Workspace>) -> Self {
        let options = SyncOptions {
            recursive: true,
            direction: SyncDirection::Both,
            left_case_insensitive: left.vfs.capabilities().case_insensitive,
            right_case_insensitive: right.vfs.capabilities().case_insensitive,
        };
        SyncView {
            left,
            right,
            options,
            plan: None,
            comparing: None,
            error: None,
            show_same: false,
            shown: Vec::new(),
            workspace,
            scroll: UniformListScrollHandle::new(),
        }
    }

    fn stop(&mut self) {
        if let Some(cancel) = self.comparing.take() {
            cancel.cancel();
        }
    }

    fn compare(&mut self, cx: &mut Context<Self>) {
        self.stop();
        let cancel = CancelToken::new();
        self.comparing = Some(cancel.clone());
        self.error = None;
        let (left, right, options) = (self.left.clone(), self.right.clone(), self.options);
        cx.spawn(async move |this, cx| {
            let token = cancel.clone();
            let result = blocking(move || sync::compare(&left, &right, options, &token)).await;
            let _ = this.update(cx, |this, cx| {
                if this.comparing.as_ref().is_some_and(|c| !c.is_cancelled()) {
                    this.comparing = None;
                    match result {
                        Ok(plan) => this.plan = Some(plan),
                        Err(VfsError::Cancelled) => {}
                        Err(err) => {
                            this.plan = None;
                            this.error = Some(err.to_string().into());
                        }
                    }
                    this.filter();
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn filter(&mut self) {
        let show_same = self.show_same;
        self.shown = self
            .plan
            .as_ref()
            .map(|plan| {
                plan.items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| show_same || item.state != SyncState::Same)
                    .map(|(ix, _)| ix)
                    .collect()
            })
            .unwrap_or_default();
    }

    fn set_direction(&mut self, direction: SyncDirection, cx: &mut Context<Self>) {
        self.options.direction = direction;
        if let Some(plan) = &mut self.plan {
            plan.set_direction(direction);
        }
        cx.notify();
    }

    fn cycle(&mut self, item: usize, cx: &mut Context<Self>) {
        if let Some(item) = self.plan.as_mut().and_then(|p| p.items.get_mut(item)) {
            item.action = item.next_action();
            cx.notify();
        }
    }

    fn synchronize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.comparing.is_some() {
            return;
        }
        let Some(plan) = self.plan.clone() else { return };
        let totals = plan.totals();
        if totals.to_left + totals.to_right == 0 {
            return;
        }
        let (left, right) = (self.left.clone(), self.right.clone());
        window.close_dialog(cx);
        let _ = self
            .workspace
            .update(cx, |ws, cx| ws.start_sync(plan, left, right, window, cx));
    }

    fn render_side(
        side: Option<&SyncSide>,
        newer: bool,
        theme: &gpui_kit::component::Theme,
        dates: crate::settings::DateFormat,
    ) -> AnyElement {
        let Some(side) = side else {
            return div().w(px(190.)).flex_shrink_0().into_any_element();
        };
        let text = if side.is_dir() {
            if side.files > 0 || side.size > 0 {
                format!("folder · {} files, {}", side.files, human_size(side.size))
            } else {
                "folder".to_string()
            }
        } else {
            format!("{} · {}", human_size(side.size), format_date(side.mtime, dates))
        };
        div()
            .w(px(190.))
            .flex_shrink_0()
            .truncate()
            .text_xs()
            .text_color(if newer { theme.success } else { theme.muted_foreground })
            .child(text)
            .into_any_element()
    }

    fn render_rows(&self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let dates = cx.global::<crate::app_state::AppState>().settings.general.date_format;
        let Some(plan) = &self.plan else { return Vec::new() };
        range
            .filter_map(|row| {
                let ix = *self.shown.get(row)?;
                let item: &SyncItem = plan.items.get(ix)?;
                let (icon, color, tip) = match item.action {
                    SyncAction::ToRight => (Lucide::ArrowRight, theme.primary, "Copy to the right (click to change)"),
                    SyncAction::ToLeft => (Lucide::ArrowLeft, theme.primary, "Copy to the left (click to change)"),
                    SyncAction::Skip if item.state == SyncState::Same => (Lucide::Equal, theme.muted_foreground, "Identical"),
                    SyncAction::Skip if item.state == SyncState::Conflict => {
                        (Lucide::TriangleAlert, theme.warning, "Differs, but neither side is newer: click to choose")
                    }
                    SyncAction::Skip => (Lucide::Minus, theme.muted_foreground, "Left alone (click to copy)"),
                };
                let state: SharedString = match item.state {
                    SyncState::OnlyLeft => "only left",
                    SyncState::OnlyRight => "only right",
                    SyncState::LeftNewer => "left newer",
                    SyncState::RightNewer => "right newer",
                    SyncState::Conflict => "conflict",
                    SyncState::Same => "same",
                }
                .into();
                let is_dir = item.left.as_ref().or(item.right.as_ref()).is_some_and(SyncSide::is_dir);
                let rel: SharedString = item.rel_lossy().into();
                Some(
                    h_flex()
                        .id(("sync-row", ix))
                        .h(px(24.))
                        .px_2()
                        .gap_2()
                        .text_size(px(12.))
                        .hover(|s| s.bg(theme.list_hover))
                        .child(
                            Button::new(("sync-dir", ix))
                                .icon(Icon::new(icon).text_color(color))
                                .ghost()
                                .xsmall()
                                .tab_stop(false)
                                .tooltip(tip)
                                .on_click(cx.listener(move |this, _, _, cx| this.cycle(ix, cx))),
                        )
                        .child(
                            Icon::new(if is_dir { Lucide::Folder } else { Lucide::File })
                                .size(px(14.))
                                .text_color(if is_dir { theme.blue } else { theme.muted_foreground }),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(rel))
                        .child(div().w(px(76.)).flex_shrink_0().text_xs().text_color(theme.muted_foreground).child(state))
                        .child(Self::render_side(
                            item.left.as_ref(),
                            item.state == SyncState::LeftNewer,
                            &theme,
                            dates,
                        ))
                        .child(Self::render_side(
                            item.right.as_ref(),
                            item.state == SyncState::RightNewer,
                            &theme,
                            dates,
                        ))
                        .into_any_element(),
                )
            })
            .collect()
    }
}

impl Render for SyncView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let comparing = self.comparing.is_some();
        let direction_ix = DIRECTIONS.iter().position(|(d, _)| *d == self.options.direction).unwrap_or(0);
        let totals = self.plan.as_ref().map(SyncPlan::totals).unwrap_or_default();
        let count = self.shown.len();

        let ends = v_flex()
            .gap_1()
            .text_sm()
            .child(h_flex().gap_2().child(div().w(px(46.)).text_color(theme.muted_foreground).child("Left")).child(display(&self.left)))
            .child(h_flex().gap_2().child(div().w(px(46.)).text_color(theme.muted_foreground).child("Right")).child(display(&self.right)));

        let options = h_flex()
            .gap_3()
            .child(
                Checkbox::new("sync-recursive")
                    .label("Subfolders")
                    .checked(self.options.recursive)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.options.recursive = *checked;
                        this.compare(cx);
                    })),
            )
            .child(
                ButtonGroup::new("sync-direction")
                    .xsmall()
                    .outline()
                    .children(DIRECTIONS.iter().enumerate().map(|(ix, (_, name))| {
                        Button::new(("direction", ix)).label(*name).selected(ix == direction_ix)
                    }))
                    .on_click(cx.listener(|this, selected: &Vec<usize>, _, cx| {
                        if let Some(ix) = selected.first() {
                            this.set_direction(DIRECTIONS[*ix].0, cx);
                        }
                    })),
            )
            .child(
                Checkbox::new("sync-same")
                    .label("Show identical")
                    .checked(self.show_same)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.show_same = *checked;
                        this.filter();
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(
                Button::new("sync-compare")
                    .icon(Lucide::RefreshCw)
                    .label("Compare Again")
                    .small()
                    .disabled(comparing)
                    .on_click(cx.listener(|this, _, _, cx| this.compare(cx))),
            );

        let body = if comparing {
            v_flex()
                .h(px(380.))
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::LoaderCircle))
                .child(div().text_sm().child(if self.options.recursive {
                    "Comparing folders and subfolders…"
                } else {
                    "Comparing folders…"
                }))
                .into_any_element()
        } else if let Some(error) = self.error.clone() {
            div().h(px(380.)).p_3().text_sm().text_color(theme.danger).child(error).into_any_element()
        } else if count == 0 {
            v_flex()
                .h(px(380.))
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if totals.same > 0 { "The folders are in sync." } else { "Both folders are empty." })
                .into_any_element()
        } else {
            div()
                .h(px(380.))
                .border_1()
                .border_color(theme.border)
                .rounded_md()
                .child(
                    uniform_list("sync-items", count, cx.processor(|this, range, _window, cx| this.render_rows(range, cx)))
                        .track_scroll(&self.scroll)
                        .size_full(),
                )
                .into_any_element()
        };

        let mut summary = vec![
            format!("→ {} ({})", totals.to_right, human_size(totals.to_right_bytes)),
            format!("← {} ({})", totals.to_left, human_size(totals.to_left_bytes)),
        ];
        if totals.conflicts > 0 {
            summary.push(format!("{} to decide", totals.conflicts));
        }
        summary.push(format!("{} identical", totals.same));
        let nothing = totals.to_left + totals.to_right == 0;
        let server_side = self.left.vfs.location_key() == self.right.vfs.location_key()
            && self.left.vfs.plugin_id() != tl_local::PLUGIN_ID;
        v_flex()
            .gap_3()
            .child(ends)
            .child(options)
            .child(body)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .text_sm()
                            .child(summary.join(" · "))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(if server_side {
                                "Both folders are on the same server: copies run there."
                            } else {
                                "Newer files win; nothing is deleted. Click an arrow to change what happens to an item."
                            })),
                    )
                    .child(Button::new("sync-cancel").label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(
                        Button::new("sync-apply")
                            .label("Synchronize")
                            .primary()
                            .disabled(comparing || nothing || self.plan.is_none())
                            .on_click(cx.listener(|this, _, window, cx| this.synchronize(window, cx))),
                    ),
            )
    }
}
