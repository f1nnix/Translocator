//! What the application remembers between runs, and the file it lives in.
//!
//! One JSON file under the application data directory, written atomically so
//! a crash or a full disk cannot leave half a file behind, and read
//! defensively: an unreadable file is kept aside instead of overwritten, and
//! every field has a default, so a file written by an older or newer version
//! still loads.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tl_core::bookmark::app_data_dir;

use crate::app_state::AppState;
use crate::pane::SortKey;

/// Written into every file, so a later version can tell what it is reading.
pub const VERSION: u32 = 1;

/// Everything persisted between runs: the preferences the settings window
/// edits, plus the little that is remembered about the last session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub general: General,
    pub appearance: Appearance,
    /// Keystrokes per command, by [`crate::keymap::Command::id`]. A command
    /// that is absent keeps its default keys; one present with an empty list
    /// has none.
    pub keys: BTreeMap<String, Vec<String>>,
    pub session: Session,

    // ----- read from files written before the sections existed -----
    #[serde(skip_serializing)]
    show_hidden: Option<bool>,
    #[serde(skip_serializing)]
    left_path: Option<String>,
    #[serde(skip_serializing)]
    right_path: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            version: VERSION,
            general: General::default(),
            appearance: Appearance::default(),
            keys: BTreeMap::new(),
            session: Session::default(),
            show_hidden: None,
            left_path: None,
            right_path: None,
        }
    }
}

/// Behaviour of the file list and the commands that act on it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Start in the folders the panes were last in.
    pub restore_folders: bool,
    /// List entries whose name begins with a dot.
    pub show_hidden: bool,
    /// Folders before files, whatever the sort column.
    pub folders_first: bool,
    /// Column a new tab sorts by.
    pub sort: SortKey,
    /// Largest, newest or last-by-name first.
    pub sort_descending: bool,
    /// Icons in front of file names.
    pub show_icons: bool,
    /// Ask before moving files to the trash. Permanent deletion always asks.
    pub confirm_trash: bool,
    pub font_size: f32,
    pub row_height: f32,
    pub date_format: DateFormat,
    /// Terminal to open for ⌘⇧T. Empty uses `$TERMINAL` and then whatever
    /// the desktop has; on macOS, Terminal.app.
    pub terminal: String,
}

impl Default for General {
    fn default() -> Self {
        General {
            restore_folders: true,
            show_hidden: false,
            folders_first: true,
            sort: SortKey::Name,
            sort_descending: false,
            show_icons: true,
            confirm_trash: true,
            font_size: 13.,
            row_height: 22.,
            date_format: DateFormat::Iso,
            terminal: String::new(),
        }
    }
}

impl General {
    /// Font size, held to what the list can still lay out.
    pub fn font_size(&self) -> f32 {
        self.font_size.clamp(FONT_SIZE.0, FONT_SIZE.1)
    }

    /// Row height, at least tall enough for the font.
    pub fn row_height(&self) -> f32 {
        self.row_height.clamp(ROW_HEIGHT.0, ROW_HEIGHT.1).max(self.font_size() + 4.)
    }
}

/// Range the settings window offers, and that a hand-edited file is held to.
pub const FONT_SIZE: (f32, f32) = (10., 20.);
pub const ROW_HEIGHT: (f32, f32) = (16., 40.);

/// How a date is written in the file list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DateFormat {
    /// `2026-10-15 14:32`
    #[default]
    Iso,
    /// `15.10.2026 14:32`
    DayFirst,
    /// `10/15/2026 2:32 PM`
    MonthFirst,
    /// `today 14:32`, `Oct 3 09:12`, `2024-03-01 11:40`
    Relative,
}

impl DateFormat {
    pub const ALL: [DateFormat; 4] = [
        DateFormat::Iso,
        DateFormat::DayFirst,
        DateFormat::MonthFirst,
        DateFormat::Relative,
    ];

    /// What the dropdown shows: an example, which says more than a name.
    pub fn label(&self) -> &'static str {
        match self {
            DateFormat::Iso => "2026-10-15 14:32",
            DateFormat::DayFirst => "15.10.2026 14:32",
            DateFormat::MonthFirst => "10/15/2026 2:32 PM",
            DateFormat::Relative => "today 14:32",
        }
    }

    /// Stored name, for the dropdown and the file.
    pub fn id(&self) -> &'static str {
        match self {
            DateFormat::Iso => "iso",
            DateFormat::DayFirst => "day-first",
            DateFormat::MonthFirst => "month-first",
            DateFormat::Relative => "relative",
        }
    }

    pub fn from_id(id: &str) -> DateFormat {
        DateFormat::ALL
            .into_iter()
            .find(|f| f.id() == id)
            .unwrap_or_default()
    }
}

/// Which theme is in use, and when.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub mode: AppearanceMode,
    /// Theme name used in light mode, as the theme catalogue spells it.
    pub light: String,
    /// Theme name used in dark mode.
    pub dark: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            mode: AppearanceMode::System,
            light: crate::themes::DEFAULT_LIGHT.to_string(),
            dark: crate::themes::DEFAULT_DARK.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppearanceMode {
    /// Follow the desktop's light / dark setting.
    #[default]
    System,
    Light,
    Dark,
}

impl AppearanceMode {
    pub const ALL: [AppearanceMode; 3] = [
        AppearanceMode::System,
        AppearanceMode::Light,
        AppearanceMode::Dark,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            AppearanceMode::System => "System",
            AppearanceMode::Light => "Light",
            AppearanceMode::Dark => "Dark",
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            AppearanceMode::System => "system",
            AppearanceMode::Light => "light",
            AppearanceMode::Dark => "dark",
        }
    }

    pub fn from_id(id: &str) -> AppearanceMode {
        AppearanceMode::ALL
            .into_iter()
            .find(|m| m.id() == id)
            .unwrap_or_default()
    }
}

/// Where the panes were when the application last quit.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub left_path: Option<String>,
    pub right_path: Option<String>,
}

impl Settings {
    /// `~/Library/Application Support/Translocator/settings.json` on macOS.
    pub fn default_path() -> PathBuf {
        app_data_dir().join("settings.json")
    }

    /// Reads `path`, falling back to the defaults for anything the file does
    /// not say. A file that cannot be parsed is copied aside rather than
    /// quietly replaced, so a hand edit with one comma too few can be fixed.
    pub fn load(path: &Path) -> Self {
        let mut settings = match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<Settings>(&bytes) {
                Ok(settings) => settings,
                Err(err) => {
                    log::error!("cannot parse {}: {err}", path.display());
                    let _ = std::fs::copy(path, path.with_extension("json.broken"));
                    Settings::default()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Settings::default(),
            Err(err) => {
                log::error!("cannot read {}: {err}", path.display());
                Settings::default()
            }
        };
        settings.migrate();
        settings
    }

    /// Folds what older versions wrote at the top level into the sections,
    /// and holds hand-edited numbers to the range the window offers.
    fn migrate(&mut self) {
        if let Some(show_hidden) = self.show_hidden.take() {
            self.general.show_hidden = show_hidden;
        }
        if let Some(path) = self.left_path.take() {
            self.session.left_path = Some(path);
        }
        if let Some(path) = self.right_path.take() {
            self.session.right_path = Some(path);
        }
        self.general.font_size = self.general.font_size();
        self.general.row_height = self.general.row_height();
        self.version = VERSION;
    }

    /// Atomic write (temp file + rename), readable only by the user.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)
    }
}

/// Settings and the file they came from.
pub struct SettingsStore {
    path: PathBuf,
    settings: Settings,
    /// What is on disk, so an edit that changes nothing writes nothing.
    saved: Settings,
}

impl SettingsStore {
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let settings = Settings::load(&path);
        SettingsStore {
            path,
            saved: settings.clone(),
            settings,
        }
    }

    /// Writes the settings, unless the file already says the same thing.
    pub fn flush(&mut self) {
        if self.settings == self.saved {
            return;
        }
        match self.settings.save(&self.path) {
            Ok(()) => self.saved = self.settings.clone(),
            Err(err) => log::error!("cannot save {}: {err}", self.path.display()),
        }
    }
}

impl std::ops::Deref for SettingsStore {
    type Target = Settings;

    fn deref(&self) -> &Settings {
        &self.settings
    }
}

impl std::ops::DerefMut for SettingsStore {
    fn deref_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }
}

/// Edits the settings, writes them and redraws every window.
///
/// Everything that reads a setting reads it from the global on the way to
/// painting, so one refresh is all a change needs — bar the theme and the
/// key map, which install themselves and are applied by their own modules.
pub fn update(cx: &mut gpui_kit::App, edit: impl FnOnce(&mut Settings)) {
    let store = &mut cx.global_mut::<AppState>().settings;
    edit(store);
    store.flush();
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        (dir, path)
    }

    #[test]
    fn a_missing_file_gives_the_defaults() {
        let (_dir, path) = file();
        let settings = Settings::load(&path);
        assert_eq!(settings, Settings::default());
        assert!(settings.general.restore_folders);
        assert!(settings.general.confirm_trash);
        assert_eq!(settings.appearance.mode, AppearanceMode::System);
    }

    #[test]
    fn what_is_saved_comes_back() {
        let (_dir, path) = file();
        let mut store = SettingsStore::load(&path);
        store.general.show_hidden = true;
        store.general.sort = SortKey::Size;
        store.appearance.mode = AppearanceMode::Dark;
        store.appearance.dark = "Gruvbox Dark".into();
        store.keys.insert("copy-files".into(), vec!["f9".into()]);
        store.session.left_path = Some("/etc".into());
        store.flush();

        let back = Settings::load(&path);
        assert!(back.general.show_hidden);
        assert_eq!(back.general.sort, SortKey::Size);
        assert_eq!(back.appearance.mode, AppearanceMode::Dark);
        assert_eq!(back.appearance.dark, "Gruvbox Dark");
        assert_eq!(back.keys["copy-files"], ["f9"]);
        assert_eq!(back.session.left_path.as_deref(), Some("/etc"));
    }

    #[test]
    fn an_unchanged_store_does_not_write() {
        let (_dir, path) = file();
        let mut store = SettingsStore::load(&path);
        store.flush();
        assert!(!path.exists(), "nothing changed, so there is nothing to write");
        store.general.show_hidden = true;
        store.flush();
        let written = std::fs::metadata(&path).unwrap().modified().unwrap();
        store.flush();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), written);
    }

    #[test]
    fn a_file_from_the_first_release_keeps_its_settings() {
        let (_dir, path) = file();
        std::fs::write(
            &path,
            r#"{"show_hidden":true,"left_path":"/var/log","right_path":null}"#,
        )
        .unwrap();
        let settings = Settings::load(&path);
        assert!(settings.general.show_hidden);
        assert_eq!(settings.session.left_path.as_deref(), Some("/var/log"));
        assert_eq!(settings.version, VERSION);

        // And the old keys are gone once it is written again.
        let mut store = SettingsStore::load(&path);
        store.general.folders_first = false;
        store.flush();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("\"show_hidden\": true,\n  \"left"), "{text}");
        assert!(Settings::load(&path).general.show_hidden);
    }

    #[test]
    fn a_setting_the_file_omits_falls_back_to_its_default() {
        let (_dir, path) = file();
        std::fs::write(&path, r#"{"general":{"row_height":30.0},"keys":{}}"#).unwrap();
        let settings = Settings::load(&path);
        assert_eq!(settings.general.row_height, 30.);
        assert_eq!(settings.general.font_size, 13.);
        assert!(settings.general.show_icons);
        assert_eq!(settings.appearance.light, crate::themes::DEFAULT_LIGHT);
    }

    #[test]
    fn a_broken_file_is_kept_and_the_defaults_used() {
        let (_dir, path) = file();
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load(&path).general, General::default());
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.broken")).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn hand_edited_sizes_are_held_to_what_the_list_can_draw() {
        let (_dir, path) = file();
        std::fs::write(&path, r#"{"general":{"font_size":400.0,"row_height":2.0}}"#).unwrap();
        let settings = Settings::load(&path);
        assert_eq!(settings.general.font_size(), FONT_SIZE.1);
        assert_eq!(settings.general.row_height(), FONT_SIZE.1 + 4.);
    }
}
