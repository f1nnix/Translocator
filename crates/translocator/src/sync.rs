//! The Synchronize Directories dialog (⌘⌥S).
//!
//! Called by the dialog host ([`crate::dialogs`]) for every render of a
//! `DialogKind::Sync` dialog in `view.dialogs`. The host has already set
//! `on_ok` → `DialogInput::Submit` (the core turns it into
//! `SyncInput::Synchronize`) and `on_cancel` → `DialogInput::Cancel` (the
//! core stops a comparison still running), both returning `false`.
//!
//! Every click is `Event::Sync(id, SyncInput::..)`; the plan, comparing
//! state and errors are in `state`. No file system access here: the shell
//! only writes the dates in local time.

use std::ops::Range;
use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::Dialog;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _, Theme,
    h_flex, v_flex,
};
use gpui_kit::{prelude::*, *};
use tl_app::dialog::DialogInput;
use tl_app::settings::DateFormat;
use tl_app::sync::{
    ActionIcon, Body, DIRECTIONS, SyncDialog, SyncInput, TITLE, action_button, is_dir, is_newer,
    side_text, state_label,
};
use tl_app::tl_model::sync::SyncSide;
use tl_app::{DialogId, Event};

use crate::app_state::{dispatch, view};
use crate::util::format_date;

fn send(id: DialogId, input: SyncInput, cx: &App) {
    dispatch(Event::Sync(id, input), cx);
}

pub fn render(
    dialog: Dialog,
    state: &SyncDialog,
    id: DialogId,
    _window: &mut Window,
    cx: &mut App,
) -> Dialog {
    let theme = cx.theme().clone();
    let dates = view(cx).settings.general.date_format;
    let direction_ix = DIRECTIONS
        .iter()
        .position(|(d, _)| *d == state.options.direction)
        .unwrap_or(0);

    let end = |label: &'static str, display: &str| {
        h_flex()
            .gap_2()
            .child(
                div()
                    .w(px(46.))
                    .text_color(theme.muted_foreground)
                    .child(label),
            )
            .child(display.to_string())
    };
    let ends = v_flex()
        .gap_1()
        .text_sm()
        .child(end("Left", &state.left_display))
        .child(end("Right", &state.right_display));

    let options = h_flex()
        .gap_3()
        .child(
            Checkbox::new("sync-recursive")
                .label("Subfolders")
                .checked(state.options.recursive)
                .on_click(move |checked: &bool, _, cx| {
                    send(id, SyncInput::Recursive(*checked), cx)
                }),
        )
        .child(
            ButtonGroup::new("sync-direction")
                .xsmall()
                .outline()
                .children(DIRECTIONS.iter().enumerate().map(|(ix, (_, name))| {
                    Button::new(("direction", ix))
                        .label(*name)
                        .selected(ix == direction_ix)
                }))
                .on_click(move |selected: &Vec<usize>, _, cx| {
                    if let Some((direction, _)) =
                        selected.first().and_then(|ix| DIRECTIONS.get(*ix))
                    {
                        send(id, SyncInput::Direction(*direction), cx);
                    }
                }),
        )
        .child(
            Checkbox::new("sync-same")
                .label("Show identical")
                .checked(state.show_same)
                .on_click(move |checked: &bool, _, cx| send(id, SyncInput::ShowSame(*checked), cx)),
        )
        .child(div().flex_1())
        .child(
            Button::new("sync-compare")
                .icon(Lucide::RefreshCw)
                .label("Compare Again")
                .small()
                .disabled(!state.can_compare())
                .on_click(move |_, _, cx| send(id, SyncInput::CompareAgain, cx)),
        );

    let body = match state.body() {
        Body::Comparing(text) => v_flex()
            .h(px(380.))
            .items_center()
            .justify_center()
            .gap_2()
            .text_color(theme.muted_foreground)
            .child(Icon::new(IconName::LoaderCircle))
            .child(div().text_sm().child(text))
            .into_any_element(),
        Body::Error(error) => div()
            .h(px(380.))
            .p_3()
            .text_sm()
            .text_color(theme.danger)
            .child(error)
            .into_any_element(),
        Body::Empty(text) => v_flex()
            .h(px(380.))
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(text)
            .into_any_element(),
        Body::List => {
            let rows = Rc::new(state.clone());
            let count = state.shown.len();
            let theme = theme.clone();
            div()
                .h(px(380.))
                .border_1()
                .border_color(theme.border)
                .rounded_md()
                .child(
                    uniform_list("sync-items", count, move |range: Range<usize>, _, _| {
                        render_rows(&rows, range, id, &theme, dates)
                    })
                    .size_full(),
                )
                .into_any_element()
        }
    };

    dialog.title(TITLE).width(px(1000.)).child(
        v_flex()
            .gap_3()
            .child(ends)
            .child(options)
            .child(body)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        v_flex().flex_1().text_sm().child(state.summary()).child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(state.note()),
                        ),
                    )
                    .child(
                        Button::new("sync-cancel")
                            .label("Cancel")
                            .on_click(move |_, _, cx| {
                                dispatch(Event::Dialog(id, DialogInput::Cancel), cx)
                            }),
                    )
                    .child(
                        Button::new("sync-apply")
                            .label("Synchronize")
                            .primary()
                            .disabled(!state.can_synchronize())
                            .on_click(move |_, _, cx| send(id, SyncInput::Synchronize, cx)),
                    ),
            ),
    )
}

fn render_rows(
    state: &SyncDialog,
    range: Range<usize>,
    id: DialogId,
    theme: &Theme,
    dates: DateFormat,
) -> Vec<AnyElement> {
    range
        .filter_map(|row| {
            let (ix, item) = state.row(row)?;
            let (icon, tip) = action_button(item);
            let (icon, color) = match icon {
                ActionIcon::ArrowRight => (Lucide::ArrowRight, theme.primary),
                ActionIcon::ArrowLeft => (Lucide::ArrowLeft, theme.primary),
                ActionIcon::Equal => (Lucide::Equal, theme.muted_foreground),
                ActionIcon::TriangleAlert => (Lucide::TriangleAlert, theme.warning),
                ActionIcon::Minus => (Lucide::Minus, theme.muted_foreground),
            };
            let is_dir = is_dir(item);
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
                            .on_click(move |_, _, cx| send(id, SyncInput::Cycle(ix), cx)),
                    )
                    .child(
                        Icon::new(if is_dir { Lucide::Folder } else { Lucide::File })
                            .size(px(14.))
                            .text_color(if is_dir {
                                theme.blue
                            } else {
                                theme.muted_foreground
                            }),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(rel))
                    .child(
                        div()
                            .w(px(76.))
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(state_label(item.state)),
                    )
                    .child(render_side(
                        item.left.as_ref(),
                        is_newer(item, true),
                        theme,
                        dates,
                    ))
                    .child(render_side(
                        item.right.as_ref(),
                        is_newer(item, false),
                        theme,
                        dates,
                    ))
                    .into_any_element(),
            )
        })
        .collect()
}

fn render_side(
    side: Option<&SyncSide>,
    newer: bool,
    theme: &Theme,
    dates: DateFormat,
) -> AnyElement {
    let Some(side) = side else {
        return div().w(px(190.)).flex_shrink_0().into_any_element();
    };
    let (mut text, mtime) = side_text(side);
    if let Some(mtime) = mtime {
        text = format!("{text} · {}", format_date(Some(mtime), dates));
    }
    div()
        .w(px(190.))
        .flex_shrink_0()
        .truncate()
        .text_xs()
        .text_color(if newer {
            theme.success
        } else {
            theme.muted_foreground
        })
        .child(text)
        .into_any_element()
}
