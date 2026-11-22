//! The colour schemes (`tl-themes`) for the native shell.
//!
//! The macOS app keeps its chrome native (toolbar, sidebars, controls in
//! the system's materials) and paints only the content layer — the file
//! lists, the console, the viewer and editor — in the chosen scheme. These
//! are the colours it needs for that, as `#rrggbb` (or `#rrggbbaa` where a
//! colour lies over the canvas). "Default Light" and "Default Dark" have no
//! palette: they are the system's own colours.

/// One entry of a light or dark theme picker.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ThemeChoice {
    /// The theme's name, as `SettingsEdit::LightTheme` / `DarkTheme` and
    /// the settings file spell it ("Catppuccin Latte").
    pub name: String,
    /// The scheme it belongs to, which is what the picker shows ("Catppuccin").
    pub scheme: String,
}

/// A scheme's colours for the content layer.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ThemePalette {
    pub name: String,
    pub dark: bool,
    // ----- as published -----
    /// The canvas: file lists, console, editor.
    pub background: String,
    /// Raised surfaces over the canvas: column headers, status lines.
    pub chrome: String,
    pub foreground: String,
    /// Secondary text as published (see `muted` for it as text).
    pub dim: String,
    pub border: String,
    pub accent: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    // ----- derived for the file lists -----
    /// Every other row.
    pub stripe: String,
    /// The row under the pointer.
    pub hover: String,
    /// The cursor row in the pane with the keyboard (translucent).
    pub selection: String,
    /// The cursor row in the other pane.
    pub selection_inactive: String,
    /// A marked row's name.
    pub marked: String,
    /// Behind a marked row (translucent).
    pub marked_background: String,
    /// Secondary text made legible on the canvas: sizes, dates.
    pub muted: String,
    /// A folder's icon.
    pub folder: String,
    /// An executable's icon.
    pub executable: String,
}

/// The light (`dark: false`) or dark themes, in catalogue order; the first
/// is the default.
#[uniffi::export]
pub fn theme_choices(dark: bool) -> Vec<ThemeChoice> {
    tl_themes::choices(dark)
        .into_iter()
        .map(|(name, scheme)| ThemeChoice {
            name: name.to_string(),
            scheme: scheme.to_string(),
        })
        .collect()
}

/// The colours of a theme; `None` for "Default Light" / "Default Dark"
/// (and for a name the catalogue does not have, which falls back to the
/// default): draw those in the system's colours.
#[uniffi::export]
pub fn palette(name: String) -> Option<ThemePalette> {
    let p = tl_themes::palette(&name)?;
    let list = p.list();
    Some(ThemePalette {
        name: p.name.to_string(),
        dark: p.dark,
        background: p.bg.to_string(),
        chrome: p.chrome.to_string(),
        foreground: p.fg.to_string(),
        dim: p.dim.to_string(),
        border: p.border.to_string(),
        accent: p.accent.to_string(),
        red: p.red.to_string(),
        green: p.green.to_string(),
        yellow: p.yellow.to_string(),
        blue: p.blue.to_string(),
        magenta: p.magenta.to_string(),
        cyan: p.cyan.to_string(),
        stripe: list.stripe,
        hover: list.hover,
        selection: list.selection,
        selection_inactive: list.selection_inactive,
        marked: list.marked,
        marked_background: list.marked_background,
        muted: list.dim,
        folder: list.folder,
        executable: list.executable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_but_the_defaults_has_a_palette() {
        for dark in [false, true] {
            let choices = theme_choices(dark);
            assert_eq!(choices.len(), 10);
            assert_eq!(choices[0].scheme, "Default");
            assert!(palette(choices[0].name.clone()).is_none(), "the default is the system's");
            for choice in &choices[1..] {
                let p = palette(choice.name.clone()).unwrap_or_else(|| panic!("{}", choice.name));
                assert_eq!(p.dark, dark, "{}", p.name);
                assert!(p.background.starts_with('#') && p.background.len() == 7);
                assert_eq!(p.selection.len(), 9);
            }
        }
        assert_eq!(palette("Nord".into()).unwrap().background, "#2e3440");
        assert!(palette("No Such Theme".into()).is_none());
    }
}
