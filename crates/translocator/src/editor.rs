//! The built-in viewer (F3) and editor (F4), each in a window of its own.
//!
//! The editors are the core's ([`tl_app::editor`]): it reads the file, says
//! what the header, notice and footer show, saves (retrying with sudo when
//! asked) and decides when a window goes. This keeps one window per
//! [`EditorView`] in the view: [`sync`] opens a window for an editor it has
//! not seen, passes every later view to its window and closes the window
//! whose editor left the view.
//!
//! The text widget owns the live text. It is loaded from
//! [`EditorView::content`] whenever [`EditorView::content_version`] changes,
//! the first edit is reported ([`EditorInput::Changed`]) and the text goes
//! back to the core only to be saved.

use std::collections::HashMap;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, RopeExt as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::{prelude::*, *};
use tl_app::editor::{ClosePrompt, CloseChoice, EditorInput, EditorView};
use tl_app::{EditorId, Event, ToastKind, ViewModel};

use crate::actions::{CloseEditor, EDITOR, SaveFile};
use crate::app_state::dispatch;

/// The open editor windows, by the core's id.
#[derive(Default)]
struct Windows(HashMap<EditorId, (AnyWindowHandle, Entity<EditorWindow>)>);

impl Global for Windows {}

/// Brings the editor windows in line with the view: opens the new ones,
/// updates the open ones and closes those whose editor is gone.
pub fn sync(view: &ViewModel, cx: &mut App) {
    let open: Vec<EditorId> = cx
        .try_global::<Windows>()
        .map(|windows| windows.0.keys().copied().collect())
        .unwrap_or_default();
    for id in open {
        if view.editors.iter().any(|editor| editor.id == id) {
            continue;
        }
        if let Some((handle, _)) = cx.default_global::<Windows>().0.remove(&id) {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    for editor in &view.editors {
        let id = editor.id;
        match cx.default_global::<Windows>().0.get(&id).cloned() {
            Some((handle, entity)) => {
                let editor = editor.clone();
                let updated = handle.update(cx, |_, window, cx| {
                    entity.update(cx, |this, cx| this.view_changed(editor, window, cx))
                });
                if updated.is_err() {
                    // The window went without asking the core first.
                    cx.default_global::<Windows>().0.remove(&id);
                    dispatch(Event::Editor(id, EditorInput::Closed), cx);
                }
            }
            None => open_window(editor.clone(), cx),
        }
    }
}

fn open_window(view: EditorView, cx: &mut App) {
    let id = view.id;
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(960.), px(720.)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(view.title.clone().into()),
            ..Default::default()
        }),
        window_min_size: Some(size(px(420.), px(280.))),
        ..Default::default()
    };
    match gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| EditorWindow::new(view, window, cx))
    }) {
        Ok((handle, entity)) => {
            cx.default_global::<Windows>().0.insert(id, (handle, entity));
        }
        Err(err) => {
            log::error!("cannot open editor window: {err}");
            dispatch(Event::Editor(id, EditorInput::Closed), cx);
        }
    }
}

struct EditorWindow {
    /// The core's view of this editor, as last drawn.
    view: EditorView,
    state: Entity<EditorState>,
    focus: FocusHandle,
    /// The first edit since the text was last clean has been reported.
    sent_changed: bool,
    /// The "Save changes?" prompt is up, or answered and waiting for the
    /// core to take it down.
    prompting: bool,
    _subs: Vec<Subscription>,
}

impl EditorWindow {
    fn new(view: EditorView, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(view.language)
                .line_number(true)
                .soft_wrap(false)
                .default_value(view.content.to_string())
        });
        let subs = vec![
            cx.subscribe(&state, |this: &mut Self, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.edited(cx);
                }
            }),
            cx.observe_window_appearance(window, |_, window, cx| crate::themes::apply(Some(window), cx)),
        ];
        // The close button asks the core, which closes the window itself
        // (through `sync`) or asks about the unsaved changes first.
        let id = view.id;
        window.on_window_should_close(cx, move |_, cx| {
            dispatch(Event::Editor(id, EditorInput::Close), cx);
            false
        });
        state.update(cx, |s, cx| s.focus(window, cx));
        let prompt = view.close_prompt.clone();
        let mut this = EditorWindow {
            view,
            state,
            focus: cx.focus_handle(),
            sent_changed: false,
            prompting: false,
            _subs: subs,
        };
        if let Some(prompt) = prompt {
            this.prompt(prompt, window, cx);
        }
        this
    }

    /// Takes in the core's latest view of this editor.
    fn view_changed(&mut self, new: EditorView, window: &mut Window, cx: &mut Context<Self>) {
        if new == self.view {
            return;
        }
        let old = std::mem::replace(&mut self.view, new);
        let view = &self.view;
        if old.content_version != view.content_version {
            let text = view.content.to_string();
            self.state.update(cx, |s, cx| s.set_value(text, window, cx));
        }
        if !view.dirty {
            self.sent_changed = false;
        }
        let seq = |n: &Option<tl_app::editor::Notice>| n.as_ref().map(|n| n.seq);
        if seq(&old.notification) != seq(&view.notification)
            && let Some(notice) = view.notification.clone()
        {
            let note = match notice.kind {
                ToastKind::Info => Notification::info(notice.message),
                ToastKind::Success => Notification::success(notice.message),
                ToastKind::Error => Notification::error(notice.message),
            };
            window.push_notification(note, cx);
        }
        match (view.close_prompt.clone(), self.prompting) {
            (Some(prompt), false) => self.prompt(prompt, window, cx),
            (None, _) => self.prompting = false,
            (Some(_), true) => {}
        }
        cx.notify();
    }

    /// The native "Save changes?" prompt; the answer goes to the core.
    fn prompt(&mut self, prompt: ClosePrompt, window: &mut Window, cx: &mut Context<Self>) {
        self.prompting = true;
        let answer = window.prompt(
            PromptLevel::Warning,
            &prompt.message,
            Some(prompt.detail),
            &prompt.buttons,
            cx,
        );
        let id = self.view.id;
        cx.spawn_in(window, async move |this, cx| {
            let answer = answer.await;
            let _ = this.update(cx, |this, cx| {
                let choice = match answer {
                    Ok(0) => CloseChoice::Save { text: this.text(cx) },
                    Ok(1) => CloseChoice::DontSave,
                    _ => CloseChoice::Cancel,
                };
                dispatch(Event::Editor(id, EditorInput::CloseAnswer(choice)), cx);
            });
        })
        .detach();
    }

    fn text(&self, cx: &App) -> String {
        self.state.read(cx).value().to_string()
    }

    /// The text widget changed: the first edit of an editable file makes it
    /// modified, which only the core decides.
    fn edited(&mut self, cx: &mut Context<Self>) {
        if self.view.readonly || self.view.dirty || self.sent_changed {
            return;
        }
        self.sent_changed = true;
        dispatch(Event::Editor(self.view.id, EditorInput::Changed), cx);
    }

    fn save_now(&mut self, cx: &mut Context<Self>) {
        if self.view.readonly {
            return;
        }
        let text = self.text(cx);
        dispatch(Event::Editor(self.view.id, EditorInput::Save { text }), cx);
    }

    fn save(&mut self, _: &SaveFile, _: &mut Window, cx: &mut Context<Self>) {
        self.save_now(cx);
    }

    fn close(&mut self, _: &CloseEditor, _: &mut Window, cx: &mut Context<Self>) {
        dispatch(Event::Editor(self.view.id, EditorInput::Close), cx);
    }
}

impl Render for EditorWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let lines = self.state.read(cx).text().lines_len();
        let view = &self.view;
        v_flex()
            .size_full()
            .key_context(EDITOR)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::close))
            .bg(theme.background)
            .child(
                h_flex()
                    .h(px(34.))
                    .flex_shrink_0()
                    .px_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.))
                            .font_family(theme.mono_font_family.clone())
                            .child(view.full_path.clone()),
                    )
                    .when(view.elevated, |el| el.child(Tag::danger().small().child("sudo")))
                    .when(view.readonly, |el| el.child(Tag::secondary().small().child("read-only")))
                    .when(view.dirty, |el| el.child(Tag::warning().small().child("modified")))
                    .when(!view.readonly, |el| {
                        el.child(
                            Button::new("save")
                                .label(view.save_label)
                                .primary()
                                .small()
                                .disabled(!view.can_save)
                                .on_click(cx.listener(|this, _, _, cx| this.save_now(cx))),
                        )
                    }),
            )
            .when_some(view.notice.clone(), |el, notice| {
                el.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_size(px(12.))
                        .bg(theme.warning.opacity(0.12))
                        .text_color(theme.warning_foreground)
                        .child(notice),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(Editor::new(&self.state).h_full().bordered(false).readonly(view.readonly)),
            )
            .child(
                h_flex()
                    .h(px(22.))
                    .flex_shrink_0()
                    .px_2()
                    .gap_3()
                    .text_size(px(11.))
                    .text_color(theme.muted_foreground)
                    .border_t_1()
                    .border_color(theme.border)
                    .child(format!("{lines} lines"))
                    .child(view.language)
                    .child(div().flex_1())
                    .child(view.hint),
            )
    }
}
