//! AREA: the settings window (⌘,).
//!
//! Owns what the settings window reads ([`SettingsSnapshot`], carried in
//! `Snapshot.settings` because the main window needs the list's font, row
//! height and date format too) and what it sends ([`SettingsEdit`], the
//! full mirror of `tl_app::model::SettingsEdit`).
//!
//! The Swift side is `macos/Sources/Translocator/Areas/SettingsWindow.swift`
//! (a stub today). Still to add here for it: labels for the pickers
//! (`DateFormat::label`, `SortKey::label`, `AppearanceMode::label` from
//! tl-app), and whatever the key-binding page needs beyond `crate::keys`.
//! Keep the boundary: records and conversions only, no decisions.

use tl_app::model;
use tl_app::settings::Settings;

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
    /// Theme names of the GPUI shell; the native app follows the system
    /// look and only uses `mode`.
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
