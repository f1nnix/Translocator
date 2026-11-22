//! The Properties dialog (⌘I).
//!
//! Called by the dialog host ([`crate::dialogs`]) for every render of a
//! `DialogKind::Properties` dialog in `view.dialogs`. The host has already
//! set `on_ok` → `DialogInput::Submit` (the core turns it into
//! `PropertiesInput::Apply`) and `on_cancel` → `DialogInput::Cancel`, both
//! returning `false`.
//!
//! Every edit is `Event::Properties(id, PropertiesInput::..)`; what to show
//! is in `state`. No file system access here: the shell only writes the
//! modification time in local time.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{Dialog, DialogFooter};
use gpui_kit::component::input::Input;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{prelude::*, *};
use tl_app::dialog::DialogInput;
use tl_app::properties::{
    COLUMNS, DetailValue, PropertiesDialog, PropertiesInput, RECURSIVE_LABEL, ROWS, SPECIALS,
};
use tl_app::{DialogId, Event};

use crate::app_state::dispatch;
use crate::rename::CoreInput;
use crate::util::format_date_long;

/// The Owner and Group fields, kept while the dialog is rendered.
struct Fields {
    owner: CoreInput,
    group: CoreInput,
}

fn send(id: DialogId, input: PropertiesInput, cx: &App) {
    dispatch(Event::Properties(id, input), cx);
}

pub fn render(
    dialog: Dialog,
    state: &PropertiesDialog,
    id: DialogId,
    window: &mut Window,
    cx: &mut App,
) -> Dialog {
    let fields = window.use_keyed_state(("properties", id.0), cx, |window, cx| Fields {
        owner: CoreInput::new(
            &state.owner,
            |s| s,
            move |text, cx| send(id, PropertiesInput::Owner(text), cx),
            window,
            cx,
        ),
        group: CoreInput::new(
            &state.group,
            |s| s,
            move |text, cx| send(id, PropertiesInput::Group(text), cx),
            window,
            cx,
        ),
    });
    let (owner, group) = {
        let f = fields.read(cx);
        (f.owner.clone(), f.group.clone())
    };
    owner.sync(&state.owner, window, cx);
    group.sync(&state.group, window, cx);

    let theme = cx.theme().clone();
    let details = v_flex().gap_1().children(state.details.iter().map(|d| {
        let value = match &d.value {
            DetailValue::Text(text) => text.clone(),
            DetailValue::Date(mtime) => format_date_long(*mtime),
        };
        h_flex()
            .gap_2()
            .text_sm()
            .child(
                div()
                    .w(px(90.))
                    .flex_shrink_0()
                    .text_color(theme.muted_foreground)
                    .child(d.label),
            )
            .child(div().flex_1().min_w_0().whitespace_normal().child(value))
    }));

    let can_chmod = state.can_chmod;
    let cell = |bit: u32| {
        Checkbox::new(("perm", bit as usize))
            .checked(state.has(bit))
            .disabled(!can_chmod)
            .on_click(move |checked: &bool, _, cx| {
                send(id, PropertiesInput::Bit { bit, on: *checked }, cx)
            })
    };
    let permissions = v_flex()
        .gap_2()
        .child(
            h_flex()
                .gap_4()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(div().w(px(70.)))
                .children(COLUMNS.iter().map(|c| div().w(px(50.)).child(*c))),
        )
        .children(ROWS.iter().map(|(label, bits)| {
            h_flex()
                .gap_4()
                .text_sm()
                .child(div().w(px(70.)).child(*label))
                .children(bits.iter().map(|bit| div().w(px(50.)).child(cell(*bit))))
        }))
        .child(
            h_flex()
                .gap_4()
                .children(SPECIALS.iter().map(|(bit, label)| {
                    let bit = *bit;
                    Checkbox::new(("perm-special", bit as usize))
                        .label(*label)
                        .checked(state.has(bit))
                        .disabled(!can_chmod)
                        .on_click(move |checked: &bool, _, cx| {
                            send(id, PropertiesInput::Bit { bit, on: *checked }, cx)
                        })
                })),
        )
        .child(
            div()
                .text_sm()
                .font_family(theme.mono_font_family.clone())
                .text_color(theme.muted_foreground)
                .child(state.mode_text()),
        );

    dialog
        .title(SharedString::from(state.title.clone()))
        .width(px(520.))
        .child(
            v_flex()
                .gap_4()
                .child(details)
                .child(div().h(px(1.)).bg(theme.border))
                .child(permissions)
                .when(state.can_chown, |el| {
                    el.child(
                        h_flex()
                            .gap_2()
                            .child(div().text_sm().w(px(50.)).child("Owner"))
                            .child(div().w(px(150.)).child(Input::new(&owner.input).small()))
                            .child(div().text_sm().w(px(50.)).child("Group"))
                            .child(div().w(px(150.)).child(Input::new(&group.input).small())),
                    )
                })
                .when(state.any_dir, |el| {
                    el.child(
                        Checkbox::new("recursive")
                            .label(RECURSIVE_LABEL)
                            .checked(state.recursive)
                            .on_click(move |checked: &bool, _, cx| {
                                send(id, PropertiesInput::Recursive(*checked), cx)
                            }),
                    )
                })
                .when_some(state.elevated_note(), |el, note| {
                    el.child(div().text_xs().text_color(theme.danger).child(note))
                }),
        )
        .footer(
            DialogFooter::new()
                .child(
                    Button::new("p-cancel")
                        .label("Cancel")
                        .on_click(move |_, _, cx| {
                            dispatch(Event::Dialog(id, DialogInput::Cancel), cx)
                        }),
                )
                .child(
                    Button::new("p-apply")
                        .label("Apply")
                        .primary()
                        .on_click(move |_, _, cx| send(id, PropertiesInput::Apply, cx)),
                ),
        )
}
