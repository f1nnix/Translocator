//! The viewer and editor windows (F3 / F4), one `adw::Window` per entry of
//! `Snapshot.editors` ([`EditorSnapshot`], `tl_ffi::editor`).
//!
//! The header bar has the file's name over its full path, the sudo /
//! read-only / modified tags and Save (`save_label`, enabled by
//! `can_save`); `notice` is an `adw::Banner` under it; the text is a
//! [`CodeView`], loaded with `Core::editor_content(id)` only when
//! `content_version` moves and owned by the view after that. The first
//! edit since the core last called the file clean sends
//! `EditorInput::Changed`. The footer counts the lines and has the
//! language and the key hint.
//!
//! Every decision is the core's: Ctrl+S and Save send the text, Ctrl+W
//! and the window's close button send `Close` and the window stays
//! (`close-request` is stopped) until the core drops the editor. The core
//! may ask first, through `close_prompt`, shown as an `adw::AlertDialog`
//! and answered with `CloseAnswer`. `notification` is a toast in the
//! window, again whenever its `seq` moves. A window that goes some other
//! way sends `Closed`.
//!
//! Called by the shell as `Editors::sync(&[EditorSnapshot], &window)` with
//! every snapshot whose editors changed. Swift: `EditorWindow.swift`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};
use tl_ffi::editor::{CloseChoice, EditorInput, EditorSnapshot};
use tl_ffi::input::Input;
use tl_ffi::keys::{EditorKey, KeyContext, KeyTarget};
use tl_ffi::types::ToastKind;

use super::code::CodeView;
use crate::{keys, shell};

#[derive(Default)]
pub struct Editors {
    windows: RefCell<HashMap<u64, Rc<EditorWindow>>>,
}

impl Editors {
    /// Opens a window for each listed editor, updates the open ones and
    /// closes those the core dropped.
    pub fn sync(&self, editors: &[EditorSnapshot], parent: &impl IsA<gtk::Window>) {
        let gone: Vec<Rc<EditorWindow>> = {
            let mut windows = self.windows.borrow_mut();
            let ids: Vec<u64> = windows.keys().copied().filter(|id| !editors.iter().any(|e| e.id == *id)).collect();
            ids.into_iter().filter_map(|id| windows.remove(&id)).collect()
        };
        for window in gone {
            window.drop_window();
        }
        for editor in editors {
            let existing = self.windows.borrow().get(&editor.id).cloned();
            match existing {
                Some(window) => window.update(editor),
                None => {
                    let window = EditorWindow::new(editor, parent.upcast_ref());
                    self.windows.borrow_mut().insert(editor.id, window.clone());
                    window.update(editor);
                    window.window.present();
                    window.code.grab_focus();
                }
            }
        }
    }
}

struct EditorWindow {
    id: u64,
    window: adw::Window,
    title: adw::WindowTitle,
    sudo: gtk::Label,
    readonly: gtk::Label,
    modified: gtk::Label,
    save: gtk::Button,
    banner: adw::Banner,
    toasts: adw::ToastOverlay,
    code: CodeView,
    lines: gtk::Label,
    language: gtk::Label,
    hint: gtk::Label,
    /// The snapshot last applied.
    snapshot: RefCell<EditorSnapshot>,
    /// `Changed` went to the core since it last said the file was clean.
    sent_changed: Cell<bool>,
    /// The "Save changes?" alert while it is up (or answered and not yet
    /// taken back by the core).
    prompt: RefCell<Option<adw::AlertDialog>>,
    /// The notification last shown.
    seen_seq: Cell<u64>,
    /// The core dropped the editor: the window goes for good.
    dropped: Cell<bool>,
}

impl EditorWindow {
    fn new(editor: &EditorSnapshot, parent: &gtk::Window) -> Rc<EditorWindow> {
        install_css();
        let id = editor.id;

        let title = adw::WindowTitle::new("", "");
        title.add_css_class("tl-editor-title");
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&title));
        let save = gtk::Button::with_label("Save");
        save.add_css_class("suggested-action");
        let tags = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        tags.set_valign(gtk::Align::Center);
        let tag = |label: &str, class: &str| {
            let tag = gtk::Label::new(Some(label));
            tag.add_css_class("tl-tag");
            tag.add_css_class(class);
            tag.set_valign(gtk::Align::Center);
            tags.append(&tag);
            tag
        };
        let sudo = tag("sudo", "sudo");
        let readonly = tag("read-only", "readonly");
        let modified = tag("modified", "modified");
        header.pack_end(&save);
        header.pack_end(&tags);

        let banner = adw::Banner::new("");
        let code = CodeView::new(true);

        let lines = gtk::Label::new(None);
        let language = gtk::Label::new(None);
        let hint = gtk::Label::new(None);
        hint.set_hexpand(true);
        hint.set_xalign(1.0);
        hint.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        footer.add_css_class("tl-code-footer");
        footer.append(&lines);
        footer.append(&language);
        footer.append(&hint);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&banner);
        body.append(&code.root);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&body));
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&toasts));
        view.add_bottom_bar(&footer);
        view.set_bottom_bar_style(adw::ToolbarStyle::Raised);

        let window = adw::Window::builder()
            .title(&editor.title)
            .default_width(960)
            .default_height(720)
            .width_request(420)
            .height_request(280)
            .content(&view)
            .build();
        // One of the app's windows, not a dialog of the main one.
        if let Some(app) = parent.application() {
            window.set_application(Some(&app));
        }

        let this = Rc::new(EditorWindow {
            id,
            window,
            title,
            sudo,
            readonly,
            modified,
            save,
            banner,
            toasts,
            code,
            lines,
            language,
            hint,
            snapshot: RefCell::new(editor.clone()),
            sent_changed: Cell::new(false),
            prompt: RefCell::new(None),
            seen_seq: Cell::new(editor.notification.as_ref().map_or(0, |n| n.seq)),
            dropped: Cell::new(false),
        });

        let weak = Rc::downgrade(&this);
        this.save.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.save();
            }
        });
        let weak = Rc::downgrade(&this);
        this.code.connect_edit(move || {
            if let Some(this) = weak.upgrade() {
                this.edited();
            }
        });
        let weak = Rc::downgrade(&this);
        this.code.connect_lines(move |n| {
            if let Some(this) = weak.upgrade() {
                this.lines.set_text(&if n == 1 { "1 line".to_string() } else { format!("{n} lines") });
            }
        });
        this.window.connect_close_request(move |_| {
            // The core decides; the window goes when it drops the editor.
            shell::send(Input::Editor {
                editor: id,
                input: EditorInput::Close,
            });
            glib::Propagation::Stop
        });
        let weak = Rc::downgrade(&this);
        this.window.connect_destroy(move |_| {
            // Gone some other way (the app quits): the core lets it go.
            if weak.upgrade().is_none_or(|this| !this.dropped.get()) {
                shell::send(Input::Editor {
                    editor: id,
                    input: EditorInput::Closed,
                });
            }
        });

        // Save and Close as the key table has them, before the text sees
        // the keys; not while the alert is up.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&this);
        keys.connect_key_pressed(move |ctrl, keyval, keycode, state| {
            let Some(this) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if this.window.visible_dialog().is_some() {
                return glib::Propagation::Proceed;
            }
            let group = ctrl
                .current_event()
                .and_then(|e| e.downcast::<gdk::KeyEvent>().ok())
                .map(|e| e.layout())
                .unwrap_or(0);
            let display = WidgetExt::display(&this.window);
            let spellings = keys::spellings(&display, keyval, keycode, state, group);
            let target = shell::with(|s| s.keys.borrow().target(&spellings, &[KeyContext::Editor]).cloned());
            match target {
                Some(KeyTarget::Editor { key: EditorKey::Save }) => {
                    this.save();
                    glib::Propagation::Stop
                }
                Some(KeyTarget::Editor { key: EditorKey::Close }) => {
                    this.window.close();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        this.window.add_controller(keys);
        this
    }

    fn update(self: &Rc<Self>, editor: &EditorSnapshot) {
        self.snapshot.replace(editor.clone());

        self.window.set_title(Some(&editor.title));
        let name = editor.full_path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(&editor.full_path);
        self.title.set_title(name);
        self.title.set_subtitle(&editor.full_path);
        self.title.set_tooltip_text(Some(&editor.full_path));
        self.sudo.set_visible(editor.elevated);
        self.readonly.set_visible(editor.readonly);
        self.modified.set_visible(editor.dirty);
        self.save.set_visible(!editor.readonly);
        self.save.set_label(&editor.save_label);
        self.save.set_sensitive(editor.can_save);
        let save_key = shell::with(|s| s.keys.borrow().label("save-file"));
        self.save
            .set_tooltip_text(Some(&save_key.map_or("Save".to_string(), |k| format!("Save ({k})"))));
        match &editor.notice {
            Some(notice) => {
                self.banner.set_title(&glib::markup_escape_text(notice));
                self.banner.set_revealed(true);
            }
            None => self.banner.set_revealed(false),
        }
        self.language.set_text(&editor.language);
        self.hint.set_text(&editor.hint);

        if !editor.dirty {
            self.sent_changed.set(false);
        }
        self.code.set_editable(!editor.readonly);
        if self.code.revision() != Some(editor.content_version) {
            let text = shell::core().editor_content(self.id);
            self.code.load(text, &editor.language, editor.content_version);
        }

        match &editor.close_prompt {
            Some(prompt) if self.prompt.borrow().is_none() => self.ask(prompt),
            Some(_) => {}
            None => {
                if let Some(alert) = self.prompt.take() {
                    alert.force_close();
                }
            }
        }

        if let Some(note) = &editor.notification
            && note.seq != self.seen_seq.get()
        {
            self.seen_seq.set(note.seq);
            let toast = adw::Toast::builder()
                .title(glib::markup_escape_text(&note.message))
                .timeout(if note.kind == ToastKind::Error { 6 } else { 2 })
                .priority(if note.kind == ToastKind::Error {
                    adw::ToastPriority::High
                } else {
                    adw::ToastPriority::Normal
                })
                .build();
            self.toasts.add_toast(toast);
        }
    }

    /// The core dropped the editor.
    fn drop_window(&self) {
        self.dropped.set(true);
        if let Some(alert) = self.prompt.take() {
            alert.force_close();
        }
        self.window.destroy();
    }

    /// The first edit of an editable file makes it modified; only the core
    /// decides that.
    fn edited(&self) {
        let snapshot = self.snapshot.borrow();
        if snapshot.readonly || snapshot.dirty || self.sent_changed.get() {
            return;
        }
        self.sent_changed.set(true);
        shell::send(Input::Editor {
            editor: self.id,
            input: EditorInput::Changed,
        });
    }

    fn save(&self) {
        if self.snapshot.borrow().readonly {
            return;
        }
        shell::send(Input::Editor {
            editor: self.id,
            input: EditorInput::Save { text: self.code.text() },
        });
    }

    /// "Save changes?"; the answer goes to the core.
    fn ask(self: &Rc<Self>, prompt: &tl_ffi::editor::ClosePrompt) {
        let alert = adw::AlertDialog::new(Some(&prompt.message), Some(&prompt.detail));
        let label = |ix: usize, fallback: &'static str| prompt.buttons.get(ix).map_or(fallback, String::as_str);
        // Cancel first: libadwaita lays the responses out left to right.
        alert.add_response("cancel", label(2, "Cancel"));
        alert.add_response("discard", label(1, "Discard"));
        alert.add_response("save", label(0, "Save"));
        alert.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        alert.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        alert.set_default_response(Some("save"));
        alert.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        alert.connect_response(None, move |_, response| {
            let Some(this) = weak.upgrade() else { return };
            let choice = match response {
                "save" => CloseChoice::Save { text: this.code.text() },
                "discard" => CloseChoice::DontSave,
                _ => CloseChoice::Cancel,
            };
            shell::send(Input::Editor {
                editor: this.id,
                input: EditorInput::CloseAnswer { choice },
            });
        });
        self.prompt.replace(Some(alert.clone()));
        alert.present(Some(&self.window));
    }
}

/// The tags' and the footer's style, once.
fn install_css() {
    thread_local! {
        static DONE: Cell<bool> = const { Cell::new(false) };
    }
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    let Some(display) = gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        r#"
.tl-tag { padding: 1px 8px; border-radius: 999px; font-size: smaller; font-weight: bold; }
.tl-tag.sudo { color: @error_color; background-color: alpha(@error_bg_color, 0.16); }
.tl-tag.readonly { background-color: alpha(currentColor, 0.10); }
.tl-tag.modified { color: @warning_color; background-color: alpha(@warning_bg_color, 0.20); }
.tl-editor-title .subtitle { font-family: monospace; }
.tl-code-footer { padding: 3px 10px; font-size: smaller; }
.tl-code-footer label { opacity: 0.7; }
"#,
    );
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}
