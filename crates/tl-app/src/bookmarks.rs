//! The bookmark editor and the bookmarks manager. The editor is generic: it
//! shows whatever fields the bookmark's plugin declares, so new connection
//! types need no UI code.
//!
//! Saving checks the fields every plugin gets checked
//! ([`tl_model::plugin::validate_fields`]), then asks the plugin itself
//! ([`PluginOp::Validate`]); a bookmark without a name is named by the
//! plugin's one-line summary ([`PluginOp::Summaries`]). Both answers come
//! back asynchronously; the dialog stays open, with the error under the
//! fields, until the bookmark is saved.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crux_core::Command;
use tl_model::plugin::validate_fields;
use tl_model::{Bookmark, BookmarkConfig, FieldInfo, FieldKind, PluginInfo, VPath};

use crate::Out;
use crate::capabilities::{PluginOp, PluginOutput};
use crate::conn::{LOCAL_PLUGIN, tilde};
use crate::dialog::{self, ConfirmPurpose, DialogKind};
use crate::ids::DialogId;
use crate::model::{Model, Reply, shell};
use crate::ops;
use crate::pane::Side;

/// The plugin a new bookmark starts with.
pub const DEFAULT_PLUGIN: &str = "ssh";

/// The manager's title, and what it shows without bookmarks.
pub const MANAGER_TITLE: &str = "Bookmarks";
pub const NO_BOOKMARKS: &str = "No bookmarks yet. Create one to connect to a server over SSH.";
/// The name field's placeholder.
pub const NAME_PLACEHOLDER: &str = "Shown in menus";

/// A field of one plugin: `(plugin id, field key)`.
pub type FieldId = (String, String);

#[derive(Clone, Debug, PartialEq)]
pub struct EditDialog {
    /// The bookmark's id (kept when saving).
    pub id: String,
    /// A new bookmark: the title says so and the type can be picked.
    pub is_new: bool,
    /// The selected plugin.
    pub plugin: String,
    pub name: String,
    /// The text of every non-toggle field of every plugin, so switching
    /// the type keeps what was typed.
    pub values: BTreeMap<FieldId, String>,
    /// Every toggle field of every plugin.
    pub toggles: BTreeMap<FieldId, bool>,
    /// "Show advanced options" is on.
    pub advanced: bool,
    /// Why the last save failed (danger text under the fields).
    pub error: Option<String>,
    /// The side "Save & Open" opens the bookmark in (the left one if none).
    pub connect_to: Option<Side>,
    /// The field the keyboard starts in: the plugin's "host" field, or the
    /// name field when `None`.
    pub autofocus: Option<String>,
    /// A save waits for the plugin's answer; the number tells the answers
    /// of successive saves apart.
    pub saving: Option<u64>,
    saves: u64,
}

impl EditDialog {
    pub fn title(&self) -> &'static str {
        if self.is_new {
            "New Bookmark"
        } else {
            "Edit Bookmark"
        }
    }

    pub fn plugin_info<'a>(&self, plugins: &'a [PluginInfo]) -> Option<&'a PluginInfo> {
        plugins.iter().find(|p| p.id == self.plugin)
    }

    /// The line under the type picker.
    pub fn description<'a>(&self, plugins: &'a [PluginInfo]) -> &'a str {
        self.plugin_info(plugins)
            .map(|p| p.description.as_str())
            .unwrap_or("")
    }

    /// The fields shown after "Name", in order: advanced ones only with
    /// "Show advanced options".
    pub fn fields<'a>(&self, plugins: &'a [PluginInfo]) -> Vec<&'a FieldInfo> {
        self.plugin_info(plugins)
            .map(|p| {
                p.fields
                    .iter()
                    .filter(|f| self.advanced || !f.advanced)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The advanced-options button is shown.
    pub fn has_advanced(&self, plugins: &[PluginInfo]) -> bool {
        self.plugin_info(plugins)
            .is_some_and(|p| p.fields.iter().any(|f| f.advanced))
    }

    pub fn advanced_label(&self) -> &'static str {
        if self.advanced {
            "Hide advanced options"
        } else {
            "Show advanced options"
        }
    }

    /// A text field's value for the selected plugin.
    pub fn value(&self, key: &str) -> &str {
        self.values
            .get(&(self.plugin.clone(), key.to_string()))
            .map(String::as_str)
            .unwrap_or("")
    }

    /// A toggle's state for the selected plugin.
    pub fn toggle(&self, key: &str) -> bool {
        self.toggles
            .get(&(self.plugin.clone(), key.to_string()))
            .copied()
            .unwrap_or(false)
    }

    /// "Choose…" picks folders (for the local plugin) rather than files.
    pub fn picks_directories(&self) -> bool {
        self.plugin == LOCAL_PLUGIN
    }

    /// The configuration as saved: toggles as "true", text trimmed, empty
    /// values and "false" left out.
    pub fn config(&self, plugins: &[PluginInfo]) -> BookmarkConfig {
        let mut config = BookmarkConfig::new();
        let Some(plugin) = self.plugin_info(plugins) else {
            return config;
        };
        for spec in &plugin.fields {
            let key = (plugin.id.clone(), spec.key.clone());
            let value = if spec.kind == FieldKind::Toggle {
                self.toggles.get(&key).copied().unwrap_or(false).to_string()
            } else {
                self.values
                    .get(&key)
                    .map(|v| v.trim().to_string())
                    .unwrap_or_default()
            };
            if !value.is_empty() && value != "false" {
                config.insert(spec.key.clone(), value);
            }
        }
        config
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ManageDialog {
    /// "Connect" opens bookmarks in this side.
    pub side: Side,
}

/// A row of the manager: the summary under the bookmark's name.
pub fn summary(
    bookmark: &Bookmark,
    plugins: &[PluginInfo],
    summaries: &HashMap<String, String>,
) -> String {
    if !plugins.iter().any(|p| p.id == bookmark.plugin) {
        return format!("unknown type “{}”", bookmark.plugin);
    }
    summaries.get(&bookmark.id).cloned().unwrap_or_default()
}

/// A row's open button: "Open" when the bookmark is connected.
pub fn open_label(connected: bool) -> &'static str {
    if connected { "Open" } else { "Connect" }
}

#[derive(Clone, Debug, PartialEq)]
pub enum BookmarksInput {
    // ----- the editor -----
    /// Save (`open: false`), "Save & Open" or Enter (`open: true`).
    Save {
        dialog: DialogId,
        open: bool,
    },
    Name {
        dialog: DialogId,
        text: String,
    },
    /// A text field of the selected plugin.
    Field {
        dialog: DialogId,
        key: String,
        value: String,
    },
    /// A toggle of the selected plugin.
    Toggle {
        dialog: DialogId,
        key: String,
        on: bool,
    },
    /// "Choose…" picked a local file or folder for a field.
    PathChosen {
        dialog: DialogId,
        key: String,
        path: VPath,
    },
    /// The type picker (new bookmarks only).
    Plugin {
        dialog: DialogId,
        plugin: String,
    },
    /// "Show / Hide advanced options".
    ToggleAdvanced {
        dialog: DialogId,
    },

    // ----- the manager -----
    /// The up (-1) and down (+1) arrows.
    Move {
        id: String,
        delta: isize,
    },
    Edit {
        manager: DialogId,
        id: String,
    },
    /// Asks first.
    Delete {
        id: String,
    },
    /// "Connect" / "Open": closes the manager.
    Open {
        manager: DialogId,
        id: String,
    },
    /// "New Bookmark…".
    New {
        manager: DialogId,
    },
}

#[derive(Debug)]
pub enum BookmarksReply {
    /// The plugin checked the bookmark a save is for.
    Validated {
        dialog: DialogId,
        save: u64,
        open: bool,
        bookmark: Box<Bookmark>,
        out: PluginOutput,
    },
    /// The plugin summarised a bookmark saved without a name.
    Named {
        dialog: DialogId,
        save: u64,
        open: bool,
        bookmark: Box<Bookmark>,
        out: PluginOutput,
    },
}

fn default_plugin(model: &Model) -> String {
    if model.plugins.iter().any(|p| p.id == DEFAULT_PLUGIN) {
        DEFAULT_PLUGIN.to_string()
    } else {
        model
            .plugins
            .first()
            .map(|p| p.id.clone())
            .unwrap_or_else(|| DEFAULT_PLUGIN.to_string())
    }
}

/// Opens the editor: `template` edits (or, if its id is unknown, creates)
/// that bookmark; `connect_to` is the side "Save & Open" opens it in.
pub fn edit(
    model: &mut Model,
    template: Option<Bookmark>,
    connect_to: Option<Side>,
    _out: &mut Out,
) {
    let is_new = template
        .as_ref()
        .is_none_or(|t| model.bookmarks.get(&t.id).is_none());
    let bookmark = match template {
        Some(b) => b,
        None => {
            let plugin = default_plugin(model);
            Bookmark::with_id(model.ids.uuid(), "", plugin)
        }
    };
    let mut values = BTreeMap::new();
    let mut toggles = BTreeMap::new();
    for plugin in model.plugins.iter() {
        for spec in &plugin.fields {
            let current = (plugin.id == bookmark.plugin)
                .then(|| bookmark.config.get(&spec.key).cloned())
                .flatten();
            let key = (plugin.id.clone(), spec.key.clone());
            if spec.kind == FieldKind::Toggle {
                let on = current
                    .as_deref()
                    .map(|v| v == "true")
                    .unwrap_or(spec.default == "true");
                toggles.insert(key, on);
            } else {
                values.insert(key, current.unwrap_or_else(|| spec.default.clone()));
            }
        }
    }
    let autofocus = values
        .contains_key(&(bookmark.plugin.clone(), "host".to_string()))
        .then(|| "host".to_string());
    let dialog = EditDialog {
        id: bookmark.id,
        is_new,
        plugin: bookmark.plugin,
        name: bookmark.name,
        values,
        toggles,
        advanced: false,
        error: None,
        connect_to,
        autofocus,
        saving: None,
        saves: 0,
    };
    dialog::open(model, DialogKind::BookmarkEdit(Box::new(dialog)));
}

/// Opens the manager; "Connect" opens bookmarks in `side`.
pub fn manage(model: &mut Model, side: Side, _out: &mut Out) {
    dialog::open(model, DialogKind::Bookmarks(ManageDialog { side }));
}

fn editor(model: &mut Model, id: DialogId) -> Option<&mut EditDialog> {
    match &mut model.dialog_mut(id)?.kind {
        DialogKind::BookmarkEdit(d) => Some(d),
        _ => None,
    }
}

fn manager_side(model: &Model, id: DialogId) -> Option<Side> {
    match &model.dialog(id)?.kind {
        DialogKind::Bookmarks(m) => Some(m.side),
        _ => None,
    }
}

pub fn input(model: &mut Model, input: BookmarksInput, out: &mut Out) {
    match input {
        BookmarksInput::Save { dialog, open } => save(model, dialog, open, out),
        BookmarksInput::Name { dialog, text } => {
            if let Some(d) = editor(model, dialog) {
                d.name = text;
            }
        }
        BookmarksInput::Field { dialog, key, value } => {
            if let Some(d) = editor(model, dialog) {
                d.values.insert((d.plugin.clone(), key), value);
            }
        }
        BookmarksInput::Toggle { dialog, key, on } => {
            if let Some(d) = editor(model, dialog) {
                d.toggles.insert((d.plugin.clone(), key), on);
            }
        }
        BookmarksInput::PathChosen { dialog, key, path } => {
            let home = model.conns.local().and_then(|c| c.local_home.clone());
            let text = tilde(&path, home.as_deref());
            if let Some(d) = editor(model, dialog) {
                d.values.insert((d.plugin.clone(), key), text);
            }
        }
        BookmarksInput::Plugin { dialog, plugin } => {
            let known = model.plugins.iter().any(|p| p.id == plugin);
            if let Some(d) = editor(model, dialog)
                && d.is_new
                && known
            {
                d.plugin = plugin;
                d.error = None;
            }
        }
        BookmarksInput::ToggleAdvanced { dialog } => {
            if let Some(d) = editor(model, dialog) {
                d.advanced = !d.advanced;
            }
        }
        BookmarksInput::Move { id, delta } => {
            if Arc::make_mut(&mut model.bookmarks).reorder(&id, delta) {
                model.save_bookmarks(out, "bookmarks");
            }
        }
        BookmarksInput::Edit { manager, id } => {
            let side = manager_side(model, manager);
            if let Some(b) = model.bookmarks.get(&id).cloned() {
                edit(model, Some(b), side, out);
            }
        }
        BookmarksInput::Delete { id } => {
            let Some(b) = model.bookmarks.get(&id) else {
                return;
            };
            let title = format!("Delete bookmark “{}”?", b.name);
            dialog::ask_confirm(
                model,
                title,
                "Open connections stay open until you disconnect.",
                "Delete",
                true,
                ConfirmPurpose::DeleteBookmark { id },
            );
        }
        BookmarksInput::Open { manager, id } => {
            let Some(side) = manager_side(model, manager) else {
                return;
            };
            model.close_dialog(manager);
            ops::open_bookmark(model, id, side, out);
        }
        BookmarksInput::New { manager } => {
            let side = manager_side(model, manager);
            edit(model, None, side, out);
        }
    }
}

/// Checks the form and asks the plugin to check it too.
fn save(model: &mut Model, dialog: DialogId, open: bool, out: &mut Out) {
    let plugins = model.plugins.clone();
    let Some(d) = editor(model, dialog) else {
        return;
    };
    let Some(plugin) = d.plugin_info(&plugins) else {
        return;
    };
    let config = d.config(&plugins);
    if let Err(err) = validate_fields(&plugin.fields, &config) {
        d.error = Some(err);
        return;
    }
    let mut bookmark = Bookmark::with_id(d.id.clone(), d.name.trim(), d.plugin.clone());
    bookmark.config = config.clone();
    d.saves += 1;
    let save = d.saves;
    d.saving = Some(save);
    let op = PluginOp::Validate {
        plugin: d.plugin.clone(),
        config,
    };
    let bookmark = Box::new(bookmark);
    out.push(Command::request_from_shell(op).then_send(move |out| {
        shell(Reply::Bookmarks(BookmarksReply::Validated {
            dialog,
            save,
            open,
            bookmark,
            out,
        }))
    }));
}

pub fn reply(model: &mut Model, reply: BookmarksReply, out: &mut Out) {
    match reply {
        BookmarksReply::Validated {
            dialog,
            save,
            open,
            bookmark,
            out: answer,
        } => {
            let Some(d) = editor(model, dialog) else {
                return;
            };
            if d.saving != Some(save) {
                return;
            }
            match answer {
                PluginOutput::Valid(Ok(())) if bookmark.name.is_empty() => {
                    let op = PluginOp::Summaries(vec![(*bookmark).clone()]);
                    out.push(Command::request_from_shell(op).then_send(move |out| {
                        shell(Reply::Bookmarks(BookmarksReply::Named {
                            dialog,
                            save,
                            open,
                            bookmark,
                            out,
                        }))
                    }));
                }
                PluginOutput::Valid(Ok(())) => saved(model, dialog, *bookmark, open, out),
                PluginOutput::Valid(Err(err)) => {
                    d.saving = None;
                    d.error = Some(err);
                }
                _ => d.saving = None,
            }
        }
        BookmarksReply::Named {
            dialog,
            save,
            open,
            mut bookmark,
            out: answer,
        } => {
            let Some(d) = editor(model, dialog) else {
                return;
            };
            if d.saving != Some(save) {
                return;
            }
            if let PluginOutput::Summaries(names) = answer {
                bookmark.name = names.into_iter().next().unwrap_or_default();
            }
            saved(model, dialog, *bookmark, open, out);
        }
    }
}

/// The bookmark passed every check: store it, close the editor, and open
/// it for "Save & Open".
fn saved(model: &mut Model, dialog: DialogId, bookmark: Bookmark, open: bool, out: &mut Out) {
    let Some(d) = model.close_dialog(dialog) else {
        return;
    };
    let DialogKind::BookmarkEdit(d) = d.kind else {
        return;
    };
    let id = bookmark.id.clone();
    Arc::make_mut(&mut model.bookmarks).upsert(bookmark);
    model.save_bookmarks(out, "bookmarks");
    refresh_summaries(model, out);
    if open {
        ops::open_bookmark(model, id, d.connect_to.unwrap_or(Side::Left), out);
    }
}

pub fn delete_confirmed(model: &mut Model, id: String, out: &mut Out) {
    if Arc::make_mut(&mut model.bookmarks).remove(&id) {
        model.save_bookmarks(out, "bookmarks");
        refresh_summaries(model, out);
    }
}

/// Asks the plugins for fresh summaries of every bookmark.
pub fn refresh_summaries(model: &Model, out: &mut Out) {
    let bookmarks = model.bookmarks.all().to_vec();
    let ids: Vec<String> = bookmarks.iter().map(|b| b.id.clone()).collect();
    out.push(
        Command::request_from_shell(PluginOp::Summaries(bookmarks)).then_send(move |answer| {
            match answer {
                PluginOutput::Summaries(summaries) => shell(Reply::Summaries { ids, summaries }),
                _ => shell(Reply::Nothing),
            }
        }),
    );
}

#[cfg(test)]
mod tests {
    use tl_model::{Bookmarks, Capabilities};

    use super::*;
    use crate::capabilities::{JobOp, StoreOp, Work};
    use crate::conn::ConnInfo;
    use crate::dialog::DialogInput;
    use crate::ids::LOCAL;
    use crate::{Cmd, Effect, Event, model};

    fn field(
        key: &str,
        label: &str,
        kind: FieldKind,
        required: bool,
        default: &str,
        advanced: bool,
    ) -> FieldInfo {
        FieldInfo {
            key: key.into(),
            label: label.into(),
            kind,
            placeholder: String::new(),
            help: String::new(),
            required,
            default: default.into(),
            advanced,
        }
    }

    fn plugins() -> Vec<PluginInfo> {
        vec![
            PluginInfo {
                id: "local".into(),
                name: "This Mac".into(),
                description: "A folder on this Mac".into(),
                fields: vec![field(
                    "path",
                    "Folder",
                    FieldKind::LocalPath,
                    true,
                    "",
                    false,
                )],
            },
            PluginInfo {
                id: "ssh".into(),
                name: "SSH".into(),
                description: "Shell commands over SSH".into(),
                fields: vec![
                    field("host", "Host", FieldKind::Text, true, "", false),
                    field("port", "Port", FieldKind::Number, false, "22", false),
                    field("password", "Password", FieldKind::Secret, false, "", false),
                    field(
                        "compress",
                        "Compress",
                        FieldKind::Toggle,
                        false,
                        "true",
                        true,
                    ),
                    field("sudo", "Use sudo", FieldKind::Toggle, false, "", true),
                ],
            },
        ]
    }

    fn model() -> Model {
        let mut model = Model {
            plugins: Arc::new(plugins()),
            ..Model::default()
        };
        model.conns.insert(ConnInfo {
            id: LOCAL,
            plugin: "local".into(),
            label: "local".into(),
            location_key: "local".into(),
            caps: Capabilities::default(),
            home: VPath::new("/Users/me"),
            bookmark: None,
            host: None,
            server_info: None,
            local_home: Some("/Users/me".into()),
        });
        let mut web = Bookmark::with_id("b-web", "web", "ssh");
        web.set("host", "web-1");
        model.bookmarks = Arc::new(Bookmarks {
            bookmarks: vec![
                web,
                Bookmark::with_id("b-db", "db", "ssh"),
                Bookmark::with_id("b-x", "x", "ftp"),
            ],
            places: Vec::new(),
        });
        model
    }

    fn run(model: &mut Model, event: Event) -> Cmd {
        let mut out = Out::default();
        model::update(event, model, &mut out);
        Command::all(out.cmds)
    }

    fn send(model: &mut Model, input: BookmarksInput) -> Vec<Effect> {
        run(model, Event::Bookmarks(input)).effects().collect()
    }

    fn editor_of(model: &Model, id: DialogId) -> &EditDialog {
        match &model.dialog(id).unwrap().kind {
            DialogKind::BookmarkEdit(d) => d,
            _ => panic!("not the editor"),
        }
    }

    fn new_editor(model: &mut Model, side: Option<Side>) -> DialogId {
        edit(model, None, side, &mut Out::default());
        model.dialogs.last().unwrap().id
    }

    /// Saves, answering the plugin's questions with `valid` and `summary`;
    /// returns the effects that follow.
    fn save_answered(
        model: &mut Model,
        dialog: DialogId,
        open: bool,
        valid: Result<(), String>,
        summary: &str,
    ) -> Vec<Effect> {
        let mut cmd = run(
            model,
            Event::Bookmarks(BookmarksInput::Save { dialog, open }),
        );
        let mut rest = Vec::new();
        for effect in cmd.effects().collect::<Vec<_>>() {
            match effect {
                Effect::Plugin(mut req) => {
                    assert!(matches!(req.operation, PluginOp::Validate { .. }));
                    req.resolve(PluginOutput::Valid(valid.clone())).unwrap();
                }
                other => rest.push(other),
            }
        }
        for event in cmd.events().collect::<Vec<_>>() {
            let mut next = run(model, event);
            for effect in next.effects().collect::<Vec<_>>() {
                match effect {
                    Effect::Plugin(mut req) if matches!(&req.operation, PluginOp::Summaries(b) if b.len() == 1) =>
                    {
                        req.resolve(PluginOutput::Summaries(vec![summary.into()]))
                            .unwrap();
                        for event in next.events().collect::<Vec<_>>() {
                            rest.extend(run(model, event).effects());
                        }
                    }
                    other => rest.push(other),
                }
            }
        }
        rest
    }

    #[test]
    fn a_new_bookmark_starts_as_ssh_with_the_defaults() {
        let mut model = model();
        let id = new_editor(&mut model, Some(Side::Right));
        let d = editor_of(&model, id);
        assert!(d.is_new);
        assert_eq!(d.title(), "New Bookmark");
        assert_eq!(d.plugin, "ssh");
        assert_eq!(d.autofocus.as_deref(), Some("host"));
        assert_eq!(d.value("port"), "22");
        assert!(d.toggle("compress"));
        assert!(!d.toggle("sudo"));
        let plugins = plugins();
        let keys: Vec<&str> = d.fields(&plugins).iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, ["host", "port", "password"]);
        assert!(d.has_advanced(&plugins));
        assert_eq!(d.description(&plugins), "Shell commands over SSH");

        send(&mut model, BookmarksInput::ToggleAdvanced { dialog: id });
        let d = editor_of(&model, id);
        assert_eq!(d.advanced_label(), "Hide advanced options");
        assert_eq!(d.fields(&plugins).len(), 5);
    }

    #[test]
    fn switching_the_type_keeps_what_was_typed() {
        let mut model = model();
        let id = new_editor(&mut model, None);
        send(
            &mut model,
            BookmarksInput::Field {
                dialog: id,
                key: "host".into(),
                value: "web-2".into(),
            },
        );
        send(
            &mut model,
            BookmarksInput::Plugin {
                dialog: id,
                plugin: "local".into(),
            },
        );
        assert!(editor_of(&model, id).picks_directories());
        send(
            &mut model,
            BookmarksInput::PathChosen {
                dialog: id,
                key: "path".into(),
                path: VPath::new("/Users/me/Projects"),
            },
        );
        assert_eq!(editor_of(&model, id).value("path"), "~/Projects");
        send(
            &mut model,
            BookmarksInput::Plugin {
                dialog: id,
                plugin: "ssh".into(),
            },
        );
        assert_eq!(editor_of(&model, id).value("host"), "web-2");
    }

    #[test]
    fn an_existing_bookmark_keeps_its_type_and_values() {
        let mut model = model();
        let web = model.bookmarks.get("b-web").cloned();
        edit(&mut model, web, None, &mut Out::default());
        let id = model.dialogs.last().unwrap().id;
        send(
            &mut model,
            BookmarksInput::Plugin {
                dialog: id,
                plugin: "local".into(),
            },
        );
        let d = editor_of(&model, id);
        assert!(!d.is_new);
        assert_eq!(d.title(), "Edit Bookmark");
        assert_eq!(d.plugin, "ssh");
        assert_eq!(d.value("host"), "web-1");
        assert_eq!(d.value("port"), "22");
        // A template that is not stored yet (Bookmark Connection) is new.
        edit(
            &mut model,
            Some(Bookmark::with_id("fresh", "x", "local")),
            None,
            &mut Out::default(),
        );
        let id = model.dialogs.last().unwrap().id;
        assert!(editor_of(&model, id).is_new);
        assert_eq!(editor_of(&model, id).autofocus, None);
    }

    #[test]
    fn the_config_leaves_out_empty_values_and_false() {
        let mut model = model();
        let id = new_editor(&mut model, None);
        send(
            &mut model,
            BookmarksInput::Field {
                dialog: id,
                key: "host".into(),
                value: "  web-3 ".into(),
            },
        );
        send(
            &mut model,
            BookmarksInput::Toggle {
                dialog: id,
                key: "compress".into(),
                on: false,
            },
        );
        send(
            &mut model,
            BookmarksInput::Toggle {
                dialog: id,
                key: "sudo".into(),
                on: true,
            },
        );
        let config = editor_of(&model, id).config(&plugins());
        let expected: BookmarkConfig = [("host", "web-3"), ("port", "22"), ("sudo", "true")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(config, expected);
    }

    #[test]
    fn the_shared_checks_come_first() {
        let mut model = model();
        let id = new_editor(&mut model, None);
        let effects = send(
            &mut model,
            BookmarksInput::Save {
                dialog: id,
                open: false,
            },
        );
        assert!(effects.iter().all(|e| !matches!(e, Effect::Plugin(_))));
        assert_eq!(
            editor_of(&model, id).error.as_deref(),
            Some("Host is required")
        );
        send(
            &mut model,
            BookmarksInput::Field {
                dialog: id,
                key: "host".into(),
                value: "h".into(),
            },
        );
        send(
            &mut model,
            BookmarksInput::Field {
                dialog: id,
                key: "port".into(),
                value: "x".into(),
            },
        );
        send(
            &mut model,
            BookmarksInput::Save {
                dialog: id,
                open: false,
            },
        );
        assert_eq!(
            editor_of(&model, id).error.as_deref(),
            Some("Port must be a number")
        );
    }

    #[test]
    fn the_plugins_own_check_keeps_the_dialog_open() {
        let mut model = model();
        let id = new_editor(&mut model, None);
        send(
            &mut model,
            BookmarksInput::Field {
                dialog: id,
                key: "host".into(),
                value: "bad host".into(),
            },
        );
        save_answered(
            &mut model,
            id,
            false,
            Err("The host cannot contain spaces".into()),
            "",
        );
        let d = editor_of(&model, id);
        assert_eq!(d.error.as_deref(), Some("The host cannot contain spaces"));
        assert_eq!(d.saving, None);
        assert_eq!(model.bookmarks.all().len(), 3);
    }

    #[test]
    fn a_nameless_bookmark_is_named_by_its_summary_and_opened() {
        let mut model = model();
        let id = new_editor(&mut model, Some(Side::Right));
        let bookmark_id = editor_of(&model, id).id.clone();
        send(
            &mut model,
            BookmarksInput::Field {
                dialog: id,
                key: "host".into(),
                value: "web-9".into(),
            },
        );
        let effects = save_answered(&mut model, id, true, Ok(()), "me@web-9");
        assert!(model.dialog(id).is_none());
        let saved = model.bookmarks.get(&bookmark_id).expect("saved");
        assert_eq!(saved.name, "me@web-9");
        assert_eq!(saved.config.get("host").map(String::as_str), Some("web-9"));
        assert!(effects.iter().any(
            |e| matches!(e, Effect::Store(r) if matches!(r.operation, StoreOp::SaveBookmarks(_)))
        ));
        assert!(effects.iter().any(|e| matches!(e, Effect::Plugin(r) if matches!(&r.operation, PluginOp::Summaries(b) if b.len() == 4))));
        let connect = effects.iter().find_map(|e| match e {
            Effect::Job(r) => match &r.operation {
                JobOp::Start {
                    work: Work::Connect { bookmark, .. },
                    ..
                } => Some(bookmark.id.clone()),
                _ => None,
            },
            _ => None,
        });
        assert_eq!(connect, Some(bookmark_id));
        assert_eq!(model.active, Side::Right);
    }

    #[test]
    fn an_edited_bookmark_is_replaced_in_place() {
        let mut model = model();
        let web = model.bookmarks.get("b-web").cloned();
        edit(&mut model, web, None, &mut Out::default());
        let id = model.dialogs.last().unwrap().id;
        send(
            &mut model,
            BookmarksInput::Name {
                dialog: id,
                text: " web prod ".into(),
            },
        );
        let effects = save_answered(&mut model, id, false, Ok(()), "unused");
        assert!(model.dialog(id).is_none());
        let names: Vec<&str> = model
            .bookmarks
            .all()
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(names, ["web prod", "db", "x"]);
        assert!(
            effects.iter().all(|e| !matches!(e, Effect::Job(_))),
            "Save alone does not connect"
        );
    }

    #[test]
    fn enter_saves_and_opens() {
        let mut model = model();
        let id = new_editor(&mut model, None);
        let mut cmd = run(&mut model, Event::Dialog(id, DialogInput::Submit));
        // The shared checks failed: nothing asked of the plugin.
        assert!(cmd.effects().all(|e| !matches!(e, Effect::Plugin(_))));
        assert!(editor_of(&model, id).error.is_some());
    }

    #[test]
    fn the_manager_reorders_edits_deletes_and_connects() {
        let mut model = model();
        manage(&mut model, Side::Right, &mut Out::default());
        let manager = model.dialogs.last().unwrap().id;

        let effects = send(
            &mut model,
            BookmarksInput::Move {
                id: "b-db".into(),
                delta: -1,
            },
        );
        let ids: Vec<&str> = model
            .bookmarks
            .all()
            .iter()
            .map(|b| b.id.as_str())
            .collect();
        assert_eq!(ids, ["b-db", "b-web", "b-x"]);
        assert!(effects.iter().any(|e| matches!(e, Effect::Store(_))));
        assert!(
            send(
                &mut model,
                BookmarksInput::Move {
                    id: "b-db".into(),
                    delta: -1
                }
            )
            .is_empty()
        );

        send(
            &mut model,
            BookmarksInput::Edit {
                manager,
                id: "b-web".into(),
            },
        );
        let editor = model.dialogs.last().unwrap().id;
        assert_eq!(editor_of(&model, editor).connect_to, Some(Side::Right));
        model::update(
            Event::Dialog(editor, DialogInput::Cancel),
            &mut model,
            &mut Out::default(),
        );

        send(&mut model, BookmarksInput::Delete { id: "b-web".into() });
        let confirm = model.dialogs.last().unwrap().clone();
        let DialogKind::Confirm(c) = &confirm.kind else {
            panic!("a confirmation")
        };
        assert_eq!(c.title, "Delete bookmark “web”?");
        assert_eq!(
            c.message,
            "Open connections stay open until you disconnect."
        );
        assert_eq!((c.ok.as_str(), c.danger), ("Delete", true));
        let mut cmd = run(&mut model, Event::Dialog(confirm.id, DialogInput::Submit));
        assert!(model.bookmarks.get("b-web").is_none());
        assert!(cmd.effects().any(|e| matches!(e, Effect::Store(_))));

        let effects = send(
            &mut model,
            BookmarksInput::Open {
                manager,
                id: "b-db".into(),
            },
        );
        assert!(model.dialog(manager).is_none());
        assert!(effects.iter().any(|e| matches!(e, Effect::Job(_))));
        assert_eq!(model.active, Side::Right);
    }

    #[test]
    fn the_manager_describes_its_rows() {
        let model = model();
        let summaries: HashMap<String, String> =
            [("b-web".to_string(), "me@web-1".to_string())].into();
        let b = |id| model.bookmarks.get(id).unwrap();
        assert_eq!(summary(b("b-web"), &model.plugins, &summaries), "me@web-1");
        assert_eq!(
            summary(b("b-x"), &model.plugins, &summaries),
            "unknown type “ftp”"
        );
        assert_eq!((open_label(true), open_label(false)), ("Open", "Connect"));
    }

    #[test]
    fn summaries_are_asked_for_every_bookmark() {
        let mut model = model();
        let mut out = Out::default();
        refresh_summaries(&model, &mut out);
        let mut cmd: Cmd = Command::all(out.cmds);
        let Some(Effect::Plugin(mut req)) = cmd.effects().next() else {
            panic!("a plugin request")
        };
        let PluginOp::Summaries(all) = &req.operation else {
            panic!()
        };
        assert_eq!(all.len(), 3);
        req.resolve(PluginOutput::Summaries(vec![
            "a".into(),
            "b".into(),
            "c".into(),
        ]))
        .unwrap();
        for event in cmd.events().collect::<Vec<_>>() {
            model::update(event, &mut model, &mut Out::default());
        }
        assert_eq!(model.summaries.get("b-db").map(String::as_str), Some("b"));
    }
}
