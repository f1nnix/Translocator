//! The theme catalogue: ten colour schemes, each in a light and a dark
//! variant, plus the glue that puts the chosen pair in front of the window.
//!
//! A theme in `gpui-component` is a hundred named colours. Writing those out
//! twenty times would be twenty chances to get one wrong, so each scheme is
//! given (in `tl-themes`, which the macOS app reads too) as the dozen colours
//! its authors actually publish — canvas, text, border, the accent and the
//! six hues — and the rest is derived here:
//! stripes and hover states are the canvas mixed towards the text, a colour
//! used as a background gets text that can be read on it, and a hue used as
//! text is pushed towards the text colour until it is legible on the canvas.
//! That last step is what lets a scheme built for a dark terminal keep its
//! colours in a light window.
//!
//! "Default" is the built-in scheme that ships with the component library.
//! Solarized, Gruvbox, One, Catppuccin, GitHub and Tokyo Night are published
//! by their authors in both modes, and Dracula's light companion is its own
//! Alucard. Nord and Monokai are dark schemes: their light variants are
//! built here from the same hues over the family's lightest background.


use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};
use gpui_kit::{App, Window};
use serde_json::Value;

use tl_app::settings::AppearanceMode;
pub use tl_themes::{DEFAULT_DARK, DEFAULT_LIGHT, choices};
use tl_themes::{PALETTES, Palette, alpha, mix, on};

use crate::app_state::Shell;

/// A JSON object of colour names, which is all a theme's colours are.
fn object<const N: usize>(pairs: [(&'static str, String); N]) -> Value {
    Value::Object(pairs.into_iter().map(|(key, color)| (key.to_string(), color.into())).collect())
}

/// A JSON object whose values are themselves objects or colours.
fn tree<const N: usize>(pairs: [(&'static str, Value); N]) -> Value {
    Value::Object(pairs.into_iter().map(|(key, value)| (key.to_string(), value)).collect())
}

/// One syntax style: a colour, and the slant or weight it is drawn in.
fn style(color: String, font_style: Option<&str>, weight: Option<u16>) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("color".into(), color.into());
    if let Some(font_style) = font_style {
        map.insert("font_style".into(), font_style.into());
    }
    if let Some(weight) = weight {
        map.insert("font_weight".into(), weight.into());
    }
    Value::Object(map)
}

/// A palette as a `gpui-component` theme.
trait ThemeConfig {
    fn colors(&self) -> Value;
    fn highlight(&self) -> Value;
    fn config(&self) -> Value;
}

impl ThemeConfig for Palette {
    fn colors(&self) -> Value {
        let stripe = self.raise(0.035);
        let hover = self.raise(0.07);
        let muted = self.raise(0.1);
        let strong = self.raise(0.16);
        let accent_text = on(self.accent, self);
        let dim = self.mark(self.dim);
        let link = self.mark(self.accent);
        object([
            ("accent.background", hover.clone()),
            ("accent.foreground", self.fg.into()),
            ("accordion.background", self.bg.into()),
            ("background", self.bg.into()),
            ("base.blue", self.mark(self.blue)),
            ("base.blue.light", self.blue.into()),
            ("base.cyan", self.mark(self.cyan)),
            ("base.cyan.light", self.cyan.into()),
            ("base.green", self.mark(self.green)),
            ("base.green.light", self.green.into()),
            ("base.magenta", self.mark(self.magenta)),
            ("base.magenta.light", self.magenta.into()),
            ("base.red", self.mark(self.red)),
            ("base.red.light", self.red.into()),
            ("base.yellow", self.mark(self.yellow)),
            ("base.yellow.light", self.yellow.into()),
            ("border", self.border.into()),
            ("caret", self.fg.into()),
            ("chart.1", self.blue.into()),
            ("chart.2", self.cyan.into()),
            ("chart.3", self.green.into()),
            ("chart.4", self.yellow.into()),
            ("chart.5", self.magenta.into()),
            ("chart.bearish", self.red.into()),
            ("chart.bullish", self.green.into()),
            ("chart.grid", alpha(self.border, 0.6)),
            ("danger.background", self.red.into()),
            ("danger.foreground", on(self.red, self)),
            ("description_list_label.background", self.chrome.into()),
            ("description_list_label.foreground", self.fg.into()),
            ("drag_border", self.accent.into()),
            ("drop_target.background", alpha(self.accent, 0.22)),
            ("foreground", self.fg.into()),
            ("group_box.background", self.chrome.into()),
            ("group_box.foreground", self.fg.into()),
            ("info.background", self.cyan.into()),
            ("info.foreground", on(self.cyan, self)),
            ("input.border", self.border.into()),
            ("link.active.foreground", link.clone()),
            ("link.foreground", link.clone()),
            ("link.hover.foreground", mix(&link, self.fg, 0.25)),
            ("list.active.background", alpha(self.accent, 0.28)),
            ("list.active.border", self.accent.into()),
            ("list.background", self.bg.into()),
            ("list.even.background", stripe.clone()),
            ("list.head.background", self.chrome.into()),
            ("list.hover.background", hover.clone()),
            ("muted.background", muted.clone()),
            ("muted.foreground", dim.clone()),
            ("overlay", "#00000066".into()),
            ("popover.background", self.chrome.into()),
            ("popover.foreground", self.fg.into()),
            ("primary.active.background", mix(self.accent, self.bg, 0.15)),
            ("primary.background", self.accent.into()),
            ("primary.foreground", accent_text.clone()),
            ("primary.hover.background", mix(self.accent, self.fg, 0.15)),
            ("progress_bar.background", self.accent.into()),
            ("ring", self.accent.into()),
            ("scrollbar.background", alpha(self.chrome, 0.)),
            ("scrollbar.thumb.background", alpha(&dim, 0.7)),
            ("scrollbar.thumb.hover.background", dim.clone()),
            ("secondary.active.background", strong.clone()),
            ("secondary.background", muted.clone()),
            ("secondary.foreground", self.fg.into()),
            ("secondary.hover.background", self.raise(0.13)),
            ("selection.background", alpha(self.accent, 0.35)),
            ("sidebar.accent.background", hover.clone()),
            ("sidebar.accent.foreground", self.fg.into()),
            ("sidebar.background", self.chrome.into()),
            ("sidebar.border", self.border.into()),
            ("sidebar.foreground", self.fg.into()),
            ("sidebar.primary.background", self.accent.into()),
            ("sidebar.primary.foreground", accent_text.clone()),
            ("skeleton.background", muted),
            ("slider.bar.background", self.accent.into()),
            ("slider.thumb.background", self.bg.into()),
            ("status_bar.background", self.chrome.into()),
            ("status_bar.border", self.border.into()),
            ("success.background", self.green.into()),
            ("success.foreground", on(self.green, self)),
            ("switch.background", strong),
            ("tab.active.background", self.bg.into()),
            ("tab.active.foreground", self.fg.into()),
            ("tab.background", "#00000000".into()),
            ("tab.foreground", dim.clone()),
            ("tab_bar.background", self.chrome.into()),
            ("tab_bar.segmented.background", self.chrome.into()),
            ("table.active.background", alpha(self.accent, 0.28)),
            ("table.active.border", self.accent.into()),
            ("table.background", self.bg.into()),
            ("table.even.background", stripe),
            ("table.head.background", self.chrome.into()),
            ("table.head.foreground", dim),
            ("table.hover.background", hover),
            ("table.row.border", alpha(self.border, 0.7)),
            ("title_bar.background", self.chrome.into()),
            ("title_bar.border", self.border.into()),
            ("warning.background", self.yellow.into()),
            ("warning.foreground", on(self.yellow, self)),
            ("window.border", self.border.into()),
        ])
    }

    /// Syntax colours for the viewer and the editor, so a file on a server
    /// is read in the same scheme as the panes around it.
    fn highlight(&self) -> Value {
        let comment = self.mark(self.dim);
        let text = |color: &str| style(self.text(color), None, None);
        let italic = |color: &str| style(self.text(color), Some("italic"), None);
        let syntax = tree([
            ("attribute", text(self.yellow)),
            ("boolean", text(self.magenta)),
            ("comment", style(comment.clone(), Some("italic"), None)),
            ("comment.doc", style(comment.clone(), Some("italic"), None)),
            ("constant", text(self.cyan)),
            ("constructor", text(self.yellow)),
            ("embedded", style(self.fg.into(), None, None)),
            ("emphasis", style(self.fg.into(), Some("italic"), None)),
            ("emphasis.strong", style(self.fg.into(), None, Some(700))),
            ("enum", text(self.yellow)),
            ("function", text(self.blue)),
            ("keyword", text(self.magenta)),
            ("label", text(self.blue)),
            ("link_text", text(self.cyan)),
            ("link_uri", italic(self.cyan)),
            ("number", text(self.red)),
            ("operator", text(self.magenta)),
            ("preproc", text(self.yellow)),
            ("primary", style(self.fg.into(), None, None)),
            ("property", text(self.red)),
            ("punctuation", style(comment.clone(), None, None)),
            ("punctuation.bracket", style(self.fg.into(), None, None)),
            ("punctuation.delimiter", style(comment.clone(), None, None)),
            ("punctuation.list_marker", text(self.red)),
            ("punctuation.special", text(self.magenta)),
            ("string", text(self.green)),
            ("string.escape", text(self.cyan)),
            ("string.regex", text(self.cyan)),
            ("string.special", text(self.cyan)),
            ("string.special.symbol", text(self.green)),
            ("tag", text(self.red)),
            ("tag.doctype", text(self.magenta)),
            ("text.code.span", text(self.green)),
            ("text.literal", text(self.green)),
            ("title", style(self.text(self.blue), None, Some(700))),
            ("type", text(self.yellow)),
            ("variable", style(self.fg.into(), None, None)),
            ("variable.special", text(self.red)),
            ("variant", text(self.cyan)),
        ]);
        tree([
            ("editor.background", self.bg.into()),
            ("editor.foreground", self.fg.into()),
            ("editor.active_line.background", self.raise(0.05).into()),
            ("editor.gutter.background", self.bg.into()),
            ("editor.line_number", comment.clone().into()),
            ("editor.active_line_number", self.fg.into()),
            ("editor.invisible", alpha(&comment, 0.5).into()),
            ("conflict", self.text(self.red).into()),
            ("created", self.text(self.green).into()),
            ("deleted", self.text(self.red).into()),
            ("hidden", comment.clone().into()),
            ("hint", self.text(self.magenta).into()),
            ("modified", self.text(self.yellow).into()),
            ("predictive", comment.into()),
            ("warning", self.text(self.yellow).into()),
            ("syntax", syntax),
        ])
    }

    fn config(&self) -> Value {
        tree([
            ("name", self.name.into()),
            ("mode", if self.dark { "dark" } else { "light" }.into()),
            ("colors", self.colors()),
            ("highlight", self.highlight()),
        ])
    }
}

/// The schemes built here, in the theme-file format the registry reads.
fn theme_set() -> String {
    let themes: Vec<Value> = PALETTES.iter().map(Palette::config).collect();
    tree([("name", "Translocator".into()), ("themes", Value::Array(themes))]).to_string()
}

/// Adds the catalogue to the registry. Call once, before the first window.
pub fn register(cx: &mut App) {
    let json = theme_set();
    if let Err(err) = ThemeRegistry::global_mut(cx).load_themes_from_str(&json) {
        // The catalogue is built in this file, so this means a bug here and
        // not a bad file on disk: the built-in themes still work.
        log::error!("cannot load the theme catalogue: {err}");
    }
}

/// Installs the themes the settings name and switches to the mode they ask
/// for. The settings are the core's, as last rendered; the shell calls this
/// again whenever their appearance part changes (the title bar's sun/moon
/// and the settings window change it through the core). `window` is read for the system appearance, which on Linux is only
/// known per window.
pub fn apply(window: Option<&Window>, cx: &mut App) {
    let settings = cx.global::<Shell>().view.settings.appearance.clone();
    let mode = match settings.mode {
        AppearanceMode::Light => ThemeMode::Light,
        AppearanceMode::Dark => ThemeMode::Dark,
        AppearanceMode::System => match window {
            Some(window) => window.appearance().into(),
            None => cx.window_appearance().into(),
        },
    };
    let registry = ThemeRegistry::global(cx);
    let light = registry
        .themes()
        .get(settings.light.as_str())
        .or_else(|| registry.themes().get(DEFAULT_LIGHT))
        .cloned();
    let dark = registry
        .themes()
        .get(settings.dark.as_str())
        .or_else(|| registry.themes().get(DEFAULT_DARK))
        .cloned();
    Theme::update(cx, |theme| {
        if let Some(light) = light {
            theme.light_theme = light;
        }
        if let Some(dark) = dark {
            theme.dark_theme = dark;
        }
    });
    // Loads the theme for `mode` even when the mode itself did not change,
    // which is what a swap of the light or dark theme above needs.
    Theme::change(mode, None, cx);
    tweaks(cx);
}

/// Density and fonts for a file manager: the component defaults are sized
/// for forms. Switching theme reloads the theme file, which does not carry
/// these, so they are re-applied after every change.
pub fn tweaks(cx: &mut App) {
    let general = cx.global::<Shell>().view.settings.general.clone();
    Theme::update(cx, |theme| {
        theme.font_size = gpui_kit::px(general.font_size());
        theme.mono_font_size = gpui_kit::px(general.font_size() - 1.);
        theme.radius = gpui_kit::px(5.);
        theme.radius_lg = gpui_kit::px(8.);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // The palettes and the colour maths are tested in `tl-themes`; what is
    // left here is that they make a theme file the registry takes.

    #[test]
    fn the_catalogue_parses_as_a_theme_file() {
        let set: gpui_kit::component::ThemeSet =
            serde_json::from_str(&theme_set()).expect("the generated themes parse");
        assert_eq!(set.themes.len(), PALETTES.len());
        for theme in &set.themes {
            let palette = PALETTES.iter().find(|p| theme.name == p.name).unwrap();
            assert_eq!(theme.mode.is_dark(), palette.dark, "{}", theme.name);
            // A theme that reaches the registry without colours would paint
            // the window in the built-in scheme and look like no change.
            assert!(theme.colors.background.is_some(), "{}", theme.name);
            assert!(theme.highlight.is_some(), "{}", theme.name);
        }
        assert_eq!(choices(false).len(), tl_themes::SCHEMES.len());
    }
}
