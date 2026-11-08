//! Every command the keyboard can reach, and the key map built from it.
//!
//! The table below is the single place a command's default keys are written:
//! the key map is built from it at startup, the settings window lists it, and
//! the function bar and menus read their shortcuts back out of it, so a
//! rebound key is never described wrongly.
//!
//! Rebinding at runtime cannot clear the key map — the component library has
//! its own bindings in there, for text fields and menus — so a change binds
//! GPUI's `Unbind` marker over the keystrokes this module installed last time
//! and then installs the new ones. Later bindings win, so the marker hides
//! the old key and the new one takes effect without a restart.

use std::collections::BTreeMap;

use gpui_kit::{Action, App, Global, KeyBinding, Keystroke, SharedString, Unbind};

use crate::actions::*;
use crate::app_state::AppState;

/// One keyboard command.
pub struct Command {
    /// Stable name in the settings file. Never change one: a user's key
    /// would come back as a binding for nothing.
    pub id: &'static str,
    /// What the settings window calls it.
    pub label: &'static str,
    /// Heading it is listed under.
    pub group: &'static str,
    /// Key context it belongs to, which decides where it fires.
    pub context: Option<&'static str>,
    /// Keys it has until the user says otherwise.
    pub defaults: &'static [&'static str],
    /// Builds a binding, or says why the keystroke cannot be used.
    build: fn(&str, Option<&str>) -> Result<KeyBinding, String>,
    /// The action's registered name, which is how a binding is unbound.
    name: fn() -> &'static str,
}

impl Command {
    /// The keys in effect: the user's, or the defaults.
    pub fn keys<'a>(&self, overrides: &'a BTreeMap<String, Vec<String>>) -> Vec<&'a str> {
        match overrides.get(self.id) {
            Some(keys) => keys.iter().map(String::as_str).collect(),
            None => self.defaults.to_vec(),
        }
    }

    /// Whether the user has changed this command's keys.
    pub fn is_changed(&self, overrides: &BTreeMap<String, Vec<String>>) -> bool {
        match overrides.get(self.id) {
            Some(keys) => keys.iter().map(String::as_str).ne(self.defaults.iter().copied()),
            None => false,
        }
    }
}

fn build<A: Action>(keys: &str, action: A, context: Option<&str>) -> Result<KeyBinding, String> {
    validate(keys)?;
    // Cannot panic: both the keystrokes and the contexts are checked, and
    // the contexts are the literals below.
    Ok(KeyBinding::new(keys, action, context))
}

fn name_of<A: Action>(_: &A) -> &'static str {
    A::name_for_type()
}

/// Whether GPUI can bind `keys`: a keystroke like `cmd-shift-p`, or several
/// separated by spaces for a chord.
pub fn validate(keys: &str) -> Result<(), String> {
    if keys.trim().is_empty() {
        return Err("Press the keys you want to use.".into());
    }
    for part in keys.split_whitespace() {
        Keystroke::parse(part).map_err(|_| format!("“{part}” is not a key combination."))?;
    }
    Ok(())
}

macro_rules! commands {
    ($($group:literal => [$(($id:literal, $label:literal, $ctx:expr, [$($keys:literal),*], $action:expr)),* $(,)?]),* $(,)?) => {
        &[$($(Command {
            id: $id,
            label: $label,
            group: $group,
            context: $ctx,
            defaults: &[$($keys),*],
            build: |keys, context| build(keys, $action, context),
            name: || name_of(&$action),
        }),*),*]
    };
}

/// Every command, in the order the settings window lists them. A command
/// with no default keys is listed too, so one can be given to it.
pub static COMMANDS: &[Command] = commands! {
    "Cursor and marks" => [
        ("cursor-up", "Move up", Some(PANE), ["up"], CursorUp),
        ("cursor-down", "Move down", Some(PANE), ["down"], CursorDown),
        ("page-up", "Page up", Some(PANE), ["pageup"], PageUp),
        ("page-down", "Page down", Some(PANE), ["pagedown"], PageDown),
        ("cursor-home", "To the first row", Some(PANE), ["home", "alt-up"], CursorHome),
        ("cursor-end", "To the last row", Some(PANE), ["end", "alt-down"], CursorEnd),
        ("mark-up", "Mark and move up", Some(PANE), ["shift-up"], MarkUp),
        ("mark-down", "Mark and move down", Some(PANE), ["shift-down"], MarkDown),
        ("toggle-mark", "Mark or unmark", Some(PANE), ["space", "insert"], ToggleMark),
        ("mark-all", "Mark all", Some(PANE), ["cmd-a"], MarkAll),
        ("unmark-all", "Unmark all", Some(PANE), ["cmd-shift-a"], UnmarkAll),
        ("invert-marks", "Invert marks", Some(PANE), ["cmd-alt-a"], InvertMarks),
        ("mark-by-mask", "Mark by mask…", Some(PANE), [], MarkByMask),
        ("unmark-by-mask", "Unmark by mask…", Some(PANE), [], UnmarkByMask),
    ],
    "Navigation" => [
        ("open", "Open", Some(PANE), ["enter", "cmd-down"], Open),
        ("go-parent", "Enclosing folder", Some(PANE), ["cmd-up"], GoParent),
        ("go-back", "Back", Some(PANE), ["cmd-["], GoBack),
        ("go-forward", "Forward", Some(PANE), ["cmd-]"], GoForward),
        ("go-home", "Home folder", Some(PANE), ["cmd-shift-h"], GoHome),
        ("go-to-path", "Go to folder…", Some(PANE), ["cmd-shift-g", "cmd-l"], GoToPath),
        ("filter-backspace", "Delete a character of the filter", Some(PANE), ["backspace"], FilterBackspace),
        ("refresh", "Reload the folder", Some(PANE), ["cmd-r"], Refresh),
        ("toggle-hidden", "Show hidden files", Some(WORKSPACE), ["cmd-shift-.", "cmd->"], ToggleHidden),
        ("locations", "Locations…", Some(PANE), ["cmd-k"], Locations),
        ("locations-left", "Locations for the left pane…", Some(WORKSPACE), ["alt-f1"], LocationsLeft),
        ("locations-right", "Locations for the right pane…", Some(WORKSPACE), ["alt-f2"], LocationsRight),
    ],
    "Panes and tabs" => [
        ("new-tab", "New tab", Some(PANE), ["cmd-t"], NewTab),
        ("close-tab", "Close tab", Some(PANE), ["cmd-w"], CloseTab),
        ("next-tab", "Next tab", Some(PANE), ["ctrl-tab", "cmd-shift-]"], NextTab),
        ("prev-tab", "Previous tab", Some(PANE), ["ctrl-shift-tab", "cmd-shift-["], PrevTab),
        ("switch-pane", "Other pane", Some(PANE), ["tab"], SwitchPane),
        ("swap-panes", "Swap the panes", Some(WORKSPACE), ["cmd-u"], SwapPanes),
        ("mirror-pane", "Same folder in the other pane", Some(PANE), ["cmd-="], MirrorPane),
        ("quick-look", "Quick look at the file under the cursor", Some(WORKSPACE), ["cmd-y"], ToggleQuickLook),
    ],
    "Saved folders" => [
        ("toggle-place", "Save or forget this folder", Some(PANE), ["cmd-d"], TogglePlace),
        ("go-to-place-1", "First saved folder", Some(PANE), ["cmd-1"], GoToPlace { index: 0 }),
        ("go-to-place-2", "Second saved folder", Some(PANE), ["cmd-2"], GoToPlace { index: 1 }),
        ("go-to-place-3", "Third saved folder", Some(PANE), ["cmd-3"], GoToPlace { index: 2 }),
        ("go-to-place-4", "Fourth saved folder", Some(PANE), ["cmd-4"], GoToPlace { index: 3 }),
        ("go-to-place-5", "Fifth saved folder", Some(PANE), ["cmd-5"], GoToPlace { index: 4 }),
        ("go-to-place-6", "Sixth saved folder", Some(PANE), ["cmd-6"], GoToPlace { index: 5 }),
        ("go-to-place-7", "Seventh saved folder", Some(PANE), ["cmd-7"], GoToPlace { index: 6 }),
        ("go-to-place-8", "Eighth saved folder", Some(PANE), ["cmd-8"], GoToPlace { index: 7 }),
        ("go-to-place-9", "Ninth saved folder", Some(PANE), ["cmd-9"], GoToPlace { index: 8 }),
    ],
    "Files" => [
        ("view-file", "View", Some(PANE), ["f3"], ViewFile),
        ("edit-file", "Edit", Some(PANE), ["f4"], EditFile),
        ("new-file", "New file…", Some(PANE), ["shift-f4"], NewFile),
        ("copy-files", "Copy to the other pane…", Some(PANE), ["f5"], CopyFiles),
        ("move-files", "Move to the other pane…", Some(PANE), ["f6"], MoveFiles),
        ("rename-file", "Rename…", Some(PANE), ["f2", "shift-f6"], RenameFile),
        ("new-folder", "New folder…", Some(PANE), ["f7", "cmd-shift-n"], NewFolder),
        ("trash-files", "Move to Trash", Some(PANE), ["f8", "cmd-backspace"], TrashFiles),
        ("delete-files", "Delete permanently…", Some(PANE), ["shift-f8", "cmd-alt-backspace"], DeleteFiles),
        ("properties", "Properties…", Some(PANE), ["cmd-i"], Properties),
        ("calc-size", "Calculate folder size", Some(PANE), ["alt-enter"], CalcSize),
        ("copy-path", "Copy path", Some(PANE), ["cmd-alt-c"], CopyPath),
        ("clipboard-copy", "Copy", Some(PANE), ["cmd-c"], ClipboardCopy),
        ("clipboard-cut", "Cut", Some(PANE), ["cmd-x"], ClipboardCut),
        ("clipboard-paste", "Paste", Some(PANE), ["cmd-v"], ClipboardPaste),
        ("open-externally", "Open in the default application", Some(PANE), ["cmd-o"], OpenExternally),
        ("reveal-in-finder", "Show in the system file manager", Some(PANE), ["cmd-shift-r"], RevealInFinder),
        ("open-terminal", "Terminal in this folder", Some(PANE), ["cmd-shift-t"], OpenTerminal),
    ],
    "Commands" => [
        ("find-files", "Find files…", Some(PANE), ["cmd-f", "alt-f7"], FindFiles),
        ("compare-dirs", "Compare the two folders", Some(PANE), ["shift-f2", "cmd-shift-c"], CompareDirs),
        ("sync-dirs", "Synchronize the two folders…", Some(PANE), ["cmd-alt-s"], SyncDirs),
        ("multi-rename", "Multi-rename…", Some(PANE), ["ctrl-m", "cmd-shift-m"], MultiRename),
    ],
    "Archives" => [
        ("open-archive", "Open as a folder", Some(PANE), ["ctrl-pagedown"], OpenArchive),
        ("pack-files", "Pack into the other pane…", Some(PANE), ["alt-f5", "cmd-alt-p"], PackFiles),
        ("extract-archive", "Extract to the other pane…", Some(PANE), ["alt-f9", "cmd-alt-e"], ExtractArchive),
        ("compress", "Compress…", Some(PANE), ["shift-alt-f5", "cmd-alt-shift-p"], Compress),
        ("decompress", "Decompress…", Some(PANE), ["shift-alt-f9", "cmd-alt-shift-e"], Decompress),
    ],
    "Connections" => [
        ("toggle-sudo", "Work as root in this pane", Some(PANE), ["cmd-shift-s"], ToggleSudo),
        ("new-bookmark", "New bookmark…", Some(WORKSPACE), [], NewBookmark),
        ("bookmark-connection", "Bookmark this folder…", Some(WORKSPACE), [], BookmarkConnection),
        ("manage-bookmarks", "Bookmarks…", Some(WORKSPACE), ["cmd-b"], ManageBookmarks),
        ("disconnect", "Disconnect", Some(WORKSPACE), [], Disconnect),
    ],
    "Window" => [
        ("focus-command-line", "Command line", Some(WORKSPACE), ["cmd-e"], FocusCommandLine),
        ("focus-pane", "Back to the file list", Some(COMMAND_LINE), ["escape"], FocusPane),
        ("toggle-jobs", "Jobs", Some(WORKSPACE), ["cmd-j"], ToggleJobs),
        ("toggle-console", "Console", Some(WORKSPACE), ["cmd-shift-j"], ToggleConsole),
        ("open-settings", "Settings…", None, ["cmd-,"], OpenSettings),
        ("quit", "Quit", None, ["cmd-q"], Quit),
    ],
    "Viewer and editor" => [
        ("save-file", "Save", Some(EDITOR), ["cmd-s"], SaveFile),
        ("close-editor", "Close the window", Some(EDITOR), ["cmd-w"], CloseEditor),
    ],
};

pub fn command(id: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.id == id)
}

/// The groups, in table order.
pub fn groups() -> Vec<&'static str> {
    let mut groups: Vec<&'static str> = Vec::new();
    for command in COMMANDS {
        if !groups.contains(&command.group) {
            groups.push(command.group);
        }
    }
    groups
}

/// What this process has installed, so a later change can take it back out.
#[derive(Default)]
struct Installed(Vec<(String, &'static str, Option<&'static str>)>);

impl Global for Installed {}

/// Installs the key map the settings ask for, replacing the one installed
/// before, and re-reads the menus so their shortcuts stay truthful.
///
/// Cheap to call when nothing has changed, which matters: resetting a page
/// of keys calls it once per command.
pub fn apply(cx: &mut App) {
    let overrides = cx.global::<AppState>().settings.keys.clone();
    let mut wanted = Vec::new();
    let mut bindings = Vec::new();
    for command in COMMANDS {
        for keys in command.keys(&overrides) {
            match (command.build)(keys, command.context) {
                Ok(binding) => {
                    bindings.push(binding);
                    wanted.push((keys.to_string(), (command.name)(), command.context));
                }
                Err(err) => log::warn!("ignoring the key “{keys}” for {}: {err}", command.id),
            }
        }
    }
    let previous = cx
        .try_global::<Installed>()
        .map(|installed| installed.0.clone())
        .unwrap_or_default();
    if wanted == previous {
        return;
    }
    // The markers go in first, so that the bindings added after them win.
    let mut all: Vec<KeyBinding> = previous
        .iter()
        .map(|(keys, action, context)| {
            KeyBinding::new(keys, Unbind(SharedString::from(*action)), *context)
        })
        .collect();
    all.append(&mut bindings);
    cx.bind_keys(all);
    cx.set_global(Installed(wanted));
    crate::actions::set_menus(cx);
}

/// Commands that would answer to `keys` as well, in the same context.
pub fn conflicts(keys: &str, with: &Command, cx: &App) -> Vec<&'static Command> {
    let overrides = &cx.global::<AppState>().settings.keys;
    COMMANDS
        .iter()
        .filter(|other| other.id != with.id)
        // Two commands in different contexts can share a key: one is in a
        // text field, the other in the file list.
        .filter(|other| other.context == with.context)
        .filter(|other| other.keys(overrides).iter().any(|k| *k == keys))
        .collect()
}

/// The first key of a command, written the way this platform writes keys:
/// `⌘E` on macOS, `Ctrl+E` elsewhere.
pub fn shortcut(id: &str, cx: &App) -> Option<SharedString> {
    let overrides = &cx.global::<AppState>().settings.keys;
    let command = command(id)?;
    let keys = command.keys(overrides);
    keys.first().map(|keys| pretty(keys).into())
}

/// A keystroke as a label. GPUI's parser takes any single word as a key
/// name, so a hand edited settings file shows what it says even when no
/// keyboard can produce it.
pub fn pretty(keys: &str) -> String {
    keys.split_whitespace().map(pretty_one).collect::<Vec<_>>().join(" ")
}

fn pretty_one(keys: &str) -> String {
    let Ok(stroke) = Keystroke::parse(keys) else {
        return keys.to_string();
    };
    let m = &stroke.modifiers;
    let mut out = String::new();
    if cfg!(target_os = "macos") {
        for (on, symbol) in [
            (m.control, "⌃"),
            (m.alt, "⌥"),
            (m.shift, "⇧"),
            (m.platform, "⌘"),
        ] {
            if on {
                out.push_str(symbol);
            }
        }
    } else {
        for (on, name) in [
            (m.control, "Ctrl"),
            (m.alt, "Alt"),
            (m.shift, "Shift"),
            (m.platform, "Super"),
        ] {
            if on {
                out.push_str(name);
                out.push('+');
            }
        }
    }
    out.push_str(&key_name(&stroke.key));
    out
}

/// The printed name of a key: the symbols people expect for the ones that
/// have them, the function keys in capitals, everything else as typed.
fn key_name(key: &str) -> String {
    match key {
        "up" => "↑".into(),
        "down" => "↓".into(),
        "left" => "←".into(),
        "right" => "→".into(),
        "enter" => "⏎".into(),
        "tab" => "⇥".into(),
        "space" => "Space".into(),
        "backspace" => if cfg!(target_os = "macos") { "⌫".into() } else { "Backspace".into() },
        "delete" => if cfg!(target_os = "macos") { "⌦".into() } else { "Delete".into() },
        "escape" => if cfg!(target_os = "macos") { "⎋".into() } else { "Esc".into() },
        "pageup" => "PgUp".into(),
        "pagedown" => "PgDn".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "insert" => "Ins".into(),
        key => key.to_uppercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_named_once_and_binds_its_defaults() {
        let mut ids = std::collections::HashSet::new();
        for command in COMMANDS {
            assert!(ids.insert(command.id), "two commands are called {}", command.id);
            assert!(!command.label.is_empty(), "{} has no label", command.id);
            assert!(
                groups().contains(&command.group),
                "{} is in no group",
                command.id
            );
            for keys in command.defaults {
                validate(keys).unwrap_or_else(|err| panic!("{}: {keys}: {err}", command.id));
                // A binding is what the key map is built from: if this
                // panics, startup would panic too.
                (command.build)(keys, command.context)
                    .unwrap_or_else(|err| panic!("{}: {keys}: {err}", command.id));
            }
        }
        assert!(COMMANDS.len() > 70, "the table lost commands");
    }

    #[test]
    fn no_two_commands_share_a_default_key_in_one_context() {
        let empty = BTreeMap::new();
        for command in COMMANDS {
            for keys in command.defaults {
                let clash = COMMANDS
                    .iter()
                    .filter(|other| other.id != command.id)
                    .filter(|other| other.context == command.context)
                    .find(|other| other.keys(&empty).contains(keys));
                assert!(
                    clash.is_none(),
                    "{keys} is both {} and {}",
                    command.id,
                    clash.unwrap().id
                );
            }
        }
    }

    #[test]
    fn overrides_replace_the_defaults_and_can_empty_them() {
        let copy = command("copy-files").unwrap();
        let mut overrides = BTreeMap::new();
        assert_eq!(copy.keys(&overrides), ["f5"]);
        assert!(!copy.is_changed(&overrides));

        overrides.insert("copy-files".to_string(), vec!["f9".to_string()]);
        assert_eq!(copy.keys(&overrides), ["f9"]);
        assert!(copy.is_changed(&overrides));

        overrides.insert("copy-files".to_string(), Vec::new());
        assert!(copy.keys(&overrides).is_empty());
        assert!(copy.is_changed(&overrides));

        // The default, written out, is not a change.
        overrides.insert("copy-files".to_string(), vec!["f5".to_string()]);
        assert!(!copy.is_changed(&overrides));
    }

    #[test]
    fn a_keystroke_is_checked_before_it_is_bound() {
        assert!(validate("cmd-shift-p").is_ok());
        assert!(validate("ctrl-x ctrl-s").is_ok(), "a chord is two keystrokes");
        assert!(validate("f5").is_ok());
        // "cmd-" is ⌘ and the minus key, not a keystroke cut short.
        assert!(validate("cmd-").is_ok());
        assert!(validate("").is_err());
        assert!(validate("   ").is_err());
        // Two keys in one keystroke is the mistake a hand edit makes.
        assert!(validate("cmd-a-b").is_err());
    }

    #[test]
    fn keys_are_written_the_way_the_platform_writes_them() {
        if cfg!(target_os = "macos") {
            assert_eq!(pretty("cmd-e"), "⌘E");
            assert_eq!(pretty("cmd-alt-shift-p"), "⌥⇧⌘P");
            assert_eq!(pretty("shift-f5"), "⇧F5");
            assert_eq!(pretty("cmd-backspace"), "⌘⌫");
        } else {
            assert_eq!(pretty("cmd-e"), "Super+E");
            assert_eq!(pretty("ctrl-shift-p"), "Ctrl+Shift+P");
        }
        assert_eq!(pretty("up"), "↑");
        assert_eq!(pretty("f5"), "F5");
        assert_eq!(pretty("cmd-x cmd-s"), pretty("cmd-x") + " " + &pretty("cmd-s"));
        // Not a key any keyboard has, but the parser takes the word, so the
        // label says what the settings file says.
        assert_eq!(pretty("nonsense"), "NONSENSE");
    }
}
