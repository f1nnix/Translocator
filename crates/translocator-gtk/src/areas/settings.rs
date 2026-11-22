//! STUB: the preferences window, opened when `Snapshot.settings_requests`
//! moves (the open-settings command).
//!
//! Must become an `adw::PreferencesDialog` built from
//! `Snapshot.settings` (`tl_ffi::settings::SettingsSnapshot`), every change
//! sent as `Input::Settings { edit: SettingsEdit::.. }`:
//! - General: the switches of `GeneralSettings`, the sort and date format
//!   pickers (`tl_ffi::settings::sort_key_choices`,
//!   `date_format_choices`), font size and row height spin rows held to
//!   `settings_ranges`, the terminal entry with its help, Reset;
//! - Appearance: the mode (`appearance_mode_choices`) and the light and
//!   dark theme pickers (`tl_ffi::themes::theme_choices`);
//! - Keys: the key table (`Core::key_table`, by group) with each command's
//!   bindings as `crate::keys::label`, recording a new one from a key
//!   event in the shared spelling (`tl_ffi::keys::keystroke_spelling`;
//!   Ctrl is written `cmd`, the primary modifier, so the file stays
//!   portable), checked with `tl_ffi::keys::validate_keys` and for clashes
//!   with `tl_app::keys::conflicts_on(keys, spec, overrides,
//!   Platform::Linux)` (on Linux `cmd-a` and `ctrl-a` are one key; the
//!   ffi's `Core::key_conflicts` compares as macOS does), saved with
//!   `SettingsEdit::Keys`.
//!
//! Called by the shell as `present(&Snapshot, &window)`; it should update
//! in place from later snapshots (`SettingsSnapshot.version`). Swift:
//! `macos/.../Areas/SettingsWindow.swift`.

use adw::prelude::*;
use tl_ffi::snapshot::Snapshot;

pub fn present(_snapshot: &Snapshot, parent: &impl IsA<gtk::Widget>) {
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder()
        .title("General")
        .description("Preferences are not available in the GTK app yet.")
        .build();
    page.add(&group);
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Preferences");
    dialog.add(&page);
    dialog.present(Some(parent));
}
