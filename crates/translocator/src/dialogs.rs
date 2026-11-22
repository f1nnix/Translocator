//! Modal dialogs. They are the core's: `view.dialogs` lists the open ones,
//! the last on top, and [`DialogHost`] keeps the window's dialog stack in
//! step with that list. A dialog never closes itself: OK, Enter, Cancel,
//! Escape and the close button all become `DialogInput`s, and the dialog
//! goes when the core drops it from the view (which also answers whoever
//! waited on it — a provider's password prompt gets `None`).
//!
//! The builders read the dialog's current state from the [`Shell`] global
//! on every render, so an edit the core makes (the conflict dialog's "apply
//! to all", say) shows without reopening anything.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{Dialog as Modal, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{prelude::*, *};
use tl_app::dialog::{ConfirmPurpose, Dialog, DialogInput, DialogKind, InputDialog};
use tl_app::tl_model::transfer::{Conflict, ConflictChoice};
use tl_app::tl_model::util::human_size;
use tl_app::tl_model::Entry;
use tl_app::{DialogId, Event};

use crate::app_state::{Shell, dispatch};
use crate::util::format_date_long;

/// The window's side of `view.dialogs`.
#[derive(Default)]
pub struct DialogHost {
    /// The dialogs open in the window, bottom first.
    open: Vec<DialogId>,
    /// The text field of an input or password dialog.
    fields: HashMap<DialogId, (Entity<InputState>, Subscription)>,
    /// `view.dialogs` as last synced.
    last: Vec<Dialog>,
}

fn send(id: DialogId, input: DialogInput, cx: &App) {
    dispatch(Event::Dialog(id, input), cx);
}

impl DialogHost {
    /// Opens the dialogs new in `dialogs` and closes those gone from it.
    /// Answers whether anything changed, so the caller can redraw (the
    /// builders read the dialogs at render).
    pub fn sync<T: 'static>(&mut self, dialogs: &[Dialog], window: &mut Window, cx: &mut Context<T>) -> bool {
        if self.last == dialogs {
            return false;
        }
        // The window's stack only pops from the top, so a dialog closed
        // under another takes the ones above it down too; they are opened
        // again straight away, their state being in the view.
        let keep = self
            .open
            .iter()
            .zip(dialogs)
            .take_while(|(open, wanted)| **open == wanted.id)
            .count();
        while self.open.len() > keep {
            if let Some(id) = self.open.pop() {
                self.fields.remove(&id);
            }
            window.close_dialog(cx);
        }
        for dialog in &dialogs[keep..] {
            self.open_one(dialog, window, cx);
            self.open.push(dialog.id);
        }
        self.last = dialogs.to_vec();
        true
    }

    fn open_one<T: 'static>(&mut self, dialog: &Dialog, window: &mut Window, cx: &mut Context<T>) {
        let id = dialog.id;
        let field = match &dialog.kind {
            DialogKind::Input(d) => Some(self.field(id, &d.text, false, window, cx)),
            DialogKind::Secret { text, .. } => Some(self.field(id, text, true, window, cx)),
            _ => None,
        };
        // A provider's question may come while the app is in the background.
        if matches!(
            &dialog.kind,
            DialogKind::Secret { .. } | DialogKind::Confirm(tl_app::dialog::ConfirmDialog { purpose: ConfirmPurpose::Prompt { .. }, .. })
        ) {
            cx.activate(true);
        }
        let first = Rc::new(Cell::new(true));
        window.open_dialog(cx, move |modal, window, cx| build(id, field.clone(), &first, modal, window, cx));
    }

    fn field<T: 'static>(
        &mut self,
        id: DialogId,
        text: &str,
        masked: bool,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> Entity<InputState> {
        let input = cx.new(|cx| InputState::new(window, cx).masked(masked).default_value(text.to_string()));
        let sub = cx.subscribe_in(&input, window, move |_, input, ev: &InputEvent, _, cx| {
            if let InputEvent::Change = ev {
                send(id, DialogInput::Text(input.read(cx).value().to_string()), cx);
            }
        });
        self.fields.insert(id, (input.clone(), sub));
        input
    }
}

/// OK: the field's text first, so the core answers with what is shown.
fn submit(id: DialogId, field: Option<&Entity<InputState>>, cx: &App) {
    if let Some(field) = field {
        send(id, DialogInput::Text(field.read(cx).value().to_string()), cx);
    }
    send(id, DialogInput::Submit, cx);
}

/// One render of dialog `id`, from the view.
fn build(
    id: DialogId,
    field: Option<Entity<InputState>>,
    first: &Cell<bool>,
    modal: Modal,
    window: &mut Window,
    cx: &mut App,
) -> Modal {
    let view = cx.global::<Shell>().view.clone();
    // Gone from the view: the host closes it on its next sync.
    let Some(dialog) = view.dialogs.iter().find(|d| d.id == id) else {
        return modal;
    };
    let ok_field = field.clone();
    let modal = modal
        .on_ok(move |_, _, cx| {
            submit(id, ok_field.as_ref(), cx);
            false
        })
        .on_cancel(move |_, _, cx| {
            send(id, DialogInput::Cancel, cx);
            false
        });
    let first = first.replace(false);
    match &dialog.kind {
        DialogKind::About => about(id, modal, cx),
        DialogKind::Input(d) => input(id, d, field, first, modal, window, cx),
        DialogKind::Secret { title, message, .. } => secret(id, title, message, field, first, modal, window, cx),
        DialogKind::Confirm(d) => {
            let message = message_block(d.message.clone().into(), cx);
            modal
                .title(SharedString::from(d.title.clone()))
                .width(px(460.))
                .child(message)
                .footer(footer(id, d.ok.clone().into(), d.danger, None))
        }
        DialogKind::Message { title, message } => modal
            .title(SharedString::from(title.clone()))
            .width(px(520.))
            .child(
                div()
                    .text_sm()
                    .font_family(cx.theme().mono_font_family.clone())
                    .whitespace_normal()
                    .child(SharedString::from(message.clone())),
            )
            .footer(
                DialogFooter::new().child(
                    Button::new("dlg-ok")
                        .label("OK")
                        .primary()
                        .on_click(move |_, _, cx| send(id, DialogInput::Submit, cx)),
                ),
            ),
        DialogKind::Conflict { conflict, apply_all, .. } => self::conflict(id, conflict, *apply_all, modal, cx),
        DialogKind::FindResults { title, rows, .. } => {
            let theme = cx.theme().clone();
            let rows = rows.clone();
            let list = uniform_list("find-results", rows.len(), move |range: std::ops::Range<usize>, _, _| {
                range
                    .map(|ix| {
                        let hit = &rows[ix];
                        h_flex()
                            .id(("hit", ix))
                            .h(px(24.))
                            .px_2()
                            .gap_2()
                            .rounded_sm()
                            .cursor_pointer()
                            .text_size(px(12.))
                            .hover(|s| s.bg(theme.list_hover))
                            .child(
                                Icon::new(if hit.is_dir { Lucide::Folder } else { Lucide::File })
                                    .size(px(14.))
                                    .text_color(if hit.is_dir { theme.blue } else { theme.muted_foreground }),
                            )
                            .child(div().flex_1().min_w_0().truncate().child(SharedString::from(hit.rel.clone())))
                            .on_click(move |_, _, cx| send(id, DialogInput::Pick(ix), cx))
                    })
                    .collect::<Vec<_>>()
            })
            .h(px(420.));
            modal.title(SharedString::from(title.clone())).width(px(720.)).child(list)
        }
        DialogKind::Rename(state) => crate::rename::render(modal, state, id, window, cx),
        DialogKind::Sync(state) => crate::sync::render(modal, state, id, window, cx),
        DialogKind::Properties(state) => crate::properties::render(modal, state, id, window, cx),
        DialogKind::BookmarkEdit(state) => crate::bookmarks::render_edit(modal, state, id, window, cx),
        DialogKind::Bookmarks(state) => crate::bookmarks::render_manage(modal, state, id, window, cx),
    }
}

/// The panel behind the application menu's first item: what this is, which
/// version, and where to read about it.
fn about(id: DialogId, modal: Modal, cx: &App) -> Modal {
    let theme = cx.theme().clone();
    modal
        .title("Translocator")
        .width(px(420.))
        .child(
            v_flex()
                .gap_2()
                .text_sm()
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(format!("Version {}", env!("CARGO_PKG_VERSION"))),
                )
                .child(div().whitespace_normal().child(env!("CARGO_PKG_DESCRIPTION")))
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(format!("MIT licensed · {}", crate::actions::HELP_URL)),
                ),
        )
        .footer(
            DialogFooter::new()
                .child(
                    Button::new("about-help")
                        .label("Open the README")
                        .on_click(move |_, _, cx| send(id, DialogInput::OpenReadme, cx)),
                )
                .child(
                    Button::new("about-close")
                        .label("Close")
                        .primary()
                        .on_click(move |_, _, cx| send(id, DialogInput::Submit, cx)),
                ),
        )
}

fn message_block(text: SharedString, cx: &App) -> impl IntoElement + use<> {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .whitespace_normal()
        .child(text)
}

/// Standard Cancel / OK footer.
fn footer(id: DialogId, ok_label: SharedString, danger: bool, field: Option<Entity<InputState>>) -> DialogFooter {
    DialogFooter::new()
        .child(
            Button::new("dlg-cancel")
                .label("Cancel")
                .on_click(move |_, _, cx| send(id, DialogInput::Cancel, cx)),
        )
        .child(
            Button::new("dlg-ok")
                .label(ok_label)
                .map(|b| if danger { b.danger() } else { b.primary() })
                .on_click(move |_, _, cx| submit(id, field.as_ref(), cx)),
        )
}

/// Asks for a line of text (a name, a path, a mask).
fn input(
    id: DialogId,
    d: &InputDialog,
    field: Option<Entity<InputState>>,
    first: bool,
    modal: Modal,
    window: &mut Window,
    cx: &mut App,
) -> Modal {
    let Some(field) = field else { return modal };
    if first {
        let select = d.select;
        field.update(cx, |s, cx| {
            s.focus(window, cx);
            match select {
                Some((start, end)) => s.set_selected_range(start..end, cx),
                None => s.select_all(window, cx),
            }
        });
    }
    let message: SharedString = d.message.clone().into();
    modal
        .title(SharedString::from(d.title.clone()))
        .width(px(520.))
        .child(
            v_flex()
                .gap_3()
                .when(!message.is_empty(), |el| el.child(message_block(message.clone(), cx)))
                .child(Input::new(&field)),
        )
        .footer(footer(id, d.ok.clone().into(), false, Some(field.clone())))
}

/// Asks for a password or other secret.
#[allow(clippy::too_many_arguments)]
fn secret(
    id: DialogId,
    title: &str,
    message: &str,
    field: Option<Entity<InputState>>,
    first: bool,
    modal: Modal,
    window: &mut Window,
    cx: &mut App,
) -> Modal {
    let Some(field) = field else { return modal };
    if first {
        field.update(cx, |s, cx| s.focus(window, cx));
    }
    modal
        .title(SharedString::from(title.to_string()))
        .width(px(440.))
        .overlay_closable(false)
        .child(
            v_flex()
                .gap_3()
                .child(message_block(message.to_string().into(), cx))
                .child(Input::new(&field).mask_toggle()),
        )
        .footer(footer(id, "OK".into(), false, Some(field.clone())))
}

fn describe(entry: &Entry) -> String {
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
fn conflict(id: DialogId, conflict: &Conflict, apply_all: bool, modal: Modal, cx: &App) -> Modal {
    let theme = cx.theme().clone();
    let name = conflict.source.name_lossy();
    let title: SharedString = if conflict.both_dirs {
        format!("Folder “{name}” already exists").into()
    } else {
        format!("“{name}” already exists").into()
    };
    let newer = match (conflict.source.mtime, conflict.existing.mtime) {
        (Some(a), Some(b)) if a > b => Some(true),
        (Some(a), Some(b)) if a < b => Some(false),
        _ => None,
    };
    let row = |label: &'static str, text: String, highlight: bool| {
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
    let choice = |key: &'static str, label: &'static str, choice: ConflictChoice, primary: bool| {
        Button::new(key)
            .label(label)
            .small()
            .when(primary, |b| b.primary())
            .on_click(move |_, _, cx| send(id, DialogInput::Choose(choice), cx))
    };
    let buttons = if conflict.both_dirs {
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
    modal
        .title(title)
        .width(px(600.))
        .overlay_closable(false)
        .child(
            v_flex()
                .gap_2()
                .child(message_block(format!("in {}", conflict.dst_dir).into(), cx))
                .child(row("Existing", describe(&conflict.existing), newer == Some(false)))
                .child(row(
                    if conflict.both_dirs { "Incoming" } else { "Replace with" },
                    describe(&conflict.source),
                    newer == Some(true),
                ))
                .child(
                    Checkbox::new("c-all")
                        .label("Apply to all remaining conflicts")
                        .checked(apply_all)
                        .on_click(move |checked: &bool, _, cx| send(id, DialogInput::ApplyAll(*checked), cx)),
                ),
        )
        .footer(buttons)
}
