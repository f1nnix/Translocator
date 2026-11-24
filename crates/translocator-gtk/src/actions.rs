//! Every command of the shared key table as a window action
//! (`win.<command id>`), plus `app.about` and `app.help`, with the user's
//! keys as accelerators.
//!
//! An action is enabled by where its command fires, the way the macOS app
//! enables its menu items: file-list commands only while a list has the
//! keyboard and no dialog is up (so text fields keep their keys), window
//! commands while no dialog is up, global ones always. GTK skips the
//! accelerators of disabled actions, so a key reaches the widget instead.
//!
//! Only keys a window can own become accelerators (see
//! [`KeyMap::accels`]); the file list looks the others up itself.

use std::collections::HashMap;

use gtk::prelude::*;
use gtk::{gio, glib};
use tl_ffi::input::Command;
use tl_ffi::keys::{KeyContext, KeyTarget};
use tl_ffi::snapshot::Snapshot;
use tl_ffi::types::ViewMode;

use crate::keys::KeyMap;
use crate::shell;

/// Commands shown as check items, and how a snapshot says they are on.
fn toggle_state(id: &str, snapshot: &Snapshot) -> Option<bool> {
    let view = snapshot.pane(snapshot.active).view_mode;
    Some(match id {
        "view-as-list" => view == ViewMode::List,
        "view-as-columns" => view == ViewMode::Columns,
        "view-as-icons" => view == ViewMode::Grid,
        "toggle-hidden" => snapshot.settings.general.show_hidden,
        "quick-look" => snapshot.quick_look.is_some(),
        "toggle-jobs" => snapshot.jobs_open,
        "toggle-console" => snapshot.console_shown,
        _ => return None,
    })
}

pub struct Actions {
    /// By command id: the action and where its command fires.
    actions: HashMap<String, (gio::SimpleAction, KeyContext)>,
    /// What the accelerators were last set to, by action name.
    accels: std::cell::RefCell<HashMap<String, Vec<String>>>,
}

impl Actions {
    pub fn install(window: &adw::ApplicationWindow, app: &adw::Application, keys: &KeyMap, snapshot: &Snapshot) -> Actions {
        let mut actions = HashMap::new();
        for spec in &keys.commands {
            let KeyTarget::Command { command } = &spec.target else { continue };
            if spec.context == KeyContext::Editor {
                continue;
            }
            let action = match toggle_state(&spec.id, snapshot) {
                Some(on) => gio::SimpleAction::new_stateful(&spec.id, None, &on.to_variant()),
                None => gio::SimpleAction::new(&spec.id, None),
            };
            let command = command.clone();
            action.connect_activate(move |_, _| shell::command(command.clone()));
            window.add_action(&action);
            actions.insert(spec.id.clone(), (action, spec.context));
        }
        for (name, command) in [("about", Command::About), ("help", Command::OpenHelp)] {
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(move |_, _| shell::command(command.clone()));
            app.add_action(&action);
        }
        let actions = Actions {
            actions,
            accels: Default::default(),
        };
        actions.set_accels(app, keys);
        actions
    }

    /// Sets the accelerators from the keys in effect.
    pub fn set_accels(&self, app: &adw::Application, keys: &KeyMap) {
        let mut seen = self.accels.borrow_mut();
        for spec in &keys.commands {
            if !self.actions.contains_key(&spec.id) {
                continue;
            }
            let name = format!("win.{}", spec.id);
            let accels = keys.accels(spec);
            if seen.get(&name) == Some(&accels) {
                continue;
            }
            let refs: Vec<&str> = accels.iter().map(String::as_str).collect();
            app.set_accels_for_action(&name, &refs);
            seen.insert(name, accels);
        }
    }

    /// Enables each action by its context.
    pub fn set_enabled(&self, pane_focused: bool, dialogs: bool) {
        for (action, context) in self.actions.values() {
            let enabled = match context {
                KeyContext::Pane => pane_focused && !dialogs,
                KeyContext::Workspace | KeyContext::CommandLine => !dialogs,
                KeyContext::Editor | KeyContext::Global => true,
            };
            if action.is_enabled() != enabled {
                action.set_enabled(enabled);
            }
        }
    }

    /// The check items' states.
    pub fn set_states(&self, snapshot: &Snapshot) {
        for (id, (action, _)) in &self.actions {
            if let Some(on) = toggle_state(id, snapshot)
                && action.state().and_then(|s| s.get::<bool>()) != Some(on)
            {
                action.set_state(&on.to_variant());
            }
        }
    }
}

/// The help overlay's sections: every command of the key table with its
/// keys, by group.
pub fn shortcuts_window(keys: &KeyMap) -> gtk::ShortcutsWindow {
    let window: gtk::ShortcutsWindow = glib::Object::builder().property("modal", true).build();
    let section: gtk::ShortcutsSection = glib::Object::builder()
        .property("section-name", "shortcuts")
        .property("max-height", 14u32)
        .build();
    let mut groups: Vec<(String, gtk::ShortcutsGroup)> = Vec::new();
    for command in &keys.commands {
        let accels: Vec<String> = command
            .bindings
            .iter()
            .filter(|b| b.strokes.len() == 1)
            .filter_map(|b| crate::keys::accel(&b.strokes[0]))
            .collect();
        if accels.is_empty() {
            continue;
        }
        let group = match groups.iter().find(|(name, _)| *name == command.group) {
            Some((_, group)) => group.clone(),
            None => {
                let group: gtk::ShortcutsGroup = glib::Object::builder().property("title", &command.group).build();
                section.add_group(&group);
                groups.push((command.group.clone(), group.clone()));
                group
            }
        };
        let shortcut: gtk::ShortcutsShortcut = glib::Object::builder()
            .property("title", &command.label)
            .property("accelerator", accels.join(" "))
            .build();
        group.add_shortcut(&shortcut);
    }
    window.add_section(&section);
    window
}
