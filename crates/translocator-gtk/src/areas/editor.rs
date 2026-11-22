//! STUB: viewer and editor windows (F3 / F4), one per entry of
//! `Snapshot.editors`.
//!
//! Must become an `adw::Window` per [`EditorSnapshot`] (`tl_ffi::editor`):
//! an `adw::HeaderBar` with `title`, the `full_path` (monospace) and the
//! sudo / read-only / modified tags, Save (`save_label`, enabled by
//! `can_save`, `EditorInput::Save { text }`); `notice` as an `adw::Banner`;
//! a `gtk::TextView` (monospace, line numbers) holding the text, fetched
//! with `Core::editor_content(id)` only when `content_version` moves and
//! owned by the view after that (the first edit sends
//! `EditorInput::Changed`), coloured with `tl_ffi::highlight` for
//! `language` in the content theme's colours; `close_prompt` as an
//! `adw::AlertDialog` answered with `EditorInput::CloseAnswer`;
//! `notification` as a toast (again when its `seq` moves); `hint` in the
//! footer. The Editor context of the key table (save-file, close-editor)
//! as the window's own shortcuts. Closing the window sends
//! `EditorInput::Close` and keeps it open until the core drops it; when
//! the app quits, `EditorInput::Closed`.
//!
//! Called by the shell as `Editors::sync(&[EditorSnapshot], &window)` with
//! every snapshot. Swift: `macos/.../Areas/EditorWindow.swift` and
//! `CodeTextView.swift`.

use std::cell::RefCell;
use std::collections::HashMap;

use adw::prelude::*;
use tl_ffi::editor::{EditorInput, EditorSnapshot};
use tl_ffi::input::Input;

use crate::shell;

#[derive(Default)]
pub struct Editors {
    windows: RefCell<HashMap<u64, adw::Window>>,
}

impl Editors {
    /// Opens a window for each listed editor and closes those the core
    /// dropped.
    pub fn sync(&self, editors: &[EditorSnapshot], parent: &impl IsA<gtk::Window>) {
        let mut windows = self.windows.borrow_mut();
        windows.retain(|id, window| {
            let keep = editors.iter().any(|e| e.id == *id);
            if !keep {
                window.destroy();
            }
            keep
        });
        for editor in editors {
            if let Some(window) = windows.get(&editor.id) {
                window.set_title(Some(&editor.title));
                continue;
            }
            windows.insert(editor.id, placeholder(editor, parent));
        }
    }
}

fn placeholder(editor: &EditorSnapshot, parent: &impl IsA<gtk::Window>) -> adw::Window {
    let id = editor.id;
    let status = adw::StatusPage::builder()
        .icon_name("text-editor-symbolic")
        .title(&editor.full_path)
        .description("The viewer and editor are not available in the GTK app yet.")
        .vexpand(true)
        .build();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&status));
    let window = adw::Window::builder()
        .title(&editor.title)
        .default_width(720)
        .default_height(520)
        .transient_for(parent)
        .content(&view)
        .build();
    window.connect_close_request(move |_| {
        // The core decides; the window goes when it drops the editor.
        shell::send(Input::Editor {
            editor: id,
            input: EditorInput::Close,
        });
        gtk::glib::Propagation::Stop
    });
    window.present();
    window
}
