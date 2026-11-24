//! What the application remembers between runs: the preferences the
//! settings window edits, and the little that is remembered about the last
//! session. Every field has a default, so a file written by an older or
//! newer version still loads. Reading and writing the file is the shell's
//! job ([`crate::capabilities::StoreOp`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::pane::Side;

/// Theme names used when the settings name none (they ship with the UI kit).
pub const DEFAULT_LIGHT: &str = "Default Light";
pub const DEFAULT_DARK: &str = "Default Dark";

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
    /// Keystrokes per command, by command id. A command
    /// that is absent keeps its default keys; one present with an empty list
    /// has none.
    pub keys: BTreeMap<String, Vec<String>>,
    pub session: Session,

    // ----- read from files written before the sections existed -----
    #[serde(skip_serializing)]
    pub(crate) show_hidden: Option<bool>,
    #[serde(skip_serializing)]
    pub(crate) left_path: Option<String>,
    #[serde(skip_serializing)]
    pub(crate) right_path: Option<String>,
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
            light: DEFAULT_LIGHT.to_string(),
            dark: DEFAULT_DARK.to_string(),
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

/// Where the panes were when the application last quit, and how each
/// shows its folder.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub left_path: Option<String>,
    pub right_path: Option<String>,
    pub left_view: ViewMode,
    pub right_view: ViewMode,
}

impl Session {
    pub fn view(&self, side: Side) -> ViewMode {
        match side {
            Side::Left => self.left_view,
            Side::Right => self.right_view,
        }
    }

    pub fn set_view(&mut self, side: Side, mode: ViewMode) {
        match side {
            Side::Left => self.left_view = mode,
            Side::Right => self.right_view = mode,
        }
    }
}

/// How a pane shows its folder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViewMode {
    /// Rows with columns of details.
    #[default]
    List,
    /// The folders above as columns to the left, the folder under the
    /// cursor as one to the right.
    Columns,
    /// Icons in rows, wrapped to the pane's width.
    Grid,
}

impl ViewMode {
    pub const ALL: [ViewMode; 3] = [ViewMode::List, ViewMode::Columns, ViewMode::Grid];

    pub fn label(&self) -> &'static str {
        match self {
            ViewMode::List => "as List",
            ViewMode::Columns => "as Columns",
            ViewMode::Grid => "as Icons",
        }
    }
}

impl Settings {
    /// Folds what older versions wrote at the top level into the sections,
    /// and holds hand-edited numbers to the range the window offers. The
    /// shell calls it on whatever it read from disk.
    pub fn migrate(&mut self) {
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
}

/// Column a list is sorted by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortKey {
    #[default]
    Name,
    Ext,
    Size,
    Modified,
}

impl SortKey {
    pub const ALL: [SortKey; 4] = [SortKey::Name, SortKey::Ext, SortKey::Size, SortKey::Modified];

    /// What the settings window's dropdown calls it.
    pub fn label(&self) -> &'static str {
        match self {
            SortKey::Name => "Name",
            SortKey::Ext => "Extension",
            SortKey::Size => "Size",
            SortKey::Modified => "Date modified",
        }
    }

    /// Name in the settings file and the settings window's dropdown.
    pub fn id(&self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Ext => "extension",
            SortKey::Size => "size",
            SortKey::Modified => "modified",
        }
    }

    pub fn from_id(id: &str) -> SortKey {
        match id {
            "extension" => SortKey::Ext,
            "size" => SortKey::Size,
            "modified" => SortKey::Modified,
            _ => SortKey::Name,
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_from_the_first_release_keeps_its_settings() {
        let mut settings: Settings =
            serde_json::from_str(r#"{"show_hidden":true,"left_path":"/var/log","right_path":null}"#).unwrap();
        settings.migrate();
        assert!(settings.general.show_hidden);
        assert_eq!(settings.session.left_path.as_deref(), Some("/var/log"));
        assert_eq!(settings.version, VERSION);
        let json: serde_json::Value = serde_json::to_value(&settings).unwrap();
        assert!(json.get("left_path").is_none(), "the old top-level keys are not written back");
        assert!(json.get("show_hidden").is_none());
    }

    #[test]
    fn a_setting_the_file_omits_falls_back_to_its_default() {
        let settings: Settings = serde_json::from_str(r#"{"general":{"row_height":30.0},"keys":{}}"#).unwrap();
        assert_eq!(settings.general.row_height, 30.);
        assert_eq!(settings.general.font_size, 13.);
        assert!(settings.general.show_icons);
        assert_eq!(settings.appearance.light, DEFAULT_LIGHT);
    }

    #[test]
    fn each_pane_keeps_its_view() {
        let settings: Settings = serde_json::from_str(r#"{"session":{"right_view":"columns"}}"#).unwrap();
        assert_eq!(settings.session.view(Side::Left), ViewMode::List);
        assert_eq!(settings.session.view(Side::Right), ViewMode::Columns);
        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(json["session"]["right_view"], "columns");
        assert_eq!(json["session"]["left_view"], "list");
    }

    #[test]
    fn hand_edited_sizes_are_held_to_what_the_list_can_draw() {
        let mut settings: Settings =
            serde_json::from_str(r#"{"general":{"font_size":400.0,"row_height":2.0}}"#).unwrap();
        settings.migrate();
        assert_eq!(settings.general.font_size(), FONT_SIZE.1);
        assert_eq!(settings.general.row_height(), FONT_SIZE.1 + 4.);
    }
}
