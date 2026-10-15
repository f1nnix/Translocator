//! Built-in viewer (F3) and editor (F4) windows. Files on servers are read
//! and written over the same provider as the pane, so saving through sudo
//! works like `sudo nano` on the server.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, RopeExt as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{prelude::*, *};
use tl_core::util::human_size;
use tl_core::{OpCtx, VPath, Vfs, VfsError};

use crate::actions::{CloseEditor, EDITOR, SaveFile};
use crate::dialogs;
use crate::pane::Location;
use crate::util::{blocking, language_for};
use crate::workspace::Workspace;

/// The viewer reads at most this much.
const VIEW_LIMIT: u64 = 8 * 1024 * 1024;
/// Larger files are not opened for editing.
const EDIT_LIMIT: u64 = 16 * 1024 * 1024;

/// Reads the file on a worker thread, then opens a window for it.
pub fn open(
    vfs: Arc<dyn Vfs>,
    path: VPath,
    elevated: bool,
    readonly: bool,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    cx.spawn_in(window, async move |_this, cx| {
        let (v, p) = (vfs.clone(), path.clone());
        let result = blocking(move || {
            let ctx = OpCtx::new().elevated(elevated);
            let meta = v
                .stat(&p, &ctx)?
                .ok_or_else(|| VfsError::NotFound(p.display()))?;
            if !readonly && meta.size > EDIT_LIMIT {
                return Err(VfsError::other(format!(
                    "{} is too large to edit ({}). Press F3 to view it.",
                    p.file_name_lossy(),
                    human_size(meta.size)
                )));
            }
            let limit = if readonly { VIEW_LIMIT } else { EDIT_LIMIT };
            let data = v.read_file(&p, limit, &ctx)?;
            Ok((data, meta.size))
        })
        .await;
        let _ = cx.update(move |window, cx| match result {
            Ok((data, size)) => open_window(vfs, path, elevated, readonly, data, size, workspace, cx),
            Err(VfsError::PermissionDenied(msg)) if !elevated && vfs.capabilities().elevation => {
                let answer = dialogs::ask_confirm(
                    window,
                    cx,
                    "Permission denied",
                    format!("{msg}\n\nOpen it with sudo?"),
                    "Use sudo",
                    false,
                );
                let workspace2 = workspace.clone();
                cx.spawn(async move |cx| {
                    if answer.await {
                        let _ = workspace2.update_in(cx, |_, window, cx| {
                            open(vfs, path, true, readonly, workspace.clone(), window, cx)
                        });
                    }
                })
                .detach();
            }
            Err(err) => window.push_notification(Notification::error(err.to_string()), cx),
        });
    })
    .detach();
}

#[allow(clippy::too_many_arguments)]
fn open_window(
    vfs: Arc<dyn Vfs>,
    path: VPath,
    elevated: bool,
    readonly: bool,
    data: Vec<u8>,
    file_size: u64,
    workspace: WeakEntity<Workspace>,
    cx: &mut App,
) {
    let mut notice = None;
    let mut readonly = readonly;
    if data.len() as u64 > 0 && (data.len() as u64) < file_size {
        notice = Some(format!(
            "Showing the first {} of {}.",
            human_size(data.len() as u64),
            human_size(file_size)
        ));
    }
    let binary = data.iter().take(8192).any(|b| *b == 0);
    let text = match String::from_utf8(data) {
        Ok(text) => text,
        Err(err) => {
            if !readonly {
                notice = Some("Not valid UTF-8: opened read-only so saving cannot damage it.".into());
            }
            readonly = true;
            String::from_utf8_lossy(err.as_bytes()).into_owned()
        }
    };
    if binary {
        readonly = true;
        notice = Some("Binary file: shown as text, read-only.".into());
    }
    let loc = Location {
        vfs: vfs.clone(),
        path: path.parent().unwrap_or_else(VPath::root),
    };
    let title = format!(
        "{} — {}{}",
        path.file_name_lossy(),
        loc.display(),
        if elevated { " (sudo)" } else { "" }
    );
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(960.), px(720.)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        window_min_size: Some(size(px(420.), px(280.))),
        ..Default::default()
    };
    let result = gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| EditorView::new(vfs, path, elevated, readonly, text, notice, workspace, window, cx))
    });
    if let Err(err) = result {
        log::error!("cannot open editor window: {err}");
    }
}

pub struct EditorView {
    vfs: Arc<dyn Vfs>,
    path: VPath,
    elevated: bool,
    readonly: bool,
    state: Entity<EditorState>,
    language: &'static str,
    dirty: bool,
    saving: bool,
    notice: Option<SharedString>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EditorView {
    #[allow(clippy::too_many_arguments)]
    fn new(
        vfs: Arc<dyn Vfs>,
        path: VPath,
        elevated: bool,
        readonly: bool,
        text: String,
        notice: Option<String>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let language = language_for(&path.file_name_lossy());
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .soft_wrap(false)
                .default_value(text)
        });
        let mut subs = vec![cx.subscribe(&state, |this: &mut Self, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) && !this.readonly && !this.dirty {
                this.dirty = true;
                cx.notify();
            }
        })];
        let handle = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            handle
                .update(cx, |this, cx| this.should_close(window, cx))
                .unwrap_or(true)
        });
        subs.push(cx.observe_window_appearance(window, |_, window, cx| {
            gpui_kit::component::Theme::sync_system_appearance(Some(window), cx);
        }));
        state.update(cx, |s, cx| s.focus(window, cx));
        EditorView {
            vfs,
            path,
            elevated,
            readonly,
            state,
            language,
            dirty: false,
            saving: false,
            notice: notice.map(Into::into),
            workspace,
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.dirty {
            return true;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Save changes to “{}”?", self.path.file_name_lossy()),
            Some("Your changes will be lost if you don't save them."),
            &["Save", "Don't Save", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            match answer.await {
                Ok(0) => {
                    let _ = this.update_in(cx, |this, window, cx| this.save_then(true, window, cx));
                }
                Ok(1) => {
                    let _ = this.update_in(cx, |this, window, _cx| {
                        this.dirty = false;
                        window.remove_window();
                    });
                }
                _ => {}
            }
        })
        .detach();
        false
    }

    fn save(&mut self, _: &SaveFile, window: &mut Window, cx: &mut Context<Self>) {
        self.save_then(false, window, cx);
    }

    fn close(&mut self, _: &CloseEditor, window: &mut Window, cx: &mut Context<Self>) {
        if self.should_close(window, cx) {
            window.remove_window();
        }
    }

    fn save_then(&mut self, close: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.readonly || self.saving {
            return;
        }
        self.saving = true;
        cx.notify();
        let text = self.state.read(cx).value().to_string();
        let (vfs, path, elevated) = (self.vfs.clone(), self.path.clone(), self.elevated);
        cx.spawn_in(window, async move |this, cx| {
            let (v, p) = (vfs.clone(), path.clone());
            let bytes = text.clone().into_bytes();
            let result = blocking(move || v.write_file(&p, &bytes, &OpCtx::new().elevated(elevated))).await;
            let _ = this.update_in(cx, move |this, window, cx| {
                this.saving = false;
                match result {
                    Ok(()) => {
                        this.dirty = false;
                        this.notice = None;
                        let dir = path.parent().unwrap_or_else(VPath::root);
                        let key = vfs.location_key().to_string();
                        let _ = this.workspace.update(cx, |ws, cx| ws.refresh_dir(&key, &dir, cx));
                        if close {
                            window.remove_window();
                        } else {
                            window.push_notification(Notification::success("Saved"), cx);
                        }
                    }
                    Err(VfsError::PermissionDenied(msg)) if !elevated && vfs.capabilities().elevation => {
                        let answer = dialogs::ask_confirm(
                            window,
                            cx,
                            "Permission denied",
                            format!("{msg}\n\nSave as root with sudo? The file keeps its owner and permissions."),
                            "Save with sudo",
                            false,
                        );
                        cx.spawn_in(window, async move |this, cx| {
                            if answer.await {
                                let _ = this.update_in(cx, |this, window, cx| {
                                    this.elevated = true;
                                    this.save_then(close, window, cx);
                                });
                            }
                        })
                        .detach();
                    }
                    Err(err) => dialogs::error(window, cx, "Cannot save", err.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let lines = self.state.read(cx).text().lines_len();
        let full = format!(
            "{}{}",
            if self.vfs.plugin_id() == tl_local::PLUGIN_ID { String::new() } else { format!("{}:", self.vfs.label()) },
            self.path
        );
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
                            .child(full),
                    )
                    .when(self.elevated, |el| el.child(Tag::danger().small().child("sudo")))
                    .when(self.readonly, |el| el.child(Tag::secondary().small().child("read-only")))
                    .when(self.dirty, |el| el.child(Tag::warning().small().child("modified")))
                    .when(!self.readonly, |el| {
                        el.child(
                            Button::new("save")
                                .label(if self.saving { "Saving…" } else { "Save" })
                                .primary()
                                .small()
                                .disabled(!self.dirty || self.saving)
                                .on_click(cx.listener(|this, _, window, cx| this.save_then(false, window, cx))),
                        )
                    }),
            )
            .when_some(self.notice.clone(), |el, notice| {
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
                    .child(Editor::new(&self.state).h_full().bordered(false).readonly(self.readonly)),
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
                    .child(self.language)
                    .child(div().flex_1())
                    .child(if self.readonly { "F3 viewer · ⌘W close" } else { "⌘S save · ⌘W close" }),
            )
    }
}
