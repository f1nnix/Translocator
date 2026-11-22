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
//! STUB: the snapshot carries the id and title only. The next step adds
//! the header (full path, sudo / read-only / modified tags), the save
//! state, the notice bar, the close prompt and the notification, and the
//! text: `content_version` in the snapshot plus `Core::editor_text(id)` to
//! fetch the content only when it changed. The Swift side is
//! `macos/Sources/Translocator/Areas/EditorWindow.swift`.

use tl_app::editor;

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct EditorSnapshot {
    pub id: u64,
    /// `name — parent[ (sudo)]`.
    pub title: String,
}

pub(crate) fn snapshot(view: &editor::EditorView) -> EditorSnapshot {
    EditorSnapshot {
        id: view.id.0,
        title: view.title.clone(),
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
