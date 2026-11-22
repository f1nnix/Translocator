//! The shared key table (`tl_ffi::keys`) on GTK: GTK accelerator strings
//! and labels for the menus, the function bar and tooltips, and the lookup
//! of a key event in the file list.
//!
//! Keys are spelled the way the settings file stores them (`cmd-shift-g`).
//! "cmd" is the primary modifier, which on Linux is Ctrl (Super belongs to
//! the desktop): every binding goes through the core's
//! `Platform::Linux.effective` first, so `cmd-shift-g` is Ctrl+Shift+G
//! everywhere here — accelerators, labels (`tl_app::keys::pretty` for
//! Linux) and the lookup of a key event, which is spelled the same way
//! (`ctrl-alt-shift-key`) to find its command.

use std::collections::HashMap;

use gtk::gdk;
use gtk::gdk::prelude::*;
use gtk::glib::translate::FromGlib;
use tl_app::keys::{Keystroke, Platform};
use tl_ffi::input::Command;
use tl_ffi::keys::{KeyBinding, KeyCommand, KeyContext, KeyStroke, KeyTarget};

/// How this shell presses the shared bindings.
pub const PLATFORM: Platform = Platform::Linux;

/// A keystroke as pressed here: `cmd` becomes Ctrl.
pub fn effective(stroke: &KeyStroke) -> Keystroke {
    PLATFORM.effective(Keystroke::from(stroke.clone()))
}

/// A binding's lookup spelling here (`cmd-shift-g` → `ctrl-shift-g`), or
/// `None` for a chord or text that is not a binding.
fn spelling(binding: &KeyBinding) -> Option<String> {
    match binding.strokes.as_slice() {
        [stroke] => Some(effective(stroke).unparse()),
        _ => None,
    }
}

/// The key table with the user's keys, indexed.
pub struct KeyMap {
    pub commands: Vec<KeyCommand>,
    by_id: HashMap<String, usize>,
    /// Single keystrokes by canonical spelling, per context.
    by_key: HashMap<(KeyContext, String), KeyTarget>,
}

impl KeyMap {
    pub fn new(commands: Vec<KeyCommand>) -> KeyMap {
        let mut by_id = HashMap::new();
        let mut by_key = HashMap::new();
        for (ix, command) in commands.iter().enumerate() {
            by_id.insert(command.id.clone(), ix);
            for binding in command.bindings.iter().filter_map(spelling) {
                by_key
                    .entry((command.context, binding))
                    .or_insert_with(|| command.target.clone());
            }
        }
        KeyMap {
            commands,
            by_id,
            by_key,
        }
    }

    pub fn command(&self, id: &str) -> Option<&KeyCommand> {
        self.by_id.get(id).map(|ix| &self.commands[*ix])
    }

    /// The table entry of a core command (the first one, for commands
    /// listed twice).
    pub fn command_for(&self, command: &Command) -> Option<&KeyCommand> {
        self.commands
            .iter()
            .find(|c| matches!(&c.target, KeyTarget::Command { command: c } if c == command))
    }

    /// What a key does in the first of `contexts` that binds it.
    pub fn target(&self, spellings: &[String], contexts: &[KeyContext]) -> Option<&KeyTarget> {
        contexts.iter().find_map(|context| {
            spellings
                .iter()
                .find_map(|spelling| self.by_key.get(&(*context, spelling.clone())))
        })
    }

    /// "Ctrl+Shift+G": the first binding of a command, as the core writes
    /// it for Linux.
    pub fn label(&self, id: &str) -> Option<String> {
        self.command(id)?.bindings.first().and_then(label)
    }

    /// The accelerator a menu item shows: the command's first binding,
    /// whatever its keys.
    pub fn menu_accel(&self, id: &str) -> Option<String> {
        let binding = self.command(id)?.bindings.first()?;
        (binding.strokes.len() == 1).then(|| accel(&binding.strokes[0])).flatten()
    }

    /// The accelerators GTK should fire a command on: its single-stroke
    /// bindings that a window can own without taking keys from text
    /// fields (Ctrl or Alt held, or a function key).
    /// The rest (arrows, Space, Enter, letters) are the file list's, which
    /// looks them up itself.
    pub fn accels(&self, command: &KeyCommand) -> Vec<String> {
        command
            .bindings
            .iter()
            .filter(|b| b.strokes.len() == 1)
            .map(|b| effective(&b.strokes[0]))
            .filter(|s| s.control || s.alt || function_key(&s.key))
            .filter_map(|s| accel_of(&s))
            .collect()
    }
}

fn function_key(key: &str) -> bool {
    key.strip_prefix('f')
        .and_then(|n| n.parse::<u32>().ok())
        .is_some_and(|n| (1..=24).contains(&n))
}

/// GTK's key name for a key of the shared spelling.
fn key_name(key: &str) -> Option<String> {
    let named = match key {
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "pageup" => "Page_Up",
        "pagedown" => "Page_Down",
        "home" => "Home",
        "end" => "End",
        "enter" => "Return",
        "tab" => "Tab",
        "backspace" => "BackSpace",
        "delete" => "Delete",
        "escape" => "Escape",
        "space" => "space",
        "insert" => "Insert",
        _ if function_key(key) => return Some(key.to_uppercase()),
        _ => {
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            // SAFETY: any keyval is a valid `Key`; an unknown character
            // comes back as a keyval with the Unicode flag set.
            let keyval = unsafe { gdk::Key::from_glib(gdk::unicode_to_keyval(c.to_ascii_lowercase() as u32)) };
            return keyval.name().map(|n| n.to_string());
        }
    };
    Some(named.to_string())
}

/// A keystroke as a GTK accelerator (`<Control><Shift>g`, `cmd` being
/// Ctrl), if GTK can parse it.
pub fn accel(stroke: &KeyStroke) -> Option<String> {
    accel_of(&effective(stroke))
}

fn accel_of(stroke: &Keystroke) -> Option<String> {
    let mut out = String::new();
    if stroke.control {
        out.push_str("<Control>");
    }
    if stroke.alt {
        out.push_str("<Alt>");
    }
    if stroke.shift {
        out.push_str("<Shift>");
    }
    if stroke.platform {
        // Not after `effective`; kept for completeness.
        out.push_str("<Super>");
    }
    out.push_str(&key_name(&stroke.key)?);
    gtk::accelerator_parse(&out).map(|_| out)
}

/// A binding as the core writes it for Linux: `Ctrl+Shift+G`, `F5`, `↑`.
pub fn label(binding: &KeyBinding) -> Option<String> {
    (!binding.strokes.is_empty()).then(|| tl_app::keys::pretty(&binding.keys, PLATFORM))
}

/// The shared spelling of a key without modifiers, by keyval.
fn event_key(keyval: gdk::Key) -> Option<String> {
    use gdk::Key as K;
    let named = match keyval {
        K::Up | K::KP_Up => "up",
        K::Down | K::KP_Down => "down",
        K::Left | K::KP_Left => "left",
        K::Right | K::KP_Right => "right",
        K::Page_Up | K::KP_Page_Up => "pageup",
        K::Page_Down | K::KP_Page_Down => "pagedown",
        K::Home | K::KP_Home => "home",
        K::End | K::KP_End => "end",
        K::Return | K::KP_Enter | K::ISO_Enter => "enter",
        K::Tab | K::ISO_Left_Tab | K::KP_Tab => "tab",
        K::BackSpace => "backspace",
        K::Delete | K::KP_Delete => "delete",
        K::Escape => "escape",
        K::space | K::KP_Space => "space",
        K::Insert | K::KP_Insert => "insert",
        _ => {
            let name = keyval.name()?;
            if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
                return Some(format!("f{n}"));
            }
            let c = keyval.to_unicode()?;
            if c.is_control() || c.is_whitespace() {
                return None;
            }
            return Some(c.to_lowercase().to_string());
        }
    };
    Some(named.to_string())
}

fn spell(key: &str, state: gdk::ModifierType, shift: bool) -> String {
    let mut out = String::new();
    if state.contains(gdk::ModifierType::CONTROL_MASK) {
        out.push_str("ctrl-");
    }
    if state.contains(gdk::ModifierType::ALT_MASK) {
        out.push_str("alt-");
    }
    if shift {
        out.push_str("shift-");
    }
    out.push_str(key);
    out
}

/// The spellings a key event may be bound under: the unshifted key with
/// its modifiers (`shift-cmd-.`), and for a shifted character also the
/// character it typed (`ctrl->`). The bindings are looked up in the same
/// spelling, `cmd` turned into Ctrl. With Ctrl or Alt held,
/// the key of the first layout is tried as well, so shortcuts work on a
/// non-Latin layout.
pub fn spellings(
    display: &gdk::Display,
    keyval: gdk::Key,
    keycode: u32,
    state: gdk::ModifierType,
    group: u32,
) -> Vec<String> {
    let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
    let unshifted = |group: u32| {
        display
            .translate_key(keycode, gdk::ModifierType::empty(), group as i32)
            .and_then(|(key, ..)| event_key(key))
    };
    let typed = event_key(keyval);
    let Some(base) = unshifted(group).or_else(|| typed.clone()) else {
        return Vec::new();
    };
    let mut out = vec![spell(&base, state, shift)];
    if let Some(typed) = &typed
        && shift
        && *typed != base
    {
        out.push(spell(typed, state, false));
    }
    let commanding = state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK);
    if commanding
        && group != 0
        && let Some(latin) = unshifted(0)
    {
        out.push(spell(&latin, state, shift));
    }
    out.dedup();
    out
}

/// The text a key types into the filter: a printable character with no
/// command modifier (Ctrl, Alt, Super) held.
pub fn typed_text(keyval: gdk::Key, state: gdk::ModifierType) -> Option<String> {
    let commands = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK;
    if state.intersects(commands) {
        return None;
    }
    let c = keyval.to_unicode()?;
    (!c.is_control()).then(|| c.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_spelled_the_shared_way() {
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        assert_eq!(spell("g", ctrl | gdk::ModifierType::SHIFT_MASK, true), "ctrl-shift-g");
        assert_eq!(spell(".", ctrl, false), "ctrl-.");
        // Super is the desktop's: it spells nothing.
        assert_eq!(spell("e", gdk::ModifierType::SUPER_MASK, false), "e");
        assert_eq!(spell("f5", gdk::ModifierType::ALT_MASK, false), "alt-f5");
        assert!(function_key("f12") && !function_key("f") && !function_key("f25"));
    }

    #[test]
    fn named_keys_have_gtk_names() {
        assert_eq!(key_name("pagedown").as_deref(), Some("Page_Down"));
        assert_eq!(key_name("enter").as_deref(), Some("Return"));
        assert_eq!(key_name("backspace").as_deref(), Some("BackSpace"));
        assert_eq!(key_name("f7").as_deref(), Some("F7"));
        assert_eq!(key_name("nonsense"), None);
    }

    #[test]
    fn cmd_is_ctrl_in_lookups_and_labels() {
        let table = tl_ffi::keys::default_key_table();
        let map = KeyMap::new(table);
        let target = |s: &str| map.target(&[s.to_string()], &[KeyContext::Pane]).cloned();
        assert_eq!(
            target("ctrl-shift-g"),
            Some(KeyTarget::Command {
                command: Command::GoToPath
            })
        );
        assert_eq!(
            map.target(&["ctrl->".to_string()], &[KeyContext::Workspace]).cloned(),
            Some(KeyTarget::Command {
                command: Command::ToggleHidden
            })
        );
        assert_eq!(map.label("go-to-path").as_deref(), Some("Ctrl+Shift+G"));
        assert_eq!(map.label("copy-files").as_deref(), Some("F5"));
        let stroke = |keys: &str| effective(&tl_ffi::keys::binding(keys).strokes[0]);
        assert!(stroke("cmd-q").control && !stroke("cmd-q").platform);
    }
}
