//! The shared key table (`tl_app::keys`) for menus, key handling and the
//! settings window: every command with the keys in effect, parsed and as
//! labels.
//!
//! Keys are spelled the way the settings file stores them (`cmd-shift-g`,
//! `f5`, `space`); `canonical` is that spelling normalised
//! (`shift-cmd-g`: ctrl, alt, shift, cmd, fn, then the key), which is what
//! a shell builds from a key event to look a command up.

use std::collections::BTreeMap;

use tl_app::keys::{self as core, Context, Keystroke, Platform};

use crate::input::Command;

/// How this build's platform writes keys: symbols on macOS, `Ctrl+` on
/// Linux (where `cmd` in a binding means Ctrl).
pub(crate) const PLATFORM: Platform = if cfg!(target_os = "macos") { Platform::Mac } else { Platform::Linux };

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyContext {
    /// The file list of a pane.
    Pane,
    /// Anywhere in the main window.
    Workspace,
    /// The command line under the panes.
    CommandLine,
    /// A viewer or editor window.
    Editor,
    /// Any window.
    Global,
}

impl From<Context> for KeyContext {
    fn from(c: Context) -> Self {
        match c {
            Context::Pane => KeyContext::Pane,
            Context::Workspace => KeyContext::Workspace,
            Context::CommandLine => KeyContext::CommandLine,
            Context::Editor => KeyContext::Editor,
            Context::Global => KeyContext::Global,
        }
    }
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EditorKey {
    Save,
    Close,
}

/// What a command's keys do.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq, Hash)]
pub enum KeyTarget {
    /// A core command. `PageUp` / `PageDown` carry `rows: 0`: the shell
    /// fills in how many rows its list shows.
    Command { command: Command },
    Editor { key: EditorKey },
}

/// One keystroke of a binding.
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct KeyStroke {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    /// ⌘.
    pub platform: bool,
    pub function: bool,
    /// A character (`a`, `.`, `-`) or a name (`f5`, `space`, `pageup`,
    /// `up`, `enter`, `tab`, `backspace`, `delete`, `escape`, `home`,
    /// `end`, `insert`).
    pub key: String,
}

impl From<Keystroke> for KeyStroke {
    fn from(k: Keystroke) -> Self {
        KeyStroke {
            control: k.control,
            alt: k.alt,
            shift: k.shift,
            platform: k.platform,
            function: k.function,
            key: k.key,
        }
    }
}

impl From<KeyStroke> for Keystroke {
    fn from(k: KeyStroke) -> Self {
        Keystroke {
            control: k.control,
            alt: k.alt,
            shift: k.shift,
            platform: k.platform,
            function: k.function,
            key: k.key,
        }
    }
}

/// One binding of a command (a keystroke, or a chord of several).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyBinding {
    /// As stored.
    pub keys: String,
    /// Normalised, for lookups.
    pub canonical: String,
    /// `⇧⌘G`.
    pub pretty: String,
    /// Parsed; empty when the stored text is not a key combination.
    pub strokes: Vec<KeyStroke>,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyCommand {
    /// Stable id (the settings file's name for it).
    pub id: String,
    /// What the settings window calls it.
    pub label: String,
    /// Heading it is listed under.
    pub group: String,
    pub context: KeyContext,
    pub target: KeyTarget,
    /// The keys in effect, the first one being what menus show.
    pub bindings: Vec<KeyBinding>,
    pub defaults: Vec<String>,
    /// The user changed its keys.
    pub changed: bool,
}

/// Another command answering to the same keys in the same context.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyConflict {
    pub id: String,
    pub label: String,
}

pub fn binding(keys: &str) -> KeyBinding {
    let parsed: Option<Vec<Keystroke>> = keys.split_whitespace().map(Keystroke::parse).collect();
    let strokes = parsed.unwrap_or_default();
    let canonical = if strokes.is_empty() {
        keys.to_string()
    } else {
        strokes.iter().map(Keystroke::unparse).collect::<Vec<_>>().join(" ")
    };
    KeyBinding {
        keys: keys.to_string(),
        canonical,
        pretty: core::pretty(keys, PLATFORM),
        strokes: strokes.into_iter().map(Into::into).collect(),
    }
}

/// The whole table with `overrides` (the settings' keys) applied.
pub fn table(overrides: &BTreeMap<String, Vec<String>>) -> Vec<KeyCommand> {
    core::COMMANDS
        .iter()
        .map(|spec| KeyCommand {
            id: spec.id.to_string(),
            label: spec.label.to_string(),
            group: spec.group.to_string(),
            context: spec.context.into(),
            target: match &spec.target {
                core::KeyTarget::Command(cmd) => KeyTarget::Command {
                    command: cmd.clone().into(),
                },
                core::KeyTarget::Editor(core::EditorKey::Save) => KeyTarget::Editor { key: EditorKey::Save },
                core::KeyTarget::Editor(core::EditorKey::Close) => KeyTarget::Editor { key: EditorKey::Close },
            },
            bindings: spec.keys(overrides).into_iter().map(binding).collect(),
            defaults: spec.defaults.iter().map(|k| k.to_string()).collect(),
            changed: spec.is_changed(overrides),
        })
        .collect()
}

pub fn conflicts(keys: &str, command_id: &str, overrides: &BTreeMap<String, Vec<String>>) -> Vec<KeyConflict> {
    let Some(spec) = core::command(command_id) else {
        return Vec::new();
    };
    core::conflicts(keys, spec, overrides)
        .into_iter()
        .map(|c| KeyConflict {
            id: c.id.to_string(),
            label: c.label.to_string(),
        })
        .collect()
}

/// The table with the default keys.
#[uniffi::export]
pub fn default_key_table() -> Vec<KeyCommand> {
    table(&BTreeMap::new())
}

/// Why `keys` cannot be stored as a binding, or `None` if it can.
#[uniffi::export]
pub fn validate_keys(keys: String) -> Option<String> {
    core::validate(&keys).err()
}

/// `⇧⌘G` for `cmd-shift-g` on macOS, `Ctrl+Shift+G` on Linux; text that does not parse comes back as it is.
#[uniffi::export]
pub fn pretty_keys(keys: String) -> String {
    core::pretty(&keys, PLATFORM)
}

/// The settings file's spelling of a keystroke (for recording keys).
#[uniffi::export]
pub fn keystroke_spelling(stroke: KeyStroke) -> String {
    Keystroke::from(stroke).unparse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_carries_effective_keys() {
        let mut o = BTreeMap::new();
        o.insert("copy-files".to_string(), vec!["cmd-shift-f5".to_string()]);
        let table = table(&o);
        let copy = table.iter().find(|c| c.id == "copy-files").unwrap();
        assert!(copy.changed);
        assert_eq!(copy.bindings[0].canonical, "shift-cmd-f5");
        let pretty = if cfg!(target_os = "macos") { "⇧⌘F5" } else { "Ctrl+Shift+F5" };
        assert_eq!(copy.bindings[0].pretty, pretty);
        assert_eq!(
            copy.target,
            KeyTarget::Command {
                command: Command::CopyFiles
            }
        );
        let hidden = table.iter().find(|c| c.id == "toggle-hidden").unwrap();
        assert_eq!(hidden.bindings[0].canonical, "shift-cmd-.");
        assert_eq!(hidden.bindings[1].canonical, "cmd->");
        assert_eq!(validate_keys("cmd-a-b".into()).as_deref(), Some("“cmd-a-b” is not a key combination."));
        assert_eq!(conflicts("f5", "move-files", &BTreeMap::new())[0].id, "copy-files");
    }
}
