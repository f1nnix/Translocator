//! The theme catalogue: ten colour schemes, each in a light and a dark
//! variant, plus the glue that puts the chosen pair in front of the window.
//!
//! A theme in `gpui-component` is a hundred named colours. Writing those out
//! twenty times would be twenty chances to get one wrong, so each scheme is
//! given here as the dozen colours its authors actually publish — canvas,
//! text, border, the accent and the six hues — and the rest is derived:
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

use crate::app_state::AppState;
use crate::settings::AppearanceMode;

/// The scheme a fresh installation starts in, and the name an unknown theme
/// in the settings file falls back to. Both ship with the component library.
pub const DEFAULT_LIGHT: &str = "Default Light";
pub const DEFAULT_DARK: &str = "Default Dark";

/// One scheme, as the settings window lists it.
pub struct Scheme {
    pub name: &'static str,
    pub light: &'static str,
    pub dark: &'static str,
}

/// The schemes, in the order the dropdowns show them.
pub const SCHEMES: &[Scheme] = &[
    Scheme { name: "Default", light: DEFAULT_LIGHT, dark: DEFAULT_DARK },
    Scheme { name: "Catppuccin", light: "Catppuccin Latte", dark: "Catppuccin Mocha" },
    Scheme { name: "Dracula", light: "Alucard", dark: "Dracula" },
    Scheme { name: "GitHub", light: "GitHub Light", dark: "GitHub Dark" },
    Scheme { name: "Gruvbox", light: "Gruvbox Light", dark: "Gruvbox Dark" },
    Scheme { name: "Monokai", light: "Monokai Light", dark: "Monokai" },
    Scheme { name: "Nord", light: "Nord Light", dark: "Nord" },
    Scheme { name: "One", light: "One Light", dark: "One Dark" },
    Scheme { name: "Solarized", light: "Solarized Light", dark: "Solarized Dark" },
    Scheme { name: "Tokyo Night", light: "Tokyo Night Day", dark: "Tokyo Night" },
];

/// The colours a scheme's authors publish. Everything else is derived from
/// these, so adding a scheme is one line per colour.
struct Palette {
    name: &'static str,
    dark: bool,
    /// The canvas: window and file-list background.
    bg: &'static str,
    /// Raised surfaces: title bar, tab bar, status line, popovers, cards.
    chrome: &'static str,
    /// Body text.
    fg: &'static str,
    /// Secondary text: comments, sizes, dates, placeholder text.
    dim: &'static str,
    border: &'static str,
    /// The scheme's signature colour: cursor row, focus ring, primary button.
    accent: &'static str,
    red: &'static str,
    green: &'static str,
    yellow: &'static str,
    blue: &'static str,
    magenta: &'static str,
    cyan: &'static str,
}

/// The nine schemes built here; "Default" comes from the component library.
const PALETTES: &[Palette] = &[
    // https://github.com/catppuccin/catppuccin — Latte and Mocha.
    Palette {
        name: "Catppuccin Latte",
        dark: false,
        bg: "#eff1f5",
        chrome: "#e6e9ef",
        fg: "#4c4f69",
        dim: "#8c8fa1",
        border: "#ccd0da",
        accent: "#8839ef",
        red: "#d20f39",
        green: "#40a02b",
        yellow: "#df8e1d",
        blue: "#1e66f5",
        magenta: "#ea76cb",
        cyan: "#179299",
    },
    Palette {
        name: "Catppuccin Mocha",
        dark: true,
        bg: "#1e1e2e",
        chrome: "#181825",
        fg: "#cdd6f4",
        dim: "#9399b2",
        border: "#313244",
        accent: "#cba6f7",
        red: "#f38ba8",
        green: "#a6e3a1",
        yellow: "#f9e2af",
        blue: "#89b4fa",
        magenta: "#f5c2e7",
        cyan: "#94e2d5",
    },
    // https://draculatheme.com — Dracula, and its light companion Alucard.
    Palette {
        name: "Alucard",
        dark: false,
        bg: "#fffbeb",
        chrome: "#f5f1e1",
        fg: "#1f1f1f",
        dim: "#6c664b",
        border: "#dedabf",
        accent: "#644ac9",
        red: "#cb3a2a",
        green: "#14710a",
        yellow: "#846e15",
        blue: "#036a96",
        magenta: "#a3144d",
        cyan: "#036a96",
    },
    Palette {
        name: "Dracula",
        dark: true,
        bg: "#282a36",
        chrome: "#21222c",
        fg: "#f8f8f2",
        dim: "#6272a4",
        border: "#44475a",
        accent: "#bd93f9",
        red: "#ff5555",
        green: "#50fa7b",
        yellow: "#f1fa8c",
        blue: "#8be9fd",
        magenta: "#ff79c6",
        cyan: "#8be9fd",
    },
    // https://primer.style — GitHub's Primer colours.
    Palette {
        name: "GitHub Light",
        dark: false,
        bg: "#ffffff",
        chrome: "#f6f8fa",
        fg: "#1f2328",
        dim: "#656d76",
        border: "#d0d7de",
        accent: "#0969da",
        red: "#cf222e",
        green: "#1a7f37",
        yellow: "#9a6700",
        blue: "#0969da",
        magenta: "#8250df",
        cyan: "#1b7c83",
    },
    Palette {
        name: "GitHub Dark",
        dark: true,
        bg: "#0d1117",
        chrome: "#161b22",
        fg: "#e6edf3",
        dim: "#8b949e",
        border: "#30363d",
        accent: "#58a6ff",
        red: "#f85149",
        green: "#3fb950",
        yellow: "#d29922",
        blue: "#58a6ff",
        magenta: "#a371f7",
        cyan: "#39c5cf",
    },
    // https://github.com/morhetz/gruvbox — the medium contrast pair.
    Palette {
        name: "Gruvbox Light",
        dark: false,
        bg: "#fbf1c7",
        chrome: "#f2e5bc",
        fg: "#3c3836",
        dim: "#7c6f64",
        border: "#e3d7ae",
        accent: "#af3a03",
        red: "#9d0006",
        green: "#79740e",
        yellow: "#b57614",
        blue: "#076678",
        magenta: "#8f3f71",
        cyan: "#427b58",
    },
    Palette {
        name: "Gruvbox Dark",
        dark: true,
        bg: "#282828",
        chrome: "#32302f",
        fg: "#ebdbb2",
        dim: "#a89984",
        border: "#504945",
        accent: "#fe8019",
        red: "#fb4934",
        green: "#b8bb26",
        yellow: "#fabd2f",
        blue: "#83a598",
        magenta: "#d3869b",
        cyan: "#8ec07c",
    },
    // https://monokai.pro — the original scheme; the light variant is built
    // from its hues over the lightest background of the family.
    Palette {
        name: "Monokai Light",
        dark: false,
        bg: "#fafaf5",
        chrome: "#f0f0e6",
        fg: "#272822",
        dim: "#75715e",
        border: "#e0e0d2",
        accent: "#c6175c",
        red: "#f92672",
        green: "#8bb020",
        yellow: "#b39b00",
        blue: "#1a9cb0",
        magenta: "#ae81ff",
        cyan: "#1a9cb0",
    },
    Palette {
        name: "Monokai",
        dark: true,
        bg: "#272822",
        chrome: "#1e1f1c",
        fg: "#f8f8f2",
        dim: "#908e7f",
        border: "#49483e",
        accent: "#f92672",
        red: "#f92672",
        green: "#a6e22e",
        yellow: "#e6db74",
        blue: "#66d9ef",
        magenta: "#ae81ff",
        cyan: "#66d9ef",
    },
    // https://www.nordtheme.com — Polar Night and Snow Storm.
    Palette {
        name: "Nord Light",
        dark: false,
        bg: "#eceff4",
        chrome: "#e5e9f0",
        fg: "#2e3440",
        dim: "#5d6b84",
        border: "#d8dee9",
        accent: "#5e81ac",
        red: "#bf616a",
        green: "#a3be8c",
        yellow: "#ebcb8b",
        blue: "#81a1c1",
        magenta: "#b48ead",
        cyan: "#8fbcbb",
    },
    Palette {
        name: "Nord",
        dark: true,
        bg: "#2e3440",
        chrome: "#3b4252",
        fg: "#eceff4",
        dim: "#8d99ae",
        border: "#434c5e",
        accent: "#88c0d0",
        red: "#bf616a",
        green: "#a3be8c",
        yellow: "#ebcb8b",
        blue: "#81a1c1",
        magenta: "#b48ead",
        cyan: "#8fbcbb",
    },
    // https://github.com/atom/one-light-syntax and one-dark-syntax.
    Palette {
        name: "One Light",
        dark: false,
        bg: "#fafafa",
        chrome: "#eaeaeb",
        fg: "#383a42",
        dim: "#8e8f94",
        border: "#dcdcdd",
        accent: "#4078f2",
        red: "#e45649",
        green: "#50a14f",
        yellow: "#c18401",
        blue: "#4078f2",
        magenta: "#a626a4",
        cyan: "#0184bc",
    },
    Palette {
        name: "One Dark",
        dark: true,
        bg: "#282c34",
        chrome: "#21252b",
        fg: "#abb2bf",
        dim: "#7f8694",
        border: "#3e4451",
        accent: "#61afef",
        red: "#e06c75",
        green: "#98c379",
        yellow: "#e5c07b",
        blue: "#61afef",
        magenta: "#c678dd",
        cyan: "#56b6c2",
    },
    // https://ethanschoonover.com/solarized — the canonical pair.
    Palette {
        name: "Solarized Light",
        dark: false,
        bg: "#fdf6e3",
        chrome: "#eee8d5",
        fg: "#586e75",
        dim: "#839496",
        border: "#e0dac3",
        accent: "#268bd2",
        red: "#dc322f",
        green: "#859900",
        yellow: "#b58900",
        blue: "#268bd2",
        magenta: "#d33682",
        cyan: "#2aa198",
    },
    Palette {
        name: "Solarized Dark",
        dark: true,
        bg: "#002b36",
        chrome: "#073642",
        fg: "#93a1a1",
        dim: "#7a8e8e",
        border: "#0f4653",
        accent: "#268bd2",
        red: "#dc322f",
        green: "#859900",
        yellow: "#b58900",
        blue: "#268bd2",
        magenta: "#d33682",
        cyan: "#2aa198",
    },
    // https://github.com/folke/tokyonight.nvim — night and day.
    Palette {
        name: "Tokyo Night Day",
        dark: false,
        bg: "#e1e2e7",
        chrome: "#d4d6e4",
        fg: "#3760bf",
        dim: "#7c82a6",
        border: "#c4c8da",
        accent: "#2e7de9",
        red: "#f52a65",
        green: "#587539",
        yellow: "#8c6c3e",
        blue: "#2e7de9",
        magenta: "#9854f1",
        cyan: "#007197",
    },
    Palette {
        name: "Tokyo Night",
        dark: true,
        bg: "#1a1b26",
        chrome: "#16161e",
        fg: "#c0caf5",
        dim: "#7982a9",
        border: "#292e42",
        accent: "#7aa2f7",
        red: "#f7768e",
        green: "#9ece6a",
        yellow: "#e0af68",
        blue: "#7aa2f7",
        magenta: "#bb9af7",
        cyan: "#7dcfff",
    },
];

// ----- colour arithmetic ---------------------------------------------------

/// `#rrggbb` as three bytes. An unparsable colour is black, which no palette
/// here produces: the tests check every one of them round-trips.
fn rgb(hex: &str) -> [f32; 3] {
    let hex = hex.trim_start_matches('#');
    let byte = |i: usize| {
        u8::from_str_radix(hex.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0) as f32 / 255.
    };
    [byte(0), byte(2), byte(4)]
}

fn hex(c: [f32; 3]) -> String {
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}", byte(c[0]), byte(c[1]), byte(c[2]))
}

/// `a` with `t` of `b` mixed into it.
fn mix(a: &str, b: &str, t: f32) -> String {
    let (a, b) = (rgb(a), rgb(b));
    hex([
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ])
}

/// `color` at `a` opacity, as the eight-digit hex the theme format takes.
fn alpha(color: &str, a: f32) -> String {
    format!("{}{:02x}", hex(rgb(color)), (a.clamp(0., 1.) * 255.).round() as u8)
}

fn luminance(c: [f32; 3]) -> f32 {
    let channel = |v: f32| {
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(c[0]) + 0.7152 * channel(c[1]) + 0.0722 * channel(c[2])
}

/// WCAG contrast ratio, 1.0 (identical) to 21.0 (black on white).
fn contrast(a: &str, b: &str) -> f32 {
    let (a, b) = (luminance(rgb(a)), luminance(rgb(b)));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Text that can be read on `bg`: the palette's own canvas or text colour,
/// whichever stands out more. Used for a label on a coloured button.
fn on(bg: &str, palette: &Palette) -> String {
    let light = if palette.dark { palette.fg } else { "#ffffff" };
    let dark = if palette.dark { palette.bg } else { palette.fg };
    if contrast(bg, light) >= contrast(bg, dark) {
        light.to_string()
    } else {
        dark.to_string()
    }
}

/// `color` pushed towards the text colour until it can be read on the
/// canvas. A scheme made for a dark terminal has hues that vanish on white;
/// this keeps the hue and gives up only as much of it as it must.
fn legible(color: &str, palette: &Palette, min: f32) -> String {
    let mut shifted = color.to_string();
    for step in 0..=16 {
        shifted = mix(color, palette.fg, step as f32 / 16.);
        if contrast(&shifted, palette.bg) >= min {
            break;
        }
    }
    shifted
}

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

impl Palette {
    /// The canvas mixed `t` of the way towards the text colour: darker on a
    /// light scheme, lighter on a dark one, always in the right direction.
    fn raise(&self, t: f32) -> String {
        mix(self.bg, self.fg, t)
    }

    /// A hue as text in the file list or the editor.
    fn text(&self, color: &str) -> String {
        legible(color, self, 4.0)
    }

    /// A hue as text where it only has to be noticed, not read at length.
    fn mark(&self, color: &str) -> String {
        legible(color, self, 3.0)
    }

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

/// The light or dark themes, in catalogue order: `(theme name, scheme name)`.
pub fn choices(dark: bool) -> Vec<(&'static str, &'static str)> {
    SCHEMES
        .iter()
        .map(|s| (if dark { s.dark } else { s.light }, s.name))
        .collect()
}

/// Installs the themes the settings name and switches to the mode they ask
/// for. `window` is read for the system appearance, which on Linux is only
/// known per window.
pub fn apply(window: Option<&Window>, cx: &mut App) {
    let settings = cx.global::<AppState>().settings.appearance.clone();
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
    let general = cx.global::<AppState>().settings.general.clone();
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

    #[test]
    fn the_catalogue_is_ten_schemes_in_both_modes() {
        assert_eq!(SCHEMES.len(), 10);
        assert_eq!(choices(false).len(), 10);
        assert_eq!(choices(true).len(), 10);
        // Every name in the catalogue is either built here or built in.
        for scheme in SCHEMES {
            for (name, dark) in [(scheme.light, false), (scheme.dark, true)] {
                let built = PALETTES.iter().find(|p| p.name == name);
                match built {
                    Some(palette) => assert_eq!(palette.dark, dark, "{name}"),
                    None => assert!(
                        name == DEFAULT_LIGHT || name == DEFAULT_DARK,
                        "{name} is neither built here nor a built-in theme"
                    ),
                }
            }
        }
    }

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
    }

    #[test]
    fn every_palette_colour_is_a_hex_colour() {
        for palette in PALETTES {
            for color in [
                palette.bg,
                palette.chrome,
                palette.fg,
                palette.dim,
                palette.border,
                palette.accent,
                palette.red,
                palette.green,
                palette.yellow,
                palette.blue,
                palette.magenta,
                palette.cyan,
            ] {
                assert_eq!(color.len(), 7, "{} has {color}", palette.name);
                assert_eq!(hex(rgb(color)), color, "{} has {color}", palette.name);
            }
        }
    }

    #[test]
    fn text_can_be_read_on_its_background() {
        for palette in PALETTES {
            // The canvas and its text, which everything else is derived
            // from: WCAG AA for body text. Some schemes are published with
            // deliberately low contrast (Tokyo Night Day sits at 4.5), so
            // this is the floor rather than a comfortable margin.
            assert!(
                contrast(palette.bg, palette.fg) >= 4.5,
                "{}: {} on {}",
                palette.name,
                palette.fg,
                palette.bg
            );
            assert!(contrast(palette.bg, palette.chrome) < 1.6, "{}", palette.name);
            // Secondary text and the hues, after the legibility pass.
            for color in [palette.dim, palette.red, palette.green, palette.blue] {
                let text = palette.text(color);
                assert!(
                    contrast(&text, palette.bg) >= 4.,
                    "{}: {color} became {text}",
                    palette.name
                );
            }
            // A label on a coloured button.
            for color in [palette.accent, palette.red, palette.yellow] {
                let label = on(color, palette);
                assert!(
                    contrast(color, &label) >= 3.,
                    "{}: {label} on {color}",
                    palette.name
                );
            }
        }
    }

    #[test]
    fn mixing_moves_towards_the_other_colour() {
        assert_eq!(mix("#000000", "#ffffff", 0.), "#000000");
        assert_eq!(mix("#000000", "#ffffff", 1.), "#ffffff");
        assert_eq!(mix("#000000", "#ffffff", 0.5), "#808080");
        assert_eq!(alpha("#112233", 1.), "#112233ff");
        assert_eq!(alpha("#112233", 0.), "#11223300");
        assert!((contrast("#ffffff", "#000000") - 21.).abs() < 0.01);
    }
}
