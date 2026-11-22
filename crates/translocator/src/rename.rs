//! The Multi-Rename dialog (ctrl-m / ⌘⇧M).
//!
//! Called by the dialog host ([`crate::dialogs`]) for every render of a
//! `DialogKind::Rename` dialog in `view.dialogs`. The host has already set
//! `on_ok` → `DialogInput::Submit` (which the core turns into
//! `RenameInput::Apply`) and `on_cancel` → `DialogInput::Cancel`, both
//! returning `false`: the dialog closes when the core drops it from the
//! view, never by itself.
//!
//! Every edit goes to the core as `Event::Rename(id, RenameInput::..)`; the
//! preview, problems and counts come back in `state` (the core computes them
//! with `tl_model::multirename`). No file system access here.
//!
//! Also home of [`CoreInput`], the text field whose text the core owns,
//! which the other dialogs of the shell share.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::Dialog;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::{prelude::*, *};
use tl_app::dialog::DialogInput;
use tl_app::rename::{CASES, EXT_TOKENS, Field, HELP, NAME_TOKENS, RenameDialog, RenameInput};
use tl_app::{DialogId, Event};

use crate::app_state::dispatch;

/// A text field whose text the core owns: every edit is sent whole, and a
/// change the core makes for its own reasons (a placeholder button, a
/// chosen path) is written back into the field. The core's answers lag the
/// typing, so its echoes of what was sent are told apart from its own
/// changes and never overwrite what the user has typed since.
///
/// Clones share the field. Keep one per dialog and field across renders
/// (`window.use_keyed_state`), so the text and caret survive.
#[derive(Clone)]
pub(crate) struct CoreInput {
    pub input: Entity<InputState>,
    echo: Rc<RefCell<Echo>>,
    _sub: Rc<Subscription>,
}

#[derive(Default)]
struct Echo {
    /// The texts sent that the core has not shown back yet, oldest first.
    pending: VecDeque<String>,
    /// The core's text at the last [`CoreInput::sync`].
    last: String,
}

impl CoreInput {
    /// A field showing `text`; `configure` sets the placeholder, masking…
    /// and `send` carries each edit to the core.
    pub fn new(
        text: &str,
        configure: impl FnOnce(InputState) -> InputState,
        send: impl Fn(String, &mut App) + 'static,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let value = text.to_string();
        let input = cx.new(|cx| configure(InputState::new(window, cx)).default_value(value));
        let echo = Rc::new(RefCell::new(Echo {
            pending: VecDeque::new(),
            last: text.to_string(),
        }));
        let sent = echo.clone();
        let sub = cx.subscribe(&input, move |input, ev: &InputEvent, cx| {
            if let InputEvent::Change = ev {
                let text = input.read(cx).value().to_string();
                sent.borrow_mut().pending.push_back(text.clone());
                send(text, cx);
            }
        });
        CoreInput {
            input,
            echo,
            _sub: Rc::new(sub),
        }
    }

    /// Takes in the core's text: an echo of an edit sent is dropped, a
    /// change of the core's own replaces the field's text.
    pub fn sync(&self, core: &str, window: &mut Window, cx: &mut App) {
        {
            let mut echo = self.echo.borrow_mut();
            if echo.last == core {
                // Caught up with the typing (some echoes may have been
                // skipped): nothing is in flight any more.
                if self.input.read(cx).value() == core {
                    echo.pending.clear();
                }
                return;
            }
            echo.last = core.to_string();
            if let Some(pos) = echo.pending.iter().position(|t| t == core) {
                echo.pending.drain(..=pos);
                return;
            }
            echo.pending.clear();
        }
        if self.input.read(cx).value() != core {
            let text = core.to_string();
            self.input.update(cx, |s, cx| s.set_value(text, window, cx));
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }
}

/// The dialog's text fields, in [`Field::ALL`] order, kept while it is
/// rendered.
struct Fields([CoreInput; 7]);

fn send(id: DialogId, input: RenameInput, cx: &App) {
    dispatch(Event::Rename(id, input), cx);
}

pub fn render(
    dialog: Dialog,
    state: &RenameDialog,
    id: DialogId,
    window: &mut Window,
    cx: &mut App,
) -> Dialog {
    let mut created = false;
    let fields = window.use_keyed_state(("rename", id.0), cx, |window, cx| {
        created = true;
        Fields(Field::ALL.map(|field| {
            CoreInput::new(
                state.text(field),
                |s| s.placeholder(field.placeholder()),
                move |text, cx| send(id, RenameInput::Text(field, text), cx),
                window,
                cx,
            )
        }))
    });
    let fields = fields.read(cx).0.clone();
    for (field, input) in Field::ALL.iter().zip(&fields) {
        input.sync(state.text(*field), window, cx);
    }
    let input_of = |field: Field| -> Entity<InputState> {
        let ix = Field::ALL.iter().position(|f| *f == field).unwrap_or(0);
        fields[ix].input.clone()
    };
    if created {
        input_of(Field::Name).update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
    }

    let theme = cx.theme().clone();
    let label = |text: &'static str| {
        div()
            .w(px(76.))
            .flex_shrink_0()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(text)
    };
    let muted = |text: &'static str| {
        div()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(text)
    };
    let token = |ix: usize, token: &'static str, field: Field| {
        let target = input_of(field);
        Button::new(("rename-token", ix))
            .label(token)
            .xsmall()
            .ghost()
            .tab_stop(false)
            .on_click(move |_, window, cx| {
                send(id, RenameInput::Insert(field, token.to_string()), cx);
                target.update(cx, |s, cx| s.focus(window, cx));
            })
    };
    let case_ix = CASES
        .iter()
        .position(|(c, _)| *c == state.case)
        .unwrap_or(0);

    let masks = v_flex()
        .gap_2()
        .child(
            h_flex()
                .gap_2()
                .child(label("Name"))
                .child(
                    div()
                        .flex_1()
                        .child(Input::new(&input_of(Field::Name)).small()),
                )
                .children(
                    NAME_TOKENS
                        .iter()
                        .enumerate()
                        .map(|(ix, t)| token(ix, t, Field::Name)),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(label("Extension"))
                .child(
                    div()
                        .w(px(180.))
                        .child(Input::new(&input_of(Field::Ext)).small()),
                )
                .children(
                    EXT_TOKENS
                        .iter()
                        .enumerate()
                        .map(|(ix, t)| token(NAME_TOKENS.len() + ix, t, Field::Ext)),
                )
                .child(div().w(px(16.)))
                .child(label("Case"))
                .child(
                    ButtonGroup::new("rename-case")
                        .xsmall()
                        .outline()
                        .children(CASES.iter().enumerate().map(|(ix, (_, name))| {
                            Button::new(("case", ix))
                                .label(*name)
                                .selected(ix == case_ix)
                        }))
                        .on_click(move |selected: &Vec<usize>, _, cx| {
                            if let Some((case, _)) = selected.first().and_then(|ix| CASES.get(*ix))
                            {
                                send(id, RenameInput::Case(*case), cx);
                            }
                        }),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(label("Find"))
                .child(
                    div()
                        .flex_1()
                        .child(Input::new(&input_of(Field::Find)).small()),
                )
                .child(muted("Replace"))
                .child(
                    div()
                        .flex_1()
                        .child(Input::new(&input_of(Field::Replace)).small()),
                )
                .child(
                    Checkbox::new("rename-regex")
                        .label("Regex")
                        .checked(state.regex)
                        .on_click(move |checked: &bool, _, cx| {
                            send(id, RenameInput::Regex(*checked), cx)
                        }),
                )
                .child(
                    Checkbox::new("rename-case-sensitive")
                        .label("Match case")
                        .checked(state.match_case)
                        .on_click(move |checked: &bool, _, cx| {
                            send(id, RenameInput::MatchCase(*checked), cx)
                        }),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(label("Counter"))
                .child(muted("start"))
                .child(
                    div()
                        .w(px(64.))
                        .child(Input::new(&input_of(Field::Start)).small()),
                )
                .child(muted("step"))
                .child(
                    div()
                        .w(px(64.))
                        .child(Input::new(&input_of(Field::Step)).small()),
                )
                .child(muted("digits"))
                .child(
                    div()
                        .w(px(52.))
                        .child(Input::new(&input_of(Field::Digits)).small()),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child(HELP),
        );

    let list = match state.error() {
        Some(error) => div()
            .h(px(300.))
            .p_3()
            .text_sm()
            .text_color(theme.danger)
            .child(error.to_string())
            .into_any_element(),
        None => {
            let preview = state.preview.clone();
            let count = state.rows().len();
            let theme = theme.clone();
            div()
                .h(px(300.))
                .border_1()
                .border_color(theme.border)
                .rounded_md()
                .child(
                    uniform_list(
                        "rename-preview",
                        count,
                        move |range: std::ops::Range<usize>, _, _| {
                            let Ok(rows) = preview.as_ref() else {
                                return Vec::new();
                            };
                            range
                                .filter_map(|ix| {
                                    let row = rows.get(ix)?;
                                    let old: SharedString =
                                        String::from_utf8_lossy(&row.old).into_owned().into();
                                    let new: SharedString =
                                        String::from_utf8_lossy(&row.new).into_owned().into();
                                    let color = if row.problem.is_some() {
                                        theme.danger
                                    } else if row.changed() {
                                        theme.foreground
                                    } else {
                                        theme.muted_foreground
                                    };
                                    Some(
                                        h_flex()
                                            .id(("rename-row", ix))
                                            .h(px(22.))
                                            .px_2()
                                            .gap_2()
                                            .text_size(px(12.))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_color(theme.muted_foreground)
                                                    .child(old),
                                            )
                                            .child(
                                                div()
                                                    .flex_shrink_0()
                                                    .text_color(theme.muted_foreground)
                                                    .child("→"),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_color(color)
                                                    .child(new),
                                            )
                                            .when_some(row.problem.clone(), |el, problem| {
                                                el.child(
                                                    div()
                                                        .w(px(230.))
                                                        .flex_shrink_0()
                                                        .truncate()
                                                        .text_xs()
                                                        .text_color(theme.danger)
                                                        .child(problem),
                                                )
                                            })
                                            .into_any_element(),
                                    )
                                })
                                .collect::<Vec<_>>()
                        },
                    )
                    .size_full(),
                )
                .into_any_element()
        }
    };

    let problems = state.problems_text();
    let elevated = state.elevated_note();
    let can_apply = state.can_apply();
    dialog
        .title(SharedString::from(state.title.clone()))
        .width(px(900.))
        .child(
            v_flex().gap_3().child(masks).child(list).child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(state.summary())
                            .when_some(problems, |d, problems| {
                                d.child(div().text_color(theme.danger).child(problems))
                            })
                            .when_some(elevated, |d, note| {
                                d.child(div().text_xs().text_color(theme.danger).child(note))
                            }),
                    )
                    .child(Button::new("rename-cancel").label("Cancel").on_click(
                        move |_, _, cx| dispatch(Event::Dialog(id, DialogInput::Cancel), cx),
                    ))
                    .child(
                        Button::new("rename-apply")
                            .label("Rename")
                            .primary()
                            .disabled(!can_apply)
                            .on_click(move |_, _, cx| send(id, RenameInput::Apply, cx)),
                    ),
            ),
        )
}
