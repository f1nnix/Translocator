//! Dark and light, and the content colours.
//!
//! The chrome is libadwaita's own: `AdwStyleManager` follows the
//! appearance setting (system, light, dark). Only the content layer — the
//! file lists and the console — is painted in the chosen colour scheme
//! (`tl_ffi::themes`); "Default Light" / "Default Dark" have no palette
//! and use libadwaita's colours. The list's font size and row height come
//! from the settings, through the same generated style sheet.

use std::cell::RefCell;

use gtk::gdk;
use tl_ffi::settings::SettingsSnapshot;
use tl_ffi::themes::ThemePalette;
use tl_ffi::types::AppearanceMode;

/// The shell's own style, in libadwaita's named colours.
const BASE_CSS: &str = r#"
.tl-pane-header { padding: 2px 6px; min-height: 34px; }
.tl-pane-header.focused { background-color: alpha(@accent_bg_color, 0.10); }
.tl-crumb { padding: 2px 5px; min-height: 0; font-weight: normal; }
.tl-crumb.current { font-weight: bold; }
.tl-tabs { padding: 2px 4px; }
.tl-tab { border-radius: 6px; }
.tl-tab.current { background-color: alpha(@accent_bg_color, 0.18); }
.tl-tab button { padding: 2px 6px; min-height: 0; }
.tl-tab-close { min-width: 18px; min-height: 18px; padding: 0; }
.tl-places { padding: 1px 6px; font-size: smaller; }
.tl-place { padding: 1px 7px; min-height: 0; font-weight: normal; }
.tl-place.current { background-color: alpha(@accent_bg_color, 0.22); }
.tl-status { padding: 3px 8px; font-size: smaller; }
.tl-status label { opacity: 0.7; }
.tl-chip { padding: 4px 10px; border-radius: 999px; font-size: smaller; }
.tl-cover { background-color: alpha(@view_bg_color, 0.9); }

.tl-list > listview > row { padding: 0; min-height: 0; border-radius: 0; margin: 0; }
.tl-list > listview > row > cell { padding: 0; min-height: 0; }
.tl-list > listview > row:focus-visible { outline: none; }
.tl-list > listview > row.marked { background-color: alpha(@warning_bg_color, 0.16); }
.tl-list > listview > row.marked label { color: @warning_color; font-weight: bold; }
.tl-list > listview > row.hidden-file > cell { opacity: 0.6; }
.tl-list > listview > row:selected { background-color: alpha(currentColor, 0.10); }
.tl-list.focused > listview > row:selected { background-color: @accent_bg_color; color: @accent_fg_color; }
.tl-list.focused > listview > row:selected .tl-muted,
.tl-list.focused > listview > row:selected .tl-icon { color: inherit; }
.tl-cell { padding: 0 6px; }
.tl-list.focused > listview > row.marked:selected label { color: @accent_fg_color; }
.tl-muted { opacity: 0.65; }
.tl-small { font-size: smaller; }
.tl-icon { opacity: 0.75; }
.tl-icon.folder { color: @accent_color; opacity: 1; }
.tl-icon.exec { color: @success_color; opacity: 1; }
.tl-drag-label { padding: 4px 10px; border-radius: 6px; background-color: @popover_bg_color; }

.tl-cmdline { padding: 2px 8px; }
.tl-cmdline entry { min-height: 28px; }
.tl-jobs-strip { padding: 2px 8px; font-size: smaller; min-height: 28px; }
.tl-function-bar { padding: 2px 4px; }
.tl-function-bar button { padding: 2px 4px; min-height: 0; font-weight: normal; }
.tl-fkey { font-size: smaller; font-weight: bold; opacity: 0.55; }
"#;

pub struct Theme {
    dynamic: gtk::CssProvider,
    /// The style sheet last loaded, to skip reloading it.
    loaded: RefCell<String>,
}

impl Theme {
    pub fn install(display: &gdk::Display) -> Theme {
        let base = gtk::CssProvider::new();
        base.load_from_string(BASE_CSS);
        gtk::style_context_add_provider_for_display(display, &base, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let dynamic = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(display, &dynamic, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
        Theme {
            dynamic,
            loaded: RefCell::new(String::new()),
        }
    }

    /// Follows the appearance setting and paints the content in the theme
    /// for the side in effect.
    pub fn apply(&self, settings: &SettingsSnapshot) {
        let manager = adw::StyleManager::default();
        let scheme = match settings.appearance.mode {
            AppearanceMode::System => adw::ColorScheme::Default,
            AppearanceMode::Light => adw::ColorScheme::ForceLight,
            AppearanceMode::Dark => adw::ColorScheme::ForceDark,
        };
        if manager.color_scheme() != scheme {
            manager.set_color_scheme(scheme);
        }
        let name = if manager.is_dark() {
            &settings.appearance.dark
        } else {
            &settings.appearance.light
        };
        // The viewer, the editor and quick look paint their own text.
        crate::areas::code::set_content_theme(name, manager.is_dark());
        let general = &settings.general;
        let mut css = format!(
            ".tl-list {{ font-size: {}px; }}\n.tl-cell {{ min-height: {}px; }}\n",
            general.font_size, general.row_height
        );
        if let Some(p) = tl_ffi::themes::palette(name.clone()) {
            css.push_str(&palette_css(&p));
        }
        if *self.loaded.borrow() != css {
            self.dynamic.load_from_string(&css);
            self.loaded.replace(css);
        }
    }
}

/// The content layer in a scheme's colours.
fn palette_css(p: &ThemePalette) -> String {
    format!(
        r#"
.tl-list, .tl-list > listview, .tl-list > listview > row {{ background-color: {bg}; color: {fg}; }}
.tl-list > header > button {{ background-color: {chrome}; color: {muted}; box-shadow: none; border-bottom: 1px solid {border}; }}
.tl-list > listview > row:hover {{ background-color: {hover}; }}
.tl-list > listview > row.marked {{ background-color: {marked_bg}; }}
.tl-list > listview > row.marked label {{ color: {marked}; }}
.tl-list > listview > row:selected {{ background-color: {inactive}; color: {fg}; }}
.tl-list.focused > listview > row:selected {{ background-color: {selection}; color: {fg}; }}
.tl-list.focused > listview > row:selected .tl-muted {{ color: {muted}; }}
.tl-list.focused > listview > row:selected .tl-icon {{ color: {muted}; }}
.tl-list.focused > listview > row:selected .tl-icon.folder {{ color: {folder}; }}
.tl-list.focused > listview > row:selected .tl-icon.exec {{ color: {exec}; }}
.tl-list.focused > listview > row.marked:selected label {{ color: {marked}; }}
.tl-muted {{ color: {muted}; opacity: 1; }}
.tl-icon {{ color: {muted}; opacity: 1; }}
.tl-icon.folder {{ color: {folder}; }}
.tl-icon.exec {{ color: {exec}; }}
.tl-pane scrolledwindow.tl-list-scroll, .tl-pane overlay {{ background-color: {bg}; }}
textview.tl-console, textview.tl-console text, scrolledwindow.tl-console {{ background-color: {bg}; color: {fg}; }}
"#,
        bg = color(&p.background),
        fg = color(&p.foreground),
        chrome = color(&p.chrome),
        border = color(&p.border),
        muted = color(&p.muted),
        hover = color(&p.hover),
        inactive = color(&p.selection_inactive),
        selection = color(&p.selection),
        marked = color(&p.marked),
        marked_bg = color(&p.marked_background),
        folder = color(&p.folder),
        exec = color(&p.executable),
    )
}

/// `#rrggbb` as it is, `#rrggbbaa` as `rgba()`, which every GTK reads.
pub(crate) fn color(hex: &str) -> String {
    let digits = hex.trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(digits.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
    if digits.len() == 8 {
        format!("rgba({}, {}, {}, {:.3})", byte(0), byte(2), byte(4), f32::from(byte(6)) / 255.0)
    } else {
        hex.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translucent_colours_become_rgba() {
        assert_eq!(color("#2e3440"), "#2e3440");
        assert_eq!(color("#88c0d080"), "rgba(136, 192, 208, 0.502)");
    }

    #[test]
    fn every_palette_makes_a_style_sheet() {
        for dark in [false, true] {
            for choice in tl_ffi::themes::theme_choices(dark).into_iter().skip(1) {
                let palette = tl_ffi::themes::palette(choice.name.clone()).expect("a palette");
                let css = palette_css(&palette);
                assert!(css.contains(&palette.background) && !css.contains("#rgba"), "{}", choice.name);
            }
        }
    }
}
