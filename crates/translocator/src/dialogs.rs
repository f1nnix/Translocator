//! Modal dialogs. Each one reports its answer through a [`Reply`], which
//! falls back to a default answer if the dialog disappears unanswered.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{prelude::*, *};
use tl_core::transfer::{Conflict, ConflictAnswer, ConflictChoice};
use tl_core::util::human_size;

use crate::util::format_date_long;

struct ReplyInner<T> {
    send: Option<Box<dyn FnOnce(T)>>,
    default: Option<T>,
}

impl<T> Drop for ReplyInner<T> {
    fn drop(&mut self) {
        if let (Some(send), Some(default)) = (self.send.take(), self.default.take()) {
            send(default);
        }
    }
}

/// Answer channel of a dialog: the first `send` wins; if every clone is
/// dropped without an answer (dialog closed some other way), the default
/// is sent.
pub struct Reply<T>(Rc<RefCell<ReplyInner<T>>>);

impl<T> Clone for Reply<T> {
    fn clone(&self) -> Self {
        Reply(self.0.clone())
    }
}

impl<T: 'static> Reply<T> {
    pub fn new(default: T, send: impl FnOnce(T) + 'static) -> Self {
        Reply(Rc::new(RefCell::new(ReplyInner {
            send: Some(Box::new(send)),
            default: Some(default),
        })))
    }

    pub fn send(&self, value: T) {
        let send = self.0.borrow_mut().send.take();
        if let Some(send) = send {
            send(value);
        }
    }

    /// A reply bound to a future that resolves with the answer.
    pub fn channel(default: T) -> (Self, impl Future<Output = T> + Send + use<T>)
    where
        T: Clone + Send,
    {
        let (tx, rx) = futures::channel::oneshot::channel();
        let fallback = default.clone();
        let reply = Reply::new(default, move |v| {
            let _ = tx.send(v);
        });
        (reply, async move { rx.await.unwrap_or(fallback) })
    }
}

fn message_block(text: SharedString, cx: &App) -> impl IntoElement {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .whitespace_normal()
        .child(text)
}

/// Asks for a password or other secret.
pub fn secret(window: &mut Window, cx: &mut App, title: String, message: String, reply: Reply<Option<String>>) {
    let input = cx.new(|cx| InputState::new(window, cx).masked(true));
    let first = Cell::new(true);
    let title: SharedString = title.into();
    let message: SharedString = message.into();
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            input.update(cx, |s, cx| s.focus(window, cx));
        }
        let ok = {
            let (reply, input) = (reply.clone(), input.clone());
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                reply.send(Some(input.read(cx).value().to_string()));
                true
            }
        };
        let cancel = {
            let reply = reply.clone();
            move |_: &ClickEvent, _: &mut Window, _: &mut App| {
                reply.send(None);
                true
            }
        };
        dialog
            .title(title.clone())
            .width(px(440.))
            .overlay_closable(false)
            .child(
                v_flex()
                    .gap_3()
                    .child(message_block(message.clone(), cx))
                    .child(Input::new(&input).mask_toggle()),
            )
            .footer(footer(&reply, None, "OK", false, Some(input.clone())))
            .on_ok(ok)
            .on_cancel(cancel)
    });
}

/// Standard Cancel / OK footer that answers through `reply`.
fn footer(
    reply: &Reply<Option<String>>,
    cancel_value: Option<String>,
    ok_label: &'static str,
    danger: bool,
    input: Option<Entity<InputState>>,
) -> impl IntoElement {
    let cancel_reply = reply.clone();
    let ok_reply = reply.clone();
    DialogFooter::new()
        .child(
            Button::new("dlg-cancel")
                .label("Cancel")
                .on_click(move |_, window, cx| {
                    cancel_reply.send(cancel_value.clone());
                    window.close_dialog(cx);
                }),
        )
        .child(
            Button::new("dlg-ok")
                .label(ok_label)
                .map(|b| if danger { b.danger() } else { b.primary() })
                .on_click(move |_, window, cx| {
                    let value = input
                        .as_ref()
                        .map(|i| i.read(cx).value().to_string())
                        .unwrap_or_default();
                    ok_reply.send(Some(value));
                    window.close_dialog(cx);
                }),
        )
}

/// Yes/no question.
pub fn confirm(
    window: &mut Window,
    cx: &mut App,
    title: String,
    message: String,
    ok_label: SharedString,
    danger: bool,
    reply: Reply<bool>,
) {
    let title: SharedString = title.into();
    let message: SharedString = message.into();
    window.open_dialog(cx, move |dialog, _window, cx| {
        let (ok_reply, cancel_reply) = (reply.clone(), reply.clone());
        let (b1, b2) = (reply.clone(), reply.clone());
        dialog
            .title(title.clone())
            .width(px(460.))
            .child(message_block(message.clone(), cx))
            .footer(
                DialogFooter::new()
                    .child(Button::new("dlg-cancel").label("Cancel").on_click(
                        move |_, window, cx| {
                            b1.send(false);
                            window.close_dialog(cx);
                        },
                    ))
                    .child(
                        Button::new("dlg-ok")
                            .label(ok_label.clone())
                            .map(|b| if danger { b.danger() } else { b.primary() })
                            .on_click(move |_, window, cx| {
                                b2.send(true);
                                window.close_dialog(cx);
                            }),
                    ),
            )
            .on_ok(move |_, _, _| {
                ok_reply.send(true);
                true
            })
            .on_cancel(move |_, _, _| {
                cancel_reply.send(false);
                true
            })
    });
}

pub fn ask_confirm(
    window: &mut Window,
    cx: &mut App,
    title: impl Into<String>,
    message: impl Into<String>,
    ok_label: &'static str,
    danger: bool,
) -> futures::future::BoxFuture<'static, bool> {
    let (reply, answer) = Reply::channel(false);
    confirm(window, cx, title.into(), message.into(), ok_label.into(), danger, reply);
    Box::pin(answer)
}

/// Asks for a line of text (a name, a path, a mask).
#[allow(clippy::too_many_arguments)]
pub fn input(
    window: &mut Window,
    cx: &mut App,
    title: String,
    message: String,
    initial: String,
    select: Option<Range<usize>>,
    ok_label: &'static str,
    reply: Reply<Option<String>>,
) {
    let input = cx.new(|cx| InputState::new(window, cx).default_value(initial.clone()));
    let first = Cell::new(true);
    let title: SharedString = title.into();
    let message: SharedString = message.into();
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            let select = select.clone();
            input.update(cx, |s, cx| {
                s.focus(window, cx);
                match select {
                    Some(range) => s.set_selected_range(range, cx),
                    None => s.select_all(window, cx),
                }
            });
        }
        let (ok_reply, cancel_reply, ok_input) = (reply.clone(), reply.clone(), input.clone());
        dialog
            .title(title.clone())
            .width(px(520.))
            .child(
                v_flex()
                    .gap_3()
                    .when(!message.is_empty(), |el| el.child(message_block(message.clone(), cx)))
                    .child(Input::new(&input)),
            )
            .footer(footer(&reply, None, ok_label, false, Some(input.clone())))
            .on_ok(move |_, _, cx| {
                ok_reply.send(Some(ok_input.read(cx).value().to_string()));
                true
            })
            .on_cancel(move |_, _, _| {
                cancel_reply.send(None);
                true
            })
    });
}

#[allow(clippy::too_many_arguments)]
pub fn ask_input(
    window: &mut Window,
    cx: &mut App,
    title: impl Into<String>,
    message: impl Into<String>,
    initial: impl Into<String>,
    select: Option<Range<usize>>,
    ok_label: &'static str,
) -> futures::future::BoxFuture<'static, Option<String>> {
    let (reply, answer) = Reply::channel(None);
    input(
        window,
        cx,
        title.into(),
        message.into(),
        initial.into(),
        select,
        ok_label,
        reply,
    );
    Box::pin(answer)
}

/// Byte range of a file name's stem, for "rename" preselection.
pub fn stem_range(name: &str) -> Range<usize> {
    match name.rfind('.') {
        Some(0) | None => 0..name.len(),
        Some(i) => 0..i,
    }
}

fn describe(entry: &tl_core::Entry) -> String {
    let kind = if entry.is_dir() { "folder" } else { "file" };
    let size = if entry.is_dir() {
        String::new()
    } else {
        format!("{}, ", human_size(entry.size))
    };
    format!("{kind}, {size}modified {}", format_date_long(entry.mtime))
}

/// "An item with this name already exists" — the choice can apply to all
/// remaining conflicts of the same transfer.
pub fn conflict(window: &mut Window, cx: &mut App, conflict: Conflict, reply: Reply<ConflictAnswer>) {
    let apply_all = Rc::new(Cell::new(false));
    let name: SharedString = conflict.source.name_lossy().into();
    let title: SharedString = if conflict.both_dirs {
        format!("Folder “{name}” already exists").into()
    } else {
        format!("“{name}” already exists").into()
    };
    let existing: SharedString = describe(&conflict.existing).into();
    let incoming: SharedString = describe(&conflict.source).into();
    let place: SharedString = format!("in {}", conflict.dst_dir).into();
    let newer = match (conflict.source.mtime, conflict.existing.mtime) {
        (Some(a), Some(b)) if a > b => Some(true),
        (Some(a), Some(b)) if a < b => Some(false),
        _ => None,
    };
    let both_dirs = conflict.both_dirs;
    window.open_dialog(cx, move |dialog, _window, cx| {
        let theme = cx.theme();
        let row = |label: &'static str, text: SharedString, highlight: bool| {
            h_flex()
                .gap_2()
                .text_sm()
                .child(div().w(px(110.)).text_color(theme.muted_foreground).child(label))
                .child(
                    div()
                        .flex_1()
                        .when(highlight, |d| d.text_color(theme.success))
                        .child(text),
                )
        };
        let choice = |id: &'static str, label: &'static str, choice: ConflictChoice, primary: bool| {
            let (reply, apply_all) = (reply.clone(), apply_all.clone());
            Button::new(id)
                .label(label)
                .small()
                .when(primary, |b| b.primary())
                .on_click(move |_, window, cx| {
                    reply.send(ConflictAnswer {
                        choice,
                        apply_to_all: apply_all.get(),
                        rename_to: None,
                    });
                    window.close_dialog(cx);
                })
        };
        let apply = apply_all.clone();
        let buttons = if both_dirs {
            h_flex()
                .flex_wrap()
                .gap_2()
                .justify_end()
                .child(choice("c-cancel", "Cancel", ConflictChoice::Cancel, false))
                .child(choice("c-skip", "Skip", ConflictChoice::Skip, false))
                .child(choice("c-both", "Keep Both", ConflictChoice::KeepBoth, false))
                .child(choice("c-keep", "Merge, Keep Existing", ConflictChoice::SkipExisting, false))
                .child(choice("c-older", "Merge, Replace Older", ConflictChoice::OverwriteOlder, false))
                .child(choice("c-over", "Merge, Replace", ConflictChoice::Overwrite, true))
        } else {
            h_flex()
                .flex_wrap()
                .gap_2()
                .justify_end()
                .child(choice("c-cancel", "Cancel", ConflictChoice::Cancel, false))
                .child(choice("c-skip", "Skip", ConflictChoice::Skip, false))
                .child(choice("c-both", "Keep Both", ConflictChoice::KeepBoth, false))
                .child(choice("c-older", "Replace If Older", ConflictChoice::OverwriteOlder, false))
                .child(choice("c-over", "Replace", ConflictChoice::Overwrite, true))
        };
        let cancel_reply = reply.clone();
        dialog
            .title(title.clone())
            .width(px(600.))
            .overlay_closable(false)
            .child(
                v_flex()
                    .gap_2()
                    .child(message_block(place.clone(), cx))
                    .child(row("Existing", existing.clone(), newer == Some(false)))
                    .child(row(
                        if both_dirs { "Incoming" } else { "Replace with" },
                        incoming.clone(),
                        newer == Some(true),
                    ))
                    .child(
                        Checkbox::new("c-all")
                            .label("Apply to all remaining conflicts")
                            .checked(apply.get())
                            .on_click(move |checked: &bool, window, _cx| {
                                apply.set(*checked);
                                window.refresh();
                            }),
                    ),
            )
            .footer(buttons)
            .on_ok({
                let (reply, apply_all) = (reply.clone(), apply_all.clone());
                move |_, _, _| {
                    reply.send(ConflictAnswer {
                        choice: ConflictChoice::Overwrite,
                        apply_to_all: apply_all.get(),
                        rename_to: None,
                    });
                    true
                }
            })
            .on_cancel(move |_, _, _| {
                cancel_reply.send(ConflictAnswer::new(ConflictChoice::Cancel));
                true
            })
    });
}

/// Shows an error in a dialog (for errors that need reading, not a toast).
pub fn error(window: &mut Window, cx: &mut App, title: impl Into<SharedString>, message: impl Into<SharedString>) {
    let title = title.into();
    let message = message.into();
    window.open_dialog(cx, move |dialog, _window, cx| {
        dialog
            .title(title.clone())
            .width(px(520.))
            .child(
                div()
                    .text_sm()
                    .font_family(cx.theme().mono_font_family.clone())
                    .whitespace_normal()
                    .child(message.clone()),
            )
            .footer(
                DialogFooter::new().child(
                    Button::new("dlg-ok")
                        .label("OK")
                        .primary()
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            )
    });
}
