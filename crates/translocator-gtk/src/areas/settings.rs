//! The preferences window: an `adw::PreferencesDialog` opened when
//! `Snapshot.settings_requests` moves (the open-settings command), with the
//! pages of the macOS and GPUI settings windows:
//!
//! - General: the switches of `GeneralSettings`, the sort and date pickers,
//!   text size and row height held to `settings_ranges`, the terminal, and
//!   Restore Defaults;
//! - Keys: every command of the key table by group, its keys as chips (the
//!   Linux labels: `cmd` is Ctrl), ✕ to take one away, + to record one
//!   (the next key pressed in the dialog; Escape cancels), reset per
//!   command, and a note when the keys are another command's too
//!   (`tl_app::keys::conflicts_on` for Linux);
//! - Appearance: System / Light / Dark (the shell's `Theme` makes
//!   `AdwStyleManager` follow), the light and dark schemes, and a preview
//!   of a few rows of a file list in each.
//!
//! There is no OK button: every control sends its `SettingsEdit` at once,
//! and what is shown is read back from the snapshot ([`update`], which the
//! shell calls whenever `Snapshot.settings` changes). Recording a key and
//! the note about a clash are this window's own state. The search is the
//! dialog's own (row titles and subtitles).
//!
//! Swift: `macos/.../Areas/SettingsWindow.swift`; GPUI:
//! `crates/translocator/src/settings_window.rs`.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, glib};
use tl_app::keys::{self as table};
use tl_ffi::input::Input;
use tl_ffi::keys::{KeyBinding, KeyCommand};
use tl_ffi::settings::{self as ffi, DateFormatChoice, SettingsEdit, SettingsSnapshot, SortKeyChoice};
use tl_ffi::snapshot::Snapshot;
use tl_ffi::themes::{self, ThemeChoice};
use tl_ffi::types::AppearanceMode;

use crate::keys::{KeyMap, PLATFORM};
use crate::shell;

thread_local! {
    /// The open preferences, if any: there is only ever one.
    static OPEN: RefCell<Option<Rc<Prefs>>> = const { RefCell::new(None) };
}

/// Opens the preferences (or leaves the open one where it is).
pub fn present(snapshot: &Snapshot, parent: &impl IsA<gtk::Widget>) {
    if OPEN.with(|o| o.borrow().is_some()) {
        return;
    }
    install_css();
    let commands = shell::with(|s| s.keys.borrow().commands.clone());
    let prefs = Prefs::new(&snapshot.settings, commands);
    let weak = Rc::downgrade(&prefs);
    prefs.dialog.connect_closed(move |_| {
        if let Some(prefs) = weak.upgrade() {
            prefs.keys.stop_recording();
            if let Some(id) = prefs.dark_handler.take() {
                adw::StyleManager::default().disconnect(id);
            }
        }
        OPEN.with(|o| o.replace(None));
        // The keyboard back to the list, not to the first button.
        shell::restore_focus_soon();
    });
    prefs.dialog.present(Some(parent));
    OPEN.with(|o| o.replace(Some(prefs)));
}

/// The settings (or the keys) changed: the open preferences show them.
pub fn update(settings: &SettingsSnapshot, keys: &KeyMap) {
    let Some(prefs) = OPEN.with(|o| o.borrow().clone()) else { return };
    prefs.update(settings, Some(&keys.commands));
}

fn edit(edit: SettingsEdit) {
    shell::send(Input::Settings { edit });
}

struct Prefs {
    dialog: adw::PreferencesDialog,
    general: General,
    keys: Rc<KeysPage>,
    appearance: AppearancePage,
    /// The settings shown.
    settings: RefCell<SettingsSnapshot>,
    dark_handler: Cell<Option<glib::SignalHandlerId>>,
}

impl Prefs {
    fn new(settings: &SettingsSnapshot, commands: Vec<KeyCommand>) -> Rc<Prefs> {
        let updating = Rc::new(Cell::new(false));
        let dialog = adw::PreferencesDialog::new();
        dialog.set_title("Preferences");
        dialog.set_search_enabled(true);
        dialog.set_content_width(680);
        dialog.set_content_height(720);

        let general = General::new(settings, &commands, &updating);
        let keys = KeysPage::new(commands, &dialog);
        let appearance = AppearancePage::new(settings, &updating);
        dialog.add(&general.page);
        dialog.add(&keys.page);
        dialog.add(&appearance.page);

        let prefs = Rc::new(Prefs {
            dialog,
            general,
            keys,
            appearance,
            settings: RefCell::new(settings.clone()),
            dark_handler: Cell::new(None),
        });
        // The system switching between light and dark moves "in use".
        let weak = Rc::downgrade(&prefs);
        let id = adw::StyleManager::default().connect_dark_notify(move |_| {
            if let Some(prefs) = weak.upgrade() {
                let settings = prefs.settings.borrow().clone();
                prefs.appearance.update(&settings, &updating);
            }
        });
        prefs.dark_handler.set(Some(id));
        prefs.update(settings, None);
        prefs
    }

    fn update(&self, settings: &SettingsSnapshot, commands: Option<&[KeyCommand]>) {
        self.settings.replace(settings.clone());
        let updating = self.general.updating.clone();
        updating.set(true);
        self.general.update(settings, commands);
        self.appearance.update(settings, &updating);
        updating.set(false);
        if let Some(commands) = commands {
            self.keys.update(commands.to_vec());
        }
    }
}

// ----- General -----------------------------------------------------------------

struct General {
    page: adw::PreferencesPage,
    updating: Rc<Cell<bool>>,
    restore: adw::SwitchRow,
    hidden: adw::SwitchRow,
    folders_first: adw::SwitchRow,
    sort: adw::ComboRow,
    sorts: Vec<SortKeyChoice>,
    descending: adw::SwitchRow,
    icons: adw::SwitchRow,
    dates: adw::ComboRow,
    date_choices: Vec<DateFormatChoice>,
    font: adw::SpinRow,
    row_height: adw::SpinRow,
    trash: adw::SwitchRow,
    terminal: adw::EntryRow,
    /// The terminal as the core last said, to tell an edit in progress
    /// from text that is out of date.
    terminal_shown: RefCell<String>,
}

fn switch_row(title: &str, subtitle: &str, updating: &Rc<Cell<bool>>, make: fn(bool) -> SettingsEdit) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).use_markup(false).build();
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    let guard = updating.clone();
    row.connect_active_notify(move |row| {
        if !guard.get() {
            edit(make(row.is_active()));
        }
    });
    row
}

fn combo_row(title: &str, subtitle: &str, labels: &[&str]) -> adw::ComboRow {
    let row = adw::ComboRow::builder()
        .title(title)
        .use_markup(false)
        .model(&gtk::StringList::new(labels))
        .build();
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row
}

fn spin_row(title: &str, subtitle: &str, min: f32, max: f32, updating: &Rc<Cell<bool>>, make: fn(f32) -> SettingsEdit) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(f64::from(min), f64::from(max), 1.0);
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_digits(0);
    row.set_numeric(true);
    let guard = updating.clone();
    row.connect_value_notify(move |row| {
        if !guard.get() {
            edit(make(row.value() as f32));
        }
    });
    row
}

impl General {
    fn new(settings: &SettingsSnapshot, commands: &[KeyCommand], updating: &Rc<Cell<bool>>) -> General {
        let page = adw::PreferencesPage::builder()
            .title("General")
            .icon_name("preferences-system-symbolic")
            .name("general")
            .build();

        let startup = adw::PreferencesGroup::builder().title("Startup").build();
        let restore = switch_row(
            "Reopen the last folders",
            "Start where the panes were when you quit, instead of in your home folder.",
            updating,
            |on| SettingsEdit::RestoreFolders { on },
        );
        startup.add(&restore);
        page.add(&startup);

        let list = adw::PreferencesGroup::builder().title("File list").build();
        let hidden_key = commands
            .iter()
            .find(|c| c.id == "toggle-hidden")
            .and_then(|c| c.bindings.first())
            .and_then(crate::keys::label)
            .map(|k| format!(" {k} toggles it for this session too."))
            .unwrap_or_default();
        let hidden = switch_row(
            "Show hidden files",
            &format!("Names that begin with a dot.{hidden_key}"),
            updating,
            |on| SettingsEdit::ShowHidden { on },
        );
        let folders_first = switch_row(
            "Folders first",
            "Keep folders above files whatever the list is sorted by.",
            updating,
            |on| SettingsEdit::FoldersFirst { on },
        );
        let sorts = ffi::sort_key_choices();
        let sort = combo_row(
            "Sort new tabs by",
            "Clicking a column header still sorts one tab by hand.",
            &sorts.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(),
        );
        let descending = switch_row("Largest or newest first", "", updating, |on| SettingsEdit::SortDescending { on });
        let icons = switch_row(
            "File icons",
            "Without them a row is all name, which fits more of a long one.",
            updating,
            |on| SettingsEdit::ShowIcons { on },
        );
        let date_choices = ffi::date_format_choices();
        let dates = combo_row(
            "Dates",
            "",
            &date_choices.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(),
        );
        let ranges = ffi::settings_ranges();
        let font = spin_row(
            "Text size",
            "Pixels. The size of the file lists' text.",
            ranges.font_size_min,
            ranges.font_size_max,
            updating,
            |size| SettingsEdit::FontSize { size },
        );
        let row_height = spin_row(
            "Row height",
            "Pixels. Lower fits more files on the screen.",
            ranges.row_height_min,
            ranges.row_height_max,
            updating,
            |height| SettingsEdit::RowHeight { height },
        );
        for row in [
            hidden.upcast_ref::<gtk::Widget>(),
            folders_first.upcast_ref(),
            sort.upcast_ref(),
            descending.upcast_ref(),
            icons.upcast_ref(),
            dates.upcast_ref(),
            font.upcast_ref(),
            row_height.upcast_ref(),
        ] {
            list.add(row);
        }
        page.add(&list);

        let safety = adw::PreferencesGroup::builder().title("Safety").build();
        let trash = switch_row(
            "Ask before moving to Trash",
            "Deleting permanently always asks, whatever this says.",
            updating,
            |on| SettingsEdit::ConfirmTrash { on },
        );
        safety.add(&trash);
        page.add(&safety);

        let tools = adw::PreferencesGroup::builder()
            .title("Tools")
            .description(tl_ffi::settings::terminal_help())
            .build();
        let terminal = adw::EntryRow::builder().title("Terminal").use_markup(false).build();
        terminal.set_text(&settings.general.terminal);
        terminal.set_show_apply_button(true);
        terminal.connect_apply(|row| {
            edit(SettingsEdit::Terminal {
                command: row.text().to_string(),
            })
        });
        tools.add(&terminal);
        page.add(&tools);

        let reset_group = adw::PreferencesGroup::new();
        let reset = gtk::Button::builder()
            .label("Restore Defaults")
            .halign(gtk::Align::Center)
            .tooltip_text("Every setting on this page back to its default")
            .build();
        reset.add_css_class("pill");
        reset.connect_clicked(|_| edit(SettingsEdit::ResetGeneral));
        reset_group.add(&reset);
        page.add(&reset_group);

        let general = General {
            page,
            updating: updating.clone(),
            restore,
            hidden,
            folders_first,
            sort,
            sorts,
            descending,
            icons,
            dates,
            date_choices,
            font,
            row_height,
            trash,
            terminal,
            terminal_shown: RefCell::new(settings.general.terminal.clone()),
        };
        general.connect_pickers();
        general
    }

    fn connect_pickers(&self) {
        let (guard, keys) = (self.updating.clone(), self.sorts.iter().map(|c| c.key).collect::<Vec<_>>());
        self.sort.connect_selected_notify(move |row| {
            if let (false, Some(key)) = (guard.get(), keys.get(row.selected() as usize)) {
                edit(SettingsEdit::Sort { key: *key });
            }
        });
        let (guard, formats) = (
            self.updating.clone(),
            self.date_choices.iter().map(|c| c.format).collect::<Vec<_>>(),
        );
        self.dates.connect_selected_notify(move |row| {
            if let (false, Some(format)) = (guard.get(), formats.get(row.selected() as usize)) {
                edit(SettingsEdit::DateFormat { format: *format });
            }
        });
    }

    /// With `updating` set by the caller.
    fn update(&self, settings: &SettingsSnapshot, commands: Option<&[KeyCommand]>) {
        let g = &settings.general;
        let set = |row: &adw::SwitchRow, on: bool| {
            if row.is_active() != on {
                row.set_active(on);
            }
        };
        set(&self.restore, g.restore_folders);
        set(&self.hidden, g.show_hidden);
        set(&self.folders_first, g.folders_first);
        set(&self.descending, g.sort_descending);
        set(&self.icons, g.show_icons);
        set(&self.trash, g.confirm_trash);
        if let Some(ix) = self.sorts.iter().position(|c| c.key == g.sort)
            && self.sort.selected() != ix as u32
        {
            self.sort.set_selected(ix as u32);
        }
        if let Some(ix) = self.date_choices.iter().position(|c| c.format == g.date_format)
            && self.dates.selected() != ix as u32
        {
            self.dates.set_selected(ix as u32);
        }
        for (row, value) in [(&self.font, g.font_size), (&self.row_height, g.row_height)] {
            if (row.value() - f64::from(value)).abs() > 0.01 {
                row.set_value(f64::from(value));
            }
        }
        // The terminal is replaced only while it is not being edited.
        let shown = self.terminal_shown.borrow().clone();
        if shown != g.terminal && self.terminal.text() == shown {
            // Text set from here is not an edit: no apply button for it.
            self.terminal.set_show_apply_button(false);
            self.terminal.set_text(&g.terminal);
            self.terminal.set_show_apply_button(true);
        }
        self.terminal_shown.replace(g.terminal.clone());
        if let Some(commands) = commands {
            let key = commands
                .iter()
                .find(|c| c.id == "toggle-hidden")
                .and_then(|c| c.bindings.first())
                .and_then(crate::keys::label)
                .map(|k| format!(" {k} toggles it for this session too."))
                .unwrap_or_default();
            self.hidden.set_subtitle(&format!("Names that begin with a dot.{key}"));
        }
    }
}

// ----- Keys ----------------------------------------------------------------------

/// One command's row.
struct KeyRow {
    id: String,
    row: adw::ActionRow,
    chips: gtk::Box,
    add: gtk::Button,
    reset: gtk::Button,
}

/// Recording in progress: the command waiting for a key, and the
/// application's accelerators, taken away meanwhile so that recording
/// Ctrl+Q does not quit.
struct Recording {
    command: String,
    accels: Vec<(String, Vec<glib::GString>)>,
}

struct KeysPage {
    /// Itself, for the closures of the widgets it makes.
    me: Weak<KeysPage>,
    page: adw::PreferencesPage,
    rows: Vec<KeyRow>,
    restore: gtk::Button,
    commands: RefCell<Vec<KeyCommand>>,
    recording: RefCell<Option<Recording>>,
    /// A note under one command's row: a clash, or a key that cannot be used.
    note: RefCell<Option<(String, String)>>,
}

impl KeysPage {
    fn new(commands: Vec<KeyCommand>, dialog: &adw::PreferencesDialog) -> Rc<KeysPage> {
        let page = adw::PreferencesPage::builder()
            .title("Keys")
            .icon_name("input-keyboard-symbolic")
            .name("keys")
            .build();
        let intro = adw::PreferencesGroup::builder()
            .description("Press + to add a key combination, or ✕ on one to take it away. Escape cancels recording. Search above finds a command.")
            .build();
        let restore = gtk::Button::builder()
            .label("Restore Defaults")
            .valign(gtk::Align::Center)
            .tooltip_text("Every command back to its default keys")
            .build();
        restore.add_css_class("flat");
        intro.set_header_suffix(Some(&restore));
        page.add(&intro);

        let mut groups: Vec<(String, adw::PreferencesGroup)> = Vec::new();
        let mut rows = Vec::new();
        for command in &commands {
            let group = match groups.iter().find(|(name, _)| *name == command.group) {
                Some((_, group)) => group.clone(),
                None => {
                    let group = adw::PreferencesGroup::builder().title(&command.group).build();
                    page.add(&group);
                    groups.push((command.group.clone(), group.clone()));
                    group
                }
            };
            let row = adw::ActionRow::builder().title(&command.label).use_markup(false).build();
            let chips = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            chips.set_valign(gtk::Align::Center);
            let add = icon_button("list-add-symbolic", "Add a key combination");
            add.update_property(&[gtk::accessible::Property::Label(&format!(
                "Add a key combination for {}",
                command.label
            ))]);
            let reset = icon_button("edit-undo-symbolic", "Back to the default");
            reset.update_property(&[gtk::accessible::Property::Label(&format!(
                "Reset the keys of {}",
                command.label
            ))]);
            row.add_suffix(&chips);
            row.add_suffix(&add);
            row.add_suffix(&reset);
            group.add(&row);
            rows.push(KeyRow {
                id: command.id.clone(),
                row,
                chips,
                add,
                reset,
            });
        }

        let keys = Rc::new_cyclic(|me| KeysPage {
            me: me.clone(),
            page,
            rows,
            restore,
            commands: RefCell::new(commands),
            recording: RefCell::new(None),
            note: RefCell::new(None),
        });
        for row in &keys.rows {
            let (weak, id) = (Rc::downgrade(&keys), row.id.clone());
            row.add.connect_clicked(move |_| {
                if let Some(keys) = weak.upgrade() {
                    keys.start_recording(&id);
                }
            });
            let (weak, id) = (Rc::downgrade(&keys), row.id.clone());
            row.reset.connect_clicked(move |_| {
                if let Some(keys) = weak.upgrade() {
                    keys.note.replace(None);
                    edit(SettingsEdit::Keys {
                        command: id.clone(),
                        keys: None,
                    });
                }
            });
        }
        let weak = Rc::downgrade(&keys);
        keys.restore.connect_clicked(move |_| {
            let Some(keys) = weak.upgrade() else { return };
            keys.note.replace(None);
            for command in keys.commands.borrow().iter().filter(|c| c.changed) {
                edit(SettingsEdit::Keys {
                    command: command.id.clone(),
                    keys: None,
                });
            }
        });
        keys.capture_keys(dialog);
        keys.refill_all();
        keys
    }

    fn update(&self, commands: Vec<KeyCommand>) {
        if *self.commands.borrow() == commands {
            return;
        }
        self.commands.replace(commands);
        self.refill_all();
    }

    fn refill_all(&self) {
        let any_changed = self.commands.borrow().iter().any(|c| c.changed);
        self.restore.set_sensitive(any_changed);
        for row in &self.rows {
            self.refill(row);
        }
    }

    fn row(&self, id: &str) -> Option<&KeyRow> {
        self.rows.iter().find(|r| r.id == id)
    }

    /// Shows a command's keys (or the recording prompt) and its note.
    /// Returns the Cancel button while recording.
    fn refill(&self, row: &KeyRow) -> Option<gtk::Button> {
        while let Some(child) = row.chips.first_child() {
            row.chips.remove(&child);
        }
        let commands = self.commands.borrow();
        let command = commands.iter().find(|c| c.id == row.id)?;
        let recording = self.recording.borrow().as_ref().is_some_and(|r| r.command == row.id);
        row.add.set_visible(!recording);
        row.reset.set_visible(!recording);
        row.reset.set_sensitive(command.changed);
        let note = self.note.borrow().as_ref().filter(|(id, _)| *id == row.id).map(|(_, text)| text.clone());
        row.row.set_subtitle(note.as_deref().unwrap_or(""));
        if note.is_some() {
            row.row.add_css_class("tl-key-note");
        } else {
            row.row.remove_css_class("tl-key-note");
        }
        if recording {
            let prompt = gtk::Label::new(Some("Press a key combination…"));
            prompt.add_css_class("tl-key-recording");
            prompt.update_property(&[gtk::accessible::Property::Label(&format!(
                "Recording a key combination for {}",
                command.label
            ))]);
            let cancel = gtk::Button::with_label("Cancel");
            cancel.set_valign(gtk::Align::Center);
            let weak = self.me.clone();
            cancel.connect_clicked(move |_| {
                if let Some(keys) = weak.upgrade() {
                    keys.stop_recording();
                }
            });
            row.chips.append(&prompt);
            row.chips.append(&cancel);
            return Some(cancel);
        }
        if command.bindings.is_empty() {
            let none = gtk::Label::new(Some("no key"));
            none.add_css_class("dim-label");
            row.chips.append(&none);
        }
        for binding in &command.bindings {
            row.chips.append(&self.chip(command, binding));
        }
        None
    }

    fn chip(&self, command: &KeyCommand, binding: &KeyBinding) -> gtk::Widget {
        let text = crate::keys::label(binding).unwrap_or_else(|| binding.keys.clone());
        let chip = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        chip.add_css_class("tl-key-chip");
        chip.set_valign(gtk::Align::Center);
        let label = gtk::Label::new(Some(&text));
        label.add_css_class("tl-key-label");
        let remove = gtk::Button::from_icon_name("window-close-symbolic");
        remove.add_css_class("flat");
        remove.add_css_class("circular");
        remove.set_tooltip_text(Some("Take this key away"));
        remove.update_property(&[gtk::accessible::Property::Label(&format!(
            "Remove {text} from {}",
            command.label
        ))]);
        let (id, keys) = (command.id.clone(), binding.keys.clone());
        let weak = self.me.clone();
        remove.connect_clicked(move |_| {
            let Some(page) = weak.upgrade() else { return };
            page.note.replace(None);
            let bound: Vec<String> = page
                .commands
                .borrow()
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.bindings.iter().map(|b| b.keys.clone()).filter(|k| *k != keys).collect())
                .unwrap_or_default();
            edit(SettingsEdit::Keys {
                command: id.clone(),
                keys: Some(bound),
            });
        });
        chip.append(&label);
        chip.append(&remove);
        chip.upcast()
    }

    // ----- recording ------------------------------------------------------

    fn start_recording(&self, id: &str) {
        self.stop_recording();
        self.note.replace(None);
        // Every accelerator of the application, taken away for as long as
        // the recording lasts.
        let app = shell::with(|s| s.app.clone());
        let accels: Vec<(String, Vec<glib::GString>)> = app
            .list_action_descriptions()
            .into_iter()
            .map(|name| {
                let accels = app.accels_for_action(&name);
                (name.to_string(), accels)
            })
            .filter(|(_, accels)| !accels.is_empty())
            .collect();
        for (name, _) in &accels {
            app.set_accels_for_action(name, &[]);
        }
        self.recording.replace(Some(Recording {
            command: id.to_string(),
            accels,
        }));
        if let Some(row) = self.row(id) {
            // The keyboard stays in the dialog, where the keys are caught.
            if let Some(cancel) = self.refill(row) {
                cancel.grab_focus();
            }
        }
    }

    fn stop_recording(&self) {
        let Some(recording) = self.recording.take() else { return };
        let app = shell::with(|s| s.app.clone());
        for (name, accels) in &recording.accels {
            let refs: Vec<&str> = accels.iter().map(|a| a.as_str()).collect();
            app.set_accels_for_action(name, &refs);
        }
        if let Some(row) = self.row(&recording.command) {
            self.refill(row);
            row.add.grab_focus();
        }
    }

    /// Catches the keys pressed in the dialog while recording, before
    /// anything in it (Escape closing it, say) acts on them.
    fn capture_keys(self: &Rc<KeysPage>, dialog: &adw::PreferencesDialog) {
        let controller = gtk::EventControllerKey::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        controller.connect_key_pressed(move |controller, keyval, keycode, state| {
            let Some(page) = weak.upgrade() else { return glib::Propagation::Proceed };
            let Some(id) = page.recording.borrow().as_ref().map(|r| r.command.clone()) else {
                return glib::Propagation::Proceed;
            };
            if is_modifier(keyval) {
                // Held for the key that comes next.
                return glib::Propagation::Stop;
            }
            let modifiers = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SHIFT_MASK;
            if keyval == gdk::Key::Escape && !state.intersects(modifiers) {
                page.stop_recording();
                return glib::Propagation::Stop;
            }
            let Some(widget) = controller.widget() else { return glib::Propagation::Stop };
            let spelled = crate::keys::spellings(&widget.display(), keyval, keycode, state, controller.group())
                .into_iter()
                .next();
            page.stop_recording();
            match spelled {
                Some(keys) => page.recorded(&id, &keys),
                None => widget.error_bell(),
            }
            glib::Propagation::Stop
        });
        dialog.add_controller(controller);
    }

    /// A key combination arrived for `id` (spelled `ctrl-shift-g`: on
    /// Linux Ctrl is the same keys as `cmd`).
    fn recorded(&self, id: &str, keys: &str) {
        if let Some(error) = tl_ffi::keys::validate_keys(keys.to_string()) {
            self.set_note(id, Some(error));
            return;
        }
        let Some(spec) = table::command(id) else { return };
        // The keys in effect for every command, as the table holds them.
        let overrides: BTreeMap<String, Vec<String>> = self
            .commands
            .borrow()
            .iter()
            .map(|c| (c.id.clone(), c.bindings.iter().map(|b| b.keys.clone()).collect()))
            .collect();
        let clashes: Vec<&str> = table::conflicts_on(keys, spec, &overrides, PLATFORM)
            .into_iter()
            .map(|c| c.label)
            .collect();
        let note = (!clashes.is_empty())
            .then(|| format!("{} is also {}.", table::pretty(keys, PLATFORM), clashes.join(", ")));
        self.set_note(id, note);
        let mut bound = overrides.get(id).cloned().unwrap_or_default();
        if !bound.iter().any(|k| table::same_keys_on(k, keys, PLATFORM)) {
            bound.push(keys.to_string());
            edit(SettingsEdit::Keys {
                command: id.to_string(),
                keys: Some(bound),
            });
        }
    }

    fn set_note(&self, id: &str, note: Option<String>) {
        self.note.replace(note.map(|text| (id.to_string(), text)));
        if let Some(row) = self.row(id) {
            self.refill(row);
        }
    }
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("flat");
    button.set_valign(gtk::Align::Center);
    button.set_tooltip_text(Some(tooltip));
    button
}

fn is_modifier(keyval: gdk::Key) -> bool {
    use gdk::Key as K;
    matches!(
        keyval,
        K::Shift_L
            | K::Shift_R
            | K::Control_L
            | K::Control_R
            | K::Alt_L
            | K::Alt_R
            | K::Super_L
            | K::Super_R
            | K::Meta_L
            | K::Meta_R
            | K::Hyper_L
            | K::Hyper_R
            | K::ISO_Level3_Shift
            | K::ISO_Level5_Shift
            | K::Caps_Lock
            | K::Num_Lock
    )
}

// ----- Appearance --------------------------------------------------------------------

struct AppearancePage {
    page: adw::PreferencesPage,
    modes: Vec<(AppearanceMode, gtk::ToggleButton)>,
    light: adw::ComboRow,
    dark: adw::ComboRow,
    light_choices: Vec<ThemeChoice>,
    dark_choices: Vec<ThemeChoice>,
    previews: [Preview; 2],
}

impl AppearancePage {
    fn new(settings: &SettingsSnapshot, updating: &Rc<Cell<bool>>) -> AppearancePage {
        let page = adw::PreferencesPage::builder()
            .title("Appearance")
            .icon_name("applications-graphics-symbolic")
            .name("appearance")
            .build();
        let group = adw::PreferencesGroup::builder()
            .title("Appearance")
            .description("Ten colour schemes, each in a light and a dark variant. They colour the file lists and the console; the window around them keeps the desktop's look. “Default” is the desktop's own colours.")
            .build();

        let style = adw::ActionRow::builder()
            .title("Style")
            .subtitle("“System” follows the desktop's light and dark setting.")
            .use_markup(false)
            .build();
        let linked = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        linked.add_css_class("linked");
        linked.set_valign(gtk::Align::Center);
        let mut modes: Vec<(AppearanceMode, gtk::ToggleButton)> = Vec::new();
        for choice in ffi::appearance_mode_choices() {
            let button = gtk::ToggleButton::with_label(&choice.label);
            if let Some((_, first)) = modes.first() {
                button.set_group(Some(first));
            }
            let (guard, mode) = (updating.clone(), choice.mode);
            button.connect_toggled(move |button| {
                if button.is_active() && !guard.get() {
                    edit(SettingsEdit::AppearanceMode { mode });
                }
            });
            linked.append(&button);
            modes.push((choice.mode, button));
        }
        style.add_suffix(&linked);
        group.add(&style);

        let light_choices = themes::theme_choices(false);
        let dark_choices = themes::theme_choices(true);
        let light = theme_row("Light scheme", "Used while the appearance is light.", &light_choices, updating, false);
        let dark = theme_row("Dark scheme", "Used while the appearance is dark.", &dark_choices, updating, true);
        group.add(&light);
        group.add(&dark);
        page.add(&group);

        let preview_group = adw::PreferencesGroup::builder().title("Preview").build();
        let previews = [Preview::new(false), Preview::new(true)];
        let both = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        both.set_homogeneous(true);
        for preview in &previews {
            both.append(&preview.root);
        }
        preview_group.add(&both);
        page.add(&preview_group);

        let page = AppearancePage {
            page,
            modes,
            light,
            dark,
            light_choices,
            dark_choices,
            previews,
        };
        page.update(settings, updating);
        page
    }

    fn update(&self, settings: &SettingsSnapshot, updating: &Rc<Cell<bool>>) {
        let was = updating.replace(true);
        let a = &settings.appearance;
        for (mode, button) in &self.modes {
            if *mode == a.mode && !button.is_active() {
                button.set_active(true);
            }
        }
        for (row, choices, name) in [
            (&self.light, &self.light_choices, &a.light),
            (&self.dark, &self.dark_choices, &a.dark),
        ] {
            let ix = choices.iter().position(|c| c.name == *name).unwrap_or(0) as u32;
            if row.selected() != ix {
                row.set_selected(ix);
            }
        }
        updating.set(was);
        let dark_in_use = adw::StyleManager::default().is_dark();
        let scheme = |choices: &[ThemeChoice], name: &str| {
            choices
                .iter()
                .find(|c| c.name == name)
                .or(choices.first())
                .map(|c| c.scheme.clone())
                .unwrap_or_default()
        };
        self.previews[0].update(&scheme(&self.light_choices, &a.light), !dark_in_use, &settings.general);
        self.previews[1].update(&scheme(&self.dark_choices, &a.dark), dark_in_use, &settings.general);
        let css = format!(
            "{}{}",
            preview_css("light", &colours(&a.light, false), &settings.general),
            preview_css("dark", &colours(&a.dark, true), &settings.general)
        );
        PREVIEW_CSS.with(|p| {
            if let Some(provider) = p.get() {
                provider.load_from_string(&css);
            }
        });
    }
}

fn theme_row(title: &str, subtitle: &str, choices: &[ThemeChoice], updating: &Rc<Cell<bool>>, dark: bool) -> adw::ComboRow {
    let row = combo_row(
        title,
        subtitle,
        &choices.iter().map(|c| c.scheme.as_str()).collect::<Vec<_>>(),
    );
    let names: Vec<String> = choices.iter().map(|c| c.name.clone()).collect();
    let guard = updating.clone();
    row.connect_selected_notify(move |row| {
        if guard.get() {
            return;
        }
        let Some(name) = names.get(row.selected() as usize).cloned() else { return };
        edit(if dark {
            SettingsEdit::DarkTheme { name }
        } else {
            SettingsEdit::LightTheme { name }
        });
    });
    row
}

/// A few rows of a file list in a scheme, at the list's text size and row
/// height, so a change can be seen without leaving the window.
struct Preview {
    dark: bool,
    root: gtk::Box,
    heading: gtk::Label,
    icons: Vec<gtk::Image>,
}

/// The sample rows: name, size, icon, the icon's class, cursor, marked.
const SAMPLES: [(&str, &str, &str, &str, bool, bool); 5] = [
    ("..", "", "go-up-symbolic", "folder", false, false),
    ("translocator", "folder", "folder-symbolic", "folder", true, false),
    ("README.md", "2.1 kB", "text-x-generic-symbolic", "", false, false),
    ("install.sh", "870 B", "utilities-terminal-symbolic", "exec", false, true),
    ("notes.txt", "14 kB", "text-x-generic-symbolic", "", false, false),
];

impl Preview {
    fn new(dark: bool) -> Preview {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let heading = gtk::Label::new(None);
        heading.set_xalign(0.0);
        heading.add_css_class("heading");
        root.append(&heading);

        let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        frame.add_css_class("tl-preview");
        frame.add_css_class(if dark { "dark" } else { "light" });
        frame.set_overflow(gtk::Overflow::Hidden);
        frame.update_property(&[gtk::accessible::Property::Label(if dark {
            "Dark preview"
        } else {
            "Light preview"
        })]);

        let bar = |left: &str, right: &str, class: &str, left_class: &str| {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row.add_css_class(class);
            let l = gtk::Label::new(Some(left));
            l.set_xalign(0.0);
            l.set_hexpand(true);
            if !left_class.is_empty() {
                l.add_css_class(left_class);
            }
            row.append(&l);
            row.append(&gtk::Label::new(Some(right)));
            row
        };
        frame.append(&bar("Name", "Size", "tl-pv-head", ""));
        let mut icons = Vec::new();
        for (name, size, icon, icon_class, cursor, marked) in SAMPLES {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row.add_css_class("tl-pv-row");
            if cursor {
                row.add_css_class("cursor");
            }
            if marked {
                row.add_css_class("marked");
            }
            let image = gtk::Image::from_icon_name(icon);
            image.add_css_class("tl-pv-icon");
            if !icon_class.is_empty() {
                image.add_css_class(icon_class);
            }
            let label = gtk::Label::new(Some(name));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.add_css_class("tl-pv-name");
            let size = gtk::Label::new(Some(size));
            size.add_css_class("tl-pv-size");
            size.add_css_class("numeric");
            row.append(&image);
            row.append(&label);
            row.append(&size);
            frame.append(&row);
            icons.push(image);
        }
        frame.append(&bar("1 marked", "12 GB free", "tl-pv-status", "tl-pv-marked"));
        root.append(&frame);
        Preview {
            dark,
            root,
            heading,
            icons,
        }
    }

    fn update(&self, scheme: &str, in_use: bool, general: &tl_ffi::settings::GeneralSettings) {
        let side = if self.dark { "Dark" } else { "Light" };
        self.heading.set_text(&if in_use {
            format!("{side} · {scheme} · in use")
        } else {
            format!("{side} · {scheme}")
        });
        for icon in &self.icons {
            icon.set_visible(general.show_icons);
            icon.set_pixel_size(general.font_size.round() as i32 + 2);
        }
    }
}

/// The colours a preview is painted in.
struct Colours {
    bg: String,
    chrome: String,
    fg: String,
    muted: String,
    border: String,
    cursor_bg: String,
    cursor_fg: String,
    cursor_muted: String,
    marked: String,
    marked_bg: String,
    folder: String,
    exec: String,
}

/// A scheme's colours, or libadwaita's own for "Default Light" / "Default
/// Dark" (as the file list draws them without a palette: the accent for
/// the cursor, the warning colour for marks).
fn colours(name: &str, dark: bool) -> Colours {
    let s = |v: &str| v.to_string();
    if let Some(p) = themes::palette(name.to_string()) {
        return Colours {
            bg: crate::theme::color(&p.background),
            chrome: crate::theme::color(&p.chrome),
            fg: crate::theme::color(&p.foreground),
            muted: crate::theme::color(&p.muted),
            border: crate::theme::color(&p.border),
            cursor_bg: crate::theme::color(&p.selection),
            cursor_fg: crate::theme::color(&p.foreground),
            cursor_muted: crate::theme::color(&p.muted),
            marked: crate::theme::color(&p.marked),
            marked_bg: crate::theme::color(&p.marked_background),
            folder: crate::theme::color(&p.folder),
            exec: crate::theme::color(&p.executable),
        };
    }
    if dark {
        Colours {
            bg: s("#1e1e1e"),
            chrome: s("#242424"),
            fg: s("#ffffff"),
            muted: s("rgba(255, 255, 255, 0.6)"),
            border: s("rgba(255, 255, 255, 0.12)"),
            cursor_bg: s("#3584e4"),
            cursor_fg: s("#ffffff"),
            cursor_muted: s("rgba(255, 255, 255, 0.8)"),
            marked: s("#f8e45c"),
            marked_bg: s("rgba(205, 147, 9, 0.16)"),
            folder: s("#78aeed"),
            exec: s("#8ff0a4"),
        }
    } else {
        Colours {
            bg: s("#ffffff"),
            chrome: s("#fafafa"),
            fg: s("rgba(0, 0, 6, 0.8)"),
            muted: s("rgba(0, 0, 6, 0.55)"),
            border: s("rgba(0, 0, 6, 0.12)"),
            cursor_bg: s("#3584e4"),
            cursor_fg: s("#ffffff"),
            cursor_muted: s("rgba(255, 255, 255, 0.8)"),
            marked: s("#9c6e03"),
            marked_bg: s("rgba(229, 165, 10, 0.16)"),
            folder: s("#1c71d8"),
            exec: s("#1b8553"),
        }
    }
}

fn preview_css(side: &str, c: &Colours, general: &tl_ffi::settings::GeneralSettings) -> String {
    let p = format!(".tl-preview.{side}");
    let font = general.font_size;
    let small = (font - 2.0).max(9.0);
    let height = general.row_height;
    format!(
        r#"
{p} {{ background-color: {bg}; color: {fg}; border: 1px solid {border}; border-radius: 8px; }}
{p} .tl-pv-head, {p} .tl-pv-status {{ background-color: {chrome}; color: {muted}; font-size: {small}px; min-height: 20px; padding: 0 8px; }}
{p} .tl-pv-head {{ border-bottom: 1px solid {border}; }}
{p} .tl-pv-status {{ border-top: 1px solid {border}; }}
{p} .tl-pv-row {{ min-height: {height}px; padding: 0 8px; font-size: {font}px; }}
{p} .tl-pv-size, {p} .tl-pv-icon {{ color: {muted}; }}
{p} .tl-pv-icon.folder {{ color: {folder}; }}
{p} .tl-pv-icon.exec {{ color: {exec}; }}
{p} .tl-pv-row.marked {{ background-color: {marked_bg}; }}
{p} .tl-pv-row.marked .tl-pv-name, {p} .tl-pv-marked {{ color: {marked}; font-weight: bold; }}
{p} .tl-pv-row.cursor {{ background-color: {cursor_bg}; color: {cursor_fg}; }}
{p} .tl-pv-row.cursor .tl-pv-size {{ color: {cursor_muted}; }}
"#,
        bg = c.bg,
        fg = c.fg,
        border = c.border,
        chrome = c.chrome,
        muted = c.muted,
        folder = c.folder,
        exec = c.exec,
        marked = c.marked,
        marked_bg = c.marked_bg,
        cursor_bg = c.cursor_bg,
        cursor_fg = c.cursor_fg,
        cursor_muted = c.cursor_muted,
    )
}

// ----- style -------------------------------------------------------------------------

const CSS: &str = r#"
.tl-key-chip { border-radius: 6px; padding: 0 0 0 8px; background-color: alpha(currentColor, 0.08); border: 1px solid alpha(currentColor, 0.12); }
.tl-key-chip .tl-key-label { font-feature-settings: "tnum"; font-size: smaller; font-weight: bold; }
.tl-key-chip button { min-width: 22px; min-height: 22px; padding: 0; margin: 0; }
.tl-key-chip button image { -gtk-icon-size: 12px; }
.tl-key-recording { border-radius: 6px; padding: 3px 10px; background-color: alpha(@accent_bg_color, 0.15); border: 1px solid @accent_bg_color; font-size: smaller; }
row.tl-key-note .subtitle { color: @warning_color; opacity: 1; }
"#;

thread_local! {
    static PREVIEW_CSS: std::cell::OnceCell<gtk::CssProvider> = const { std::cell::OnceCell::new() };
}

fn install_css() {
    PREVIEW_CSS.with(|p| {
        if p.get().is_some() {
            return;
        }
        let Some(display) = gdk::Display::default() else { return };
        let fixed = gtk::CssProvider::new();
        fixed.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(&display, &fixed, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let preview = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(&display, &preview, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 2);
        let _ = p.set(preview);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scheme_paints_a_preview() {
        let general = tl_ffi::settings::GeneralSettings {
            restore_folders: true,
            show_hidden: false,
            folders_first: true,
            sort: tl_ffi::types::SortKey::Name,
            sort_descending: false,
            show_icons: true,
            confirm_trash: true,
            font_size: 13.0,
            row_height: 22.0,
            date_format: tl_ffi::types::DateFormat::Iso,
            terminal: String::new(),
        };
        for dark in [false, true] {
            for choice in themes::theme_choices(dark) {
                let css = preview_css("dark", &colours(&choice.name, dark), &general);
                assert!(css.contains("min-height: 22px") && !css.contains("#rgba"), "{}", choice.name);
            }
        }
    }

    #[test]
    fn a_clash_is_found_however_cmd_is_spelled() {
        // On Linux `ctrl-c` is the keys of `cmd-c`.
        let spec = table::command("refresh").expect("refresh");
        let clashes = table::conflicts_on("ctrl-c", spec, &BTreeMap::new(), PLATFORM);
        assert!(clashes.iter().any(|c| c.id == "clipboard-copy"), "{clashes:?}");
    }
}
