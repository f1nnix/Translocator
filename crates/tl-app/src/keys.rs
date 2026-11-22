//! Every command the keyboard can reach, with its default keys, and the
//! rules for writing, checking and showing keystrokes.
//!
//! The table is shared by every shell: each binds the keys it finds here
//! (or the user's, from [`crate::settings::Settings::keys`]) to its own
//! input system, and the settings window lists it. Keystrokes are written
//! the way the settings file stores them: `cmd-shift-g`, `f5`, `space`,
//! modifiers first, separated by `-`, chords by spaces.

use std::collections::BTreeMap;

use crate::model::Command;
use crate::pane::Side;

/// Where a command fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Context {
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

/// What an editor window's keys do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorKey {
    Save,
    Close,
}

/// What a command does when its keys are pressed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyTarget {
    /// A core command. `PageUp` and `PageDown` carry `rows: 0`: the shell
    /// fills in how many rows its list shows.
    Command(Command),
    Editor(EditorKey),
}

/// One keyboard command.
#[derive(Clone, Debug)]
pub struct CommandSpec {
    /// Stable name in the settings file. Never change one: a user's key
    /// would come back as a binding for nothing.
    pub id: &'static str,
    /// What the settings window calls it.
    pub label: &'static str,
    /// Heading it is listed under.
    pub group: &'static str,
    pub context: Context,
    /// Keys it has until the user says otherwise.
    pub defaults: &'static [&'static str],
    pub target: KeyTarget,
}

const fn spec(
    id: &'static str,
    label: &'static str,
    group: &'static str,
    context: Context,
    defaults: &'static [&'static str],
    target: KeyTarget,
) -> CommandSpec {
    CommandSpec {
        id,
        label,
        group,
        context,
        defaults,
        target,
    }
}

/// Every command, in the order the settings window lists them. A command
/// with no default keys is listed too, so one can be given to it.
pub static COMMANDS: &[CommandSpec] = &[
    spec("cursor-up", "Move up", "Cursor and marks", Context::Pane, &["up"], KeyTarget::Command(Command::CursorUp)),
    spec("cursor-down", "Move down", "Cursor and marks", Context::Pane, &["down"], KeyTarget::Command(Command::CursorDown)),
    spec("page-up", "Page up", "Cursor and marks", Context::Pane, &["pageup"], KeyTarget::Command(Command::PageUp { rows: 0 })),
    spec("page-down", "Page down", "Cursor and marks", Context::Pane, &["pagedown"], KeyTarget::Command(Command::PageDown { rows: 0 })),
    spec("cursor-home", "To the first row", "Cursor and marks", Context::Pane, &["home", "alt-up"], KeyTarget::Command(Command::CursorHome)),
    spec("cursor-end", "To the last row", "Cursor and marks", Context::Pane, &["end", "alt-down"], KeyTarget::Command(Command::CursorEnd)),
    spec("mark-up", "Mark and move up", "Cursor and marks", Context::Pane, &["shift-up"], KeyTarget::Command(Command::MarkUp)),
    spec("mark-down", "Mark and move down", "Cursor and marks", Context::Pane, &["shift-down"], KeyTarget::Command(Command::MarkDown)),
    spec("toggle-mark", "Mark or unmark", "Cursor and marks", Context::Pane, &["space", "insert"], KeyTarget::Command(Command::ToggleMark)),
    spec("mark-all", "Mark all", "Cursor and marks", Context::Pane, &["cmd-a"], KeyTarget::Command(Command::MarkAll)),
    spec("unmark-all", "Unmark all", "Cursor and marks", Context::Pane, &["cmd-shift-a"], KeyTarget::Command(Command::UnmarkAll)),
    spec("invert-marks", "Invert marks", "Cursor and marks", Context::Pane, &["cmd-alt-a"], KeyTarget::Command(Command::InvertMarks)),
    spec("mark-by-mask", "Mark by mask…", "Cursor and marks", Context::Pane, &[], KeyTarget::Command(Command::MarkByMask)),
    spec("unmark-by-mask", "Unmark by mask…", "Cursor and marks", Context::Pane, &[], KeyTarget::Command(Command::UnmarkByMask)),
    spec("open", "Open", "Navigation", Context::Pane, &["enter", "cmd-down"], KeyTarget::Command(Command::Open)),
    spec("go-parent", "Enclosing folder", "Navigation", Context::Pane, &["cmd-up"], KeyTarget::Command(Command::GoParent)),
    spec("go-back", "Back", "Navigation", Context::Pane, &["cmd-["], KeyTarget::Command(Command::GoBack)),
    spec("go-forward", "Forward", "Navigation", Context::Pane, &["cmd-]"], KeyTarget::Command(Command::GoForward)),
    spec("go-home", "Home folder", "Navigation", Context::Pane, &["cmd-shift-h"], KeyTarget::Command(Command::GoHome)),
    spec("go-to-path", "Go to folder…", "Navigation", Context::Pane, &["cmd-shift-g", "cmd-l"], KeyTarget::Command(Command::GoToPath)),
    spec("filter-backspace", "Delete a character of the filter", "Navigation", Context::Pane, &["backspace"], KeyTarget::Command(Command::FilterBackspace)),
    spec("refresh", "Reload the folder", "Navigation", Context::Pane, &["cmd-r"], KeyTarget::Command(Command::Refresh)),
    spec("toggle-hidden", "Show hidden files", "Navigation", Context::Workspace, &["cmd-shift-.", "cmd->"], KeyTarget::Command(Command::ToggleHidden)),
    spec("locations", "Locations…", "Navigation", Context::Pane, &["cmd-k"], KeyTarget::Command(Command::Locations(None))),
    spec("locations-left", "Locations for the left pane…", "Navigation", Context::Workspace, &["alt-f1"], KeyTarget::Command(Command::Locations(Some(Side::Left)))),
    spec("locations-right", "Locations for the right pane…", "Navigation", Context::Workspace, &["alt-f2"], KeyTarget::Command(Command::Locations(Some(Side::Right)))),
    spec("new-tab", "New tab", "Panes and tabs", Context::Pane, &["cmd-t"], KeyTarget::Command(Command::NewTab)),
    spec("close-tab", "Close tab", "Panes and tabs", Context::Pane, &["cmd-w"], KeyTarget::Command(Command::CloseTab)),
    spec("next-tab", "Next tab", "Panes and tabs", Context::Pane, &["ctrl-tab", "cmd-shift-]"], KeyTarget::Command(Command::NextTab)),
    spec("prev-tab", "Previous tab", "Panes and tabs", Context::Pane, &["ctrl-shift-tab", "cmd-shift-["], KeyTarget::Command(Command::PrevTab)),
    spec("switch-pane", "Other pane", "Panes and tabs", Context::Pane, &["tab"], KeyTarget::Command(Command::SwitchPane)),
    spec("swap-panes", "Swap the panes", "Panes and tabs", Context::Workspace, &["cmd-u"], KeyTarget::Command(Command::SwapPanes)),
    spec("mirror-pane", "Same folder in the other pane", "Panes and tabs", Context::Pane, &["cmd-="], KeyTarget::Command(Command::MirrorPane)),
    spec("quick-look", "Quick look at the file under the cursor", "Panes and tabs", Context::Workspace, &["cmd-y"], KeyTarget::Command(Command::ToggleQuickLook)),
    spec("toggle-place", "Save or forget this folder", "Saved folders", Context::Pane, &["cmd-d"], KeyTarget::Command(Command::TogglePlace)),
    spec("go-to-place-1", "First saved folder", "Saved folders", Context::Pane, &["cmd-1"], KeyTarget::Command(Command::GoToPlace(0))),
    spec("go-to-place-2", "Second saved folder", "Saved folders", Context::Pane, &["cmd-2"], KeyTarget::Command(Command::GoToPlace(1))),
    spec("go-to-place-3", "Third saved folder", "Saved folders", Context::Pane, &["cmd-3"], KeyTarget::Command(Command::GoToPlace(2))),
    spec("go-to-place-4", "Fourth saved folder", "Saved folders", Context::Pane, &["cmd-4"], KeyTarget::Command(Command::GoToPlace(3))),
    spec("go-to-place-5", "Fifth saved folder", "Saved folders", Context::Pane, &["cmd-5"], KeyTarget::Command(Command::GoToPlace(4))),
    spec("go-to-place-6", "Sixth saved folder", "Saved folders", Context::Pane, &["cmd-6"], KeyTarget::Command(Command::GoToPlace(5))),
    spec("go-to-place-7", "Seventh saved folder", "Saved folders", Context::Pane, &["cmd-7"], KeyTarget::Command(Command::GoToPlace(6))),
    spec("go-to-place-8", "Eighth saved folder", "Saved folders", Context::Pane, &["cmd-8"], KeyTarget::Command(Command::GoToPlace(7))),
    spec("go-to-place-9", "Ninth saved folder", "Saved folders", Context::Pane, &["cmd-9"], KeyTarget::Command(Command::GoToPlace(8))),
    spec("view-file", "View", "Files", Context::Pane, &["f3"], KeyTarget::Command(Command::ViewFile)),
    spec("edit-file", "Edit", "Files", Context::Pane, &["f4"], KeyTarget::Command(Command::EditFile)),
    spec("new-file", "New file…", "Files", Context::Pane, &["shift-f4"], KeyTarget::Command(Command::NewFile)),
    spec("copy-files", "Copy to the other pane…", "Files", Context::Pane, &["f5"], KeyTarget::Command(Command::CopyFiles)),
    spec("move-files", "Move to the other pane…", "Files", Context::Pane, &["f6"], KeyTarget::Command(Command::MoveFiles)),
    spec("rename-file", "Rename…", "Files", Context::Pane, &["f2", "shift-f6"], KeyTarget::Command(Command::RenameFile)),
    spec("new-folder", "New folder…", "Files", Context::Pane, &["f7", "cmd-shift-n"], KeyTarget::Command(Command::NewFolder)),
    spec("trash-files", "Move to Trash", "Files", Context::Pane, &["f8", "cmd-backspace"], KeyTarget::Command(Command::TrashFiles)),
    spec("delete-files", "Delete permanently…", "Files", Context::Pane, &["shift-f8", "cmd-alt-backspace"], KeyTarget::Command(Command::DeleteFiles)),
    spec("properties", "Properties…", "Files", Context::Pane, &["cmd-i"], KeyTarget::Command(Command::Properties)),
    spec("calc-size", "Calculate folder size", "Files", Context::Pane, &["alt-enter"], KeyTarget::Command(Command::CalcSize)),
    spec("copy-path", "Copy path", "Files", Context::Pane, &["cmd-alt-c"], KeyTarget::Command(Command::CopyPath)),
    spec("clipboard-copy", "Copy", "Files", Context::Pane, &["cmd-c"], KeyTarget::Command(Command::ClipboardCopy)),
    spec("clipboard-cut", "Cut", "Files", Context::Pane, &["cmd-x"], KeyTarget::Command(Command::ClipboardCut)),
    spec("clipboard-paste", "Paste", "Files", Context::Pane, &["cmd-v"], KeyTarget::Command(Command::ClipboardPaste)),
    spec("open-externally", "Open in the default application", "Files", Context::Pane, &["cmd-o"], KeyTarget::Command(Command::OpenExternally)),
    spec("reveal-in-finder", "Show in the system file manager", "Files", Context::Pane, &["cmd-shift-r"], KeyTarget::Command(Command::RevealInFinder)),
    spec("open-terminal", "Terminal in this folder", "Files", Context::Pane, &["cmd-shift-t"], KeyTarget::Command(Command::OpenTerminal)),
    spec("find-files", "Find files…", "Commands", Context::Pane, &["cmd-f", "alt-f7"], KeyTarget::Command(Command::FindFiles)),
    spec("compare-dirs", "Compare the two folders", "Commands", Context::Pane, &["shift-f2", "cmd-shift-c"], KeyTarget::Command(Command::CompareDirs)),
    spec("sync-dirs", "Synchronize the two folders…", "Commands", Context::Pane, &["cmd-alt-s"], KeyTarget::Command(Command::SyncDirs)),
    spec("multi-rename", "Multi-rename…", "Commands", Context::Pane, &["ctrl-m", "cmd-shift-m"], KeyTarget::Command(Command::MultiRename)),
    spec("open-archive", "Open as a folder", "Archives", Context::Pane, &["ctrl-pagedown"], KeyTarget::Command(Command::OpenArchive)),
    spec("pack-files", "Pack into the other pane…", "Archives", Context::Pane, &["alt-f5", "cmd-alt-p"], KeyTarget::Command(Command::PackFiles)),
    spec("extract-archive", "Extract to the other pane…", "Archives", Context::Pane, &["alt-f9", "cmd-alt-e"], KeyTarget::Command(Command::ExtractArchive)),
    spec("compress", "Compress…", "Archives", Context::Pane, &["shift-alt-f5", "cmd-alt-shift-p"], KeyTarget::Command(Command::Compress)),
    spec("decompress", "Decompress…", "Archives", Context::Pane, &["shift-alt-f9", "cmd-alt-shift-e"], KeyTarget::Command(Command::Decompress)),
    spec("toggle-sudo", "Work as root in this pane", "Connections", Context::Pane, &["cmd-shift-s"], KeyTarget::Command(Command::ToggleSudo)),
    spec("new-bookmark", "New bookmark…", "Connections", Context::Workspace, &[], KeyTarget::Command(Command::NewBookmark)),
    spec("bookmark-connection", "Bookmark this folder…", "Connections", Context::Workspace, &[], KeyTarget::Command(Command::BookmarkConnection)),
    spec("manage-bookmarks", "Bookmarks…", "Connections", Context::Workspace, &["cmd-b"], KeyTarget::Command(Command::ManageBookmarks)),
    spec("disconnect", "Disconnect", "Connections", Context::Workspace, &[], KeyTarget::Command(Command::Disconnect)),
    spec("focus-command-line", "Command line", "Window", Context::Workspace, &["cmd-e"], KeyTarget::Command(Command::FocusCommandLine)),
    spec("focus-pane", "Back to the file list", "Window", Context::CommandLine, &["escape"], KeyTarget::Command(Command::FocusPane)),
    spec("toggle-jobs", "Jobs", "Window", Context::Workspace, &["cmd-j"], KeyTarget::Command(Command::ToggleJobs)),
    spec("toggle-console", "Console", "Window", Context::Workspace, &["cmd-shift-j"], KeyTarget::Command(Command::ToggleConsole)),
    spec("open-settings", "Settings…", "Window", Context::Global, &["cmd-,"], KeyTarget::Command(Command::OpenSettings)),
    spec("quit", "Quit", "Window", Context::Global, &["cmd-q"], KeyTarget::Command(Command::Quit)),
    spec("save-file", "Save", "Viewer and editor", Context::Editor, &["cmd-s"], KeyTarget::Editor(EditorKey::Save)),
    spec("close-editor", "Close the window", "Viewer and editor", Context::Editor, &["cmd-w"], KeyTarget::Editor(EditorKey::Close)),
];

impl CommandSpec {
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

pub fn command(id: &str) -> Option<&'static CommandSpec> {
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

/// Commands that would answer to `keys` as well, in the same context. Two
/// commands in different contexts can share a key: one is in a text field,
/// the other in the file list.
pub fn conflicts(keys: &str, with: &CommandSpec, overrides: &BTreeMap<String, Vec<String>>) -> Vec<&'static CommandSpec> {
    COMMANDS
        .iter()
        .filter(|other| other.id != with.id && other.context == with.context)
        .filter(|other| other.keys(overrides).contains(&keys))
        .collect()
}

/// The first key of a command, as a label.
pub fn shortcut(id: &str, overrides: &BTreeMap<String, Vec<String>>, platform: Platform) -> Option<String> {
    let command = command(id)?;
    command.keys(overrides).first().map(|keys| pretty(keys, platform))
}

/// How a platform writes keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// `⌃⌥⇧⌘` symbols.
    Mac,
    /// `Ctrl+Alt+Shift+Super+`.
    Other,
}

/// One keystroke of a binding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keystroke {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    /// ⌘ on macOS, Super elsewhere.
    pub platform: bool,
    pub function: bool,
    /// The key: a character (`a`, `.`, `-`) or a name (`f5`, `space`,
    /// `pageup`).
    pub key: String,
}

impl Keystroke {
    /// Reads one keystroke (`cmd-shift-g`; `cmd--` and `cmd-` are ⌘-minus).
    pub fn parse(text: &str) -> Option<Keystroke> {
        let mut stroke = Keystroke::default();
        let mut rest = text;
        while let Some((head, tail)) = rest.split_once('-') {
            if !modifier(&mut stroke, head) {
                return None;
            }
            if tail.is_empty() {
                // A trailing dash is the minus key itself.
                stroke.key = "-".into();
                rest = "";
                break;
            }
            rest = tail;
        }
        if !rest.is_empty() {
            stroke.key = rest.to_string();
        }
        (!stroke.key.is_empty() && !stroke.key.contains(char::is_whitespace)).then_some(stroke)
    }

    /// Back in the settings file's spelling.
    pub fn unparse(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.control, "ctrl-"),
            (self.alt, "alt-"),
            (self.shift, "shift-"),
            (self.platform, "cmd-"),
            (self.function, "fn-"),
        ] {
            if on {
                out.push_str(name);
            }
        }
        out.push_str(&self.key);
        out
    }

    /// Whether it is only a modifier key on its own.
    pub fn is_modifier_only(&self) -> bool {
        matches!(
            self.key.as_str(),
            "shift" | "control" | "ctrl" | "alt" | "option" | "cmd" | "command" | "platform" | "fn" | "function"
        )
    }
}

fn modifier(stroke: &mut Keystroke, name: &str) -> bool {
    match name {
        "ctrl" | "control" => stroke.control = true,
        "alt" | "option" => stroke.alt = true,
        "shift" => stroke.shift = true,
        "cmd" | "super" | "win" | "platform" => stroke.platform = true,
        "fn" | "function" => stroke.function = true,
        _ => return false,
    }
    true
}

/// Checks a binding before it is stored.
pub fn validate(keys: &str) -> Result<(), String> {
    if keys.trim().is_empty() {
        return Err("Press the keys you want to use.".into());
    }
    for part in keys.split_whitespace() {
        Keystroke::parse(part).ok_or_else(|| format!("“{part}” is not a key combination."))?;
    }
    Ok(())
}

/// A binding as a label: `⌘E` on macOS, `Ctrl+E` elsewhere. A string that
/// does not parse is shown as it is.
pub fn pretty(keys: &str, platform: Platform) -> String {
    keys.split_whitespace().map(|k| pretty_one(k, platform)).collect::<Vec<_>>().join(" ")
}

fn pretty_one(keys: &str, platform: Platform) -> String {
    let Some(stroke) = Keystroke::parse(keys) else {
        return keys.to_string();
    };
    let mut out = String::new();
    let mods = [
        (stroke.control, "⌃", "Ctrl"),
        (stroke.alt, "⌥", "Alt"),
        (stroke.shift, "⇧", "Shift"),
        (stroke.platform, "⌘", "Super"),
    ];
    for (on, symbol, name) in mods {
        if on {
            match platform {
                Platform::Mac => out.push_str(symbol),
                Platform::Other => {
                    out.push_str(name);
                    out.push('+');
                }
            }
        }
    }
    out.push_str(&key_name(&stroke.key, platform));
    out
}

fn key_name(key: &str, platform: Platform) -> String {
    let mac = platform == Platform::Mac;
    match key {
        "up" => "↑".into(),
        "down" => "↓".into(),
        "left" => "←".into(),
        "right" => "→".into(),
        "enter" => "⏎".into(),
        "tab" => "⇥".into(),
        "space" => "Space".into(),
        "backspace" => if mac { "⌫" } else { "Backspace" }.into(),
        "delete" => if mac { "⌦" } else { "Delete" }.into(),
        "escape" => if mac { "⎋" } else { "Esc" }.into(),
        "pageup" => "PgUp".into(),
        "pagedown" => "PgDn".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "insert" => "Ins".into(),
        key => key.to_uppercase(),
    }
}

/// The table entry a command comes from, for menus that show its keys.
pub fn for_target(target: &KeyTarget) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|c| &c.target == target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_named_once() {
        assert!(COMMANDS.len() > 70);
        let mut ids: Vec<&str> = COMMANDS.iter().map(|c| c.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), COMMANDS.len());
        for c in COMMANDS {
            for keys in c.defaults {
                assert!(validate(keys).is_ok(), "{}: {keys}", c.id);
            }
        }
    }

    #[test]
    fn no_two_commands_share_a_default_key_in_one_context() {
        let none = BTreeMap::new();
        for c in COMMANDS {
            for keys in c.defaults {
                assert!(conflicts(keys, c, &none).is_empty(), "{} shares {keys}", c.id);
            }
        }
    }

    #[test]
    fn keystrokes_are_checked_and_written_back() {
        assert!(validate("").is_err());
        assert!(validate("cmd-a-b").is_err());
        assert!(validate("cmd-").is_ok());
        assert!(validate("ctrl-x ctrl-s").is_ok());
        assert_eq!(Keystroke::parse("cmd-").unwrap().key, "-");
        assert_eq!(Keystroke::parse("cmd-shift-.").unwrap().unparse(), "shift-cmd-.");
        assert_eq!(pretty("cmd-shift-g", Platform::Mac), "⇧⌘G");
        assert_eq!(pretty("ctrl-alt-backspace", Platform::Other), "Ctrl+Alt+Backspace");
        assert_eq!(pretty("ctrl-x ctrl-s", Platform::Mac), "⌃X ⌃S");
    }

    #[test]
    fn overrides_replace_the_defaults_and_can_empty_them() {
        let copy = command("copy-files").unwrap();
        let mut o = BTreeMap::new();
        assert_eq!(copy.keys(&o), ["f5"]);
        o.insert("copy-files".into(), vec!["f9".into()]);
        assert_eq!(copy.keys(&o), ["f9"]);
        assert!(copy.is_changed(&o));
        o.insert("copy-files".into(), vec![]);
        assert!(copy.keys(&o).is_empty());
        o.insert("copy-files".into(), vec!["f5".into()]);
        assert!(!copy.is_changed(&o));
        assert_eq!(shortcut("copy-files", &o, Platform::Mac).as_deref(), Some("F5"));
    }
}
