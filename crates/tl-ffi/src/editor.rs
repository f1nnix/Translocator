//! AREA: viewer and editor windows (F3 / F4).
//!
//! Owns [`EditorSnapshot`] (one per entry of `Snapshot.editors`) and
//! [`EditorInput`], the full mirror of `tl_app::editor::EditorInput`, sent
//! as `Input::Editor { editor, input }`.
//!
//! The core opens an editor by listing it in its view; the shell opens a
//! window for each listed id and closes it when the id leaves the list. A
//! window closed by the user sends `EditorInput::Close` (the core may ask
//! to save first, through `close_prompt`) and stays open until the core
//! drops it.
//!
//! The snapshot carries everything the window draws except the text,
//! which can be large: the shell fetches it with [`Core::editor_content`]
//! only when `content_version` moves, and its text view owns the live text
//! from then on. Colouring is the shell's, through [`crate::highlight`].
//! The Swift side is `macos/Sources/Translocator/Areas/EditorWindow.swift`.

use tl_app::editor;

use crate::Core;
use crate::types::ToastKind;

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct EditorSnapshot {
    pub id: u64,
    /// `name — parent[ (sudo)]`.
    pub title: String,
    /// The header's path, in a monospace font: `/path` locally,
    /// `label:/path` on a server.
    pub full_path: String,
    /// The "sudo" tag.
    pub elevated: bool,
    /// The "read-only" tag: the text cannot be edited and there is no
    /// Save button.
    pub readonly: bool,
    /// The "modified" tag.
    pub dirty: bool,
    pub saving: bool,
    /// "Save" or "Saving…".
    pub save_label: String,
    /// The Save button is enabled.
    pub can_save: bool,
    /// The syntax to colour the text with (`rust`, `text`), for
    /// [`crate::highlight::highlight`] and the footer.
    pub language: String,
    /// The warning bar under the header.
    pub notice: Option<String>,
    /// Moves whenever the text should replace what the text view holds:
    /// fetch it with `Core::editor_content`.
    pub content_version: u64,
    /// Ask "Save changes?" now (a native alert), answer with
    /// `EditorInput::CloseAnswer`.
    pub close_prompt: Option<ClosePrompt>,
    /// A transient banner in this window, shown again when `seq` moves.
    pub notification: Option<EditorNotification>,
    /// The footer's right side: `⌘S save · ⌘W close`.
    pub hint: String,
}

/// The "Save changes?" alert.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ClosePrompt {
    /// `Save changes to “name”?`
    pub message: String,
    pub detail: String,
    /// In order: Save, Don't Save, Cancel.
    pub buttons: Vec<String>,
}

/// A notification inside the editor's window ("Saved").
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct EditorNotification {
    pub kind: ToastKind,
    pub message: String,
    /// Moves for each one, so a repeated message shows again.
    pub seq: u64,
}

pub(crate) fn snapshot(view: &editor::EditorView, keys: &std::collections::BTreeMap<String, Vec<String>>) -> EditorSnapshot {
    EditorSnapshot {
        id: view.id.0,
        title: view.title.clone(),
        full_path: view.full_path.clone(),
        elevated: view.elevated,
        readonly: view.readonly,
        dirty: view.dirty,
        saving: view.saving,
        save_label: view.save_label.to_string(),
        can_save: view.can_save,
        language: view.language.to_string(),
        notice: view.notice.clone(),
        content_version: view.content_version,
        close_prompt: view.close_prompt.as_ref().map(|p| ClosePrompt {
            message: p.message.clone(),
            detail: p.detail.to_string(),
            buttons: p.buttons.iter().map(|b| b.to_string()).collect(),
        }),
        notification: view.notification.as_ref().map(|n| EditorNotification {
            kind: n.kind.into(),
            message: n.message.clone(),
            seq: n.seq,
        }),
        hint: hint(view.readonly, keys),
    }
}

/// The footer hint with the keys this platform and the user's settings
/// give the commands (`⌘S save · ⌘W close`, `Ctrl+S save · Ctrl+W close`).
fn hint(readonly: bool, keys: &std::collections::BTreeMap<String, Vec<String>>) -> String {
    let first = if readonly { ("view-file", "viewer") } else { ("save-file", "save") };
    [first, ("close-editor", "close")]
        .into_iter()
        .filter_map(|(id, what)| {
            tl_app::keys::shortcut(id, keys, crate::keys::PLATFORM).map(|k| format!("{k} {what}"))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

#[uniffi::export]
impl Core {
    /// The text of editor `id` as of its `content_version` in the last
    /// snapshot; empty when the editor is gone.
    pub fn editor_content(&self, id: u64) -> String {
        let view = self.view();
        view.editors
            .iter()
            .find(|e| e.id.0 == id)
            .map(|e| e.content.to_string())
            .unwrap_or_default()
    }
}

#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum EditorInput {
    /// The text changed (only the first edit of an editable file matters).
    Changed,
    /// ⌘S or the Save button, with the text.
    Save { text: String },
    /// ⌘W or the close button. The window stays until the core drops it.
    Close,
    /// The answer to the close prompt.
    CloseAnswer { choice: CloseChoice },
    /// Close without asking, discarding changes.
    CloseNow,
    /// The window is gone already (the app quits, the shell closed it).
    Closed,
}

#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum CloseChoice {
    /// Save, then close.
    Save { text: String },
    DontSave,
    Cancel,
}

impl From<EditorInput> for editor::EditorInput {
    fn from(input: EditorInput) -> Self {
        use editor::EditorInput as E;
        match input {
            EditorInput::Changed => E::Changed,
            EditorInput::Save { text } => E::Save { text },
            EditorInput::Close => E::Close,
            EditorInput::CloseAnswer { choice } => E::CloseAnswer(match choice {
                CloseChoice::Save { text } => editor::CloseChoice::Save { text },
                CloseChoice::DontSave => editor::CloseChoice::DontSave,
                CloseChoice::Cancel => editor::CloseChoice::Cancel,
            }),
            EditorInput::CloseNow => E::CloseNow,
            EditorInput::Closed => E::Closed,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tl_app::EditorId;
    use tl_app::editor::{ClosePrompt as CorePrompt, EditorView, Notice};

    use super::*;

    fn view() -> EditorView {
        EditorView {
            id: EditorId(7),
            title: "a.rs — ~/src (sudo)".into(),
            full_path: "/home/me/src/a.rs".into(),
            elevated: true,
            readonly: false,
            dirty: true,
            saving: false,
            save_label: "Save",
            can_save: true,
            language: "rust",
            notice: Some("Showing the first 8 MB of 9 MB.".into()),
            content: Arc::from("fn main() {}\n"),
            content_version: 3,
            close_prompt: Some(CorePrompt {
                message: "Save changes to “a.rs”?".into(),
                detail: "Your changes will be lost if you don't save them.",
                buttons: ["Save", "Don't Save", "Cancel"],
            }),
            notification: Some(Notice {
                kind: tl_app::ToastKind::Success,
                message: "Saved".into(),
                seq: 2,
            }),
            hint: "⌘S save · ⌘W close",
        }
    }

    #[test]
    fn the_snapshot_carries_all_but_the_text() {
        let s = snapshot(&view(), &Default::default());
        assert_eq!(s.id, 7);
        assert_eq!(s.title, "a.rs — ~/src (sudo)");
        assert_eq!(s.full_path, "/home/me/src/a.rs");
        assert!(s.elevated && !s.readonly && s.dirty && !s.saving && s.can_save);
        assert_eq!(s.save_label, "Save");
        assert_eq!(s.language, "rust");
        assert_eq!(s.notice.as_deref(), Some("Showing the first 8 MB of 9 MB."));
        assert_eq!(s.content_version, 3);
        let prompt = s.close_prompt.unwrap();
        assert_eq!(prompt.message, "Save changes to “a.rs”?");
        assert_eq!(prompt.buttons, ["Save", "Don't Save", "Cancel"]);
        let note = s.notification.unwrap();
        assert_eq!((note.kind, note.message.as_str(), note.seq), (ToastKind::Success, "Saved", 2));
        let expected = if cfg!(target_os = "macos") { "⌘S save · ⌘W close" } else { "Ctrl+S save · Ctrl+W close" };
        assert_eq!(s.hint, expected);
        let mut keys = std::collections::BTreeMap::new();
        keys.insert("save-file".to_string(), vec!["f2".to_string()]);
        keys.insert("close-editor".to_string(), vec![]);
        assert_eq!(snapshot(&view(), &keys).hint, "F2 save", "the user's keys, an unbound one left out");
    }

    #[test]
    fn inputs_map_to_the_core() {
        use editor::EditorInput as E;
        assert_eq!(E::from(EditorInput::Changed), E::Changed);
        assert_eq!(E::from(EditorInput::Save { text: "x".into() }), E::Save { text: "x".into() });
        assert_eq!(
            E::from(EditorInput::CloseAnswer {
                choice: CloseChoice::Save { text: "y".into() }
            }),
            E::CloseAnswer(editor::CloseChoice::Save { text: "y".into() })
        );
        assert_eq!(
            E::from(EditorInput::CloseAnswer {
                choice: CloseChoice::Cancel
            }),
            E::CloseAnswer(editor::CloseChoice::Cancel)
        );
        assert_eq!(E::from(EditorInput::Closed), E::Closed);
    }
}
