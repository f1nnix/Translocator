//! AREA: the settings window (⌘,).
//!
//! Owns what the settings window reads ([`SettingsSnapshot`], carried in
//! `Snapshot.settings` because the main window needs the list's font, row
//! height and date format too), what it sends ([`SettingsEdit`], the full
//! mirror of `tl_app::model::SettingsEdit`), and the fixed text of its
//! controls: the pickers' labels ([`sort_key_choices`],
//! [`date_format_choices`], [`appearance_mode_choices`]), the ranges of the
//! steppers ([`settings_ranges`]) and the terminal field's help. The key
//! bindings page reads `crate::keys`; the theme pickers `crate::themes`.
//!
//! The Swift side is `macos/Sources/Translocator/Areas/SettingsWindow.swift`.
//! Keep the boundary: records and conversions only, no decisions.

use tl_app::model;
use tl_app::settings::{self as core, Settings};

use crate::types::{AppearanceMode, DateFormat, SortKey};

/// The settings as the window and the file list use them.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct SettingsSnapshot {
    /// Changes whenever any setting changed.
    pub version: u64,
    pub general: GeneralSettings,
    pub appearance: AppearanceSettings,
    /// Changes whenever the key bindings changed: rebuild menus and key maps.
    pub keys_version: u64,
}

/// The General page. Sizes are the effective ones (held to what the list
/// can draw).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct GeneralSettings {
    pub restore_folders: bool,
    pub show_hidden: bool,
    pub folders_first: bool,
    pub sort: SortKey,
    pub sort_descending: bool,
    pub show_icons: bool,
    pub confirm_trash: bool,
    pub font_size: f32,
    pub row_height: f32,
    pub date_format: DateFormat,
    /// Empty: `$TERMINAL`, then Terminal.app.
    pub terminal: String,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct AppearanceSettings {
    pub mode: AppearanceMode,
    /// The theme used while the appearance is light, as `crate::themes`
    /// names it ("Default Light" is the system's own colours).
    pub light: String,
    pub dark: String,
}

pub(crate) fn snapshot(settings: &Settings, version: u64, keys_version: u64) -> SettingsSnapshot {
    let g = &settings.general;
    SettingsSnapshot {
        version,
        general: GeneralSettings {
            restore_folders: g.restore_folders,
            show_hidden: g.show_hidden,
            folders_first: g.folders_first,
            sort: g.sort.into(),
            sort_descending: g.sort_descending,
            show_icons: g.show_icons,
            confirm_trash: g.confirm_trash,
            font_size: g.font_size(),
            row_height: g.row_height(),
            date_format: g.date_format.into(),
            terminal: g.terminal.clone(),
        },
        appearance: AppearanceSettings {
            mode: settings.appearance.mode.into(),
            light: settings.appearance.light.clone(),
            dark: settings.appearance.dark.clone(),
        },
        keys_version,
    }
}

/// Edits from the settings window and the toggles that write settings.
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum SettingsEdit {
    RestoreFolders { on: bool },
    ShowHidden { on: bool },
    FoldersFirst { on: bool },
    Sort { key: SortKey },
    SortDescending { on: bool },
    ShowIcons { on: bool },
    ConfirmTrash { on: bool },
    FontSize { size: f32 },
    RowHeight { height: f32 },
    DateFormat { format: DateFormat },
    Terminal { command: String },
    AppearanceMode { mode: AppearanceMode },
    LightTheme { name: String },
    DarkTheme { name: String },
    /// The keys of a command (by its id in the key table), in the settings
    /// file's spelling; `None` goes back to the defaults.
    Keys { command: String, keys: Option<Vec<String>> },
    /// Every setting of the General page back to its default.
    ResetGeneral,
}

impl From<SettingsEdit> for model::SettingsEdit {
    fn from(edit: SettingsEdit) -> Self {
        use model::SettingsEdit as E;
        match edit {
            SettingsEdit::RestoreFolders { on } => E::RestoreFolders(on),
            SettingsEdit::ShowHidden { on } => E::ShowHidden(on),
            SettingsEdit::FoldersFirst { on } => E::FoldersFirst(on),
            SettingsEdit::Sort { key } => E::Sort(key.into()),
            SettingsEdit::SortDescending { on } => E::SortDescending(on),
            SettingsEdit::ShowIcons { on } => E::ShowIcons(on),
            SettingsEdit::ConfirmTrash { on } => E::ConfirmTrash(on),
            SettingsEdit::FontSize { size } => E::FontSize(size),
            SettingsEdit::RowHeight { height } => E::RowHeight(height),
            SettingsEdit::DateFormat { format } => E::DateFormat(format.into()),
            SettingsEdit::Terminal { command } => E::Terminal(command),
            SettingsEdit::AppearanceMode { mode } => E::AppearanceMode(mode.into()),
            SettingsEdit::LightTheme { name } => E::LightTheme(name),
            SettingsEdit::DarkTheme { name } => E::DarkTheme(name),
            SettingsEdit::Keys { command, keys } => E::Keys { command, keys },
            SettingsEdit::ResetGeneral => E::ResetGeneral,
        }
    }
}

/// One entry of the "Sort new tabs by" picker.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SortKeyChoice {
    pub key: SortKey,
    /// "Date modified".
    pub label: String,
}

/// One entry of the "Dates" picker.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct DateFormatChoice {
    pub format: DateFormat,
    /// An example date ("15.10.2026 14:32"), which says more than a name.
    pub label: String,
}

/// One entry of the "Use" (appearance) picker.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct AppearanceModeChoice {
    pub mode: AppearanceMode,
    pub label: String,
}

/// What the text-size and row-height steppers offer (the core holds the
/// settings to the same ranges).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct SettingsRanges {
    pub font_size_min: f32,
    pub font_size_max: f32,
    pub row_height_min: f32,
    pub row_height_max: f32,
}

#[uniffi::export]
pub fn sort_key_choices() -> Vec<SortKeyChoice> {
    core::SortKey::ALL
        .iter()
        .map(|k| SortKeyChoice {
            key: (*k).into(),
            label: k.label().to_string(),
        })
        .collect()
}

#[uniffi::export]
pub fn date_format_choices() -> Vec<DateFormatChoice> {
    core::DateFormat::ALL
        .iter()
        .map(|f| DateFormatChoice {
            format: (*f).into(),
            label: f.label().to_string(),
        })
        .collect()
}

#[uniffi::export]
pub fn appearance_mode_choices() -> Vec<AppearanceModeChoice> {
    core::AppearanceMode::ALL
        .iter()
        .map(|m| AppearanceModeChoice {
            mode: (*m).into(),
            label: m.label().to_string(),
        })
        .collect()
}

#[uniffi::export]
pub fn settings_ranges() -> SettingsRanges {
    SettingsRanges {
        font_size_min: core::FONT_SIZE.0,
        font_size_max: core::FONT_SIZE.1,
        row_height_min: core::ROW_HEIGHT.0,
        row_height_max: core::ROW_HEIGHT.1,
    }
}

/// The help under the terminal field, on macOS.
#[uniffi::export]
pub fn terminal_help() -> String {
    "A command to run instead of Terminal.app, for example `open -a iTerm`. Empty uses Terminal.app.".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pickers_list_every_choice_with_the_cores_labels() {
        let sorts = sort_key_choices();
        assert_eq!(sorts.len(), 4);
        assert_eq!(sorts[3].key, SortKey::Modified);
        assert_eq!(sorts[3].label, "Date modified");
        assert_eq!(date_format_choices()[1].label, "15.10.2026 14:32");
        assert_eq!(appearance_mode_choices()[0].label, "System");
        let r = settings_ranges();
        assert!(r.font_size_min < r.font_size_max && r.row_height_min < r.row_height_max);
    }
}
