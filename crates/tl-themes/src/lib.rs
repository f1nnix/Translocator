//! Translocator's colour schemes, for every shell: ten schemes, each in a
//! light and a dark variant, as the dozen colours their authors publish —
//! canvas, text, border, the accent and the six hues — and the colour maths
//! the rest of a theme is derived from.
//!
//! Derivation, in short: stripes and hover states are the canvas mixed
//! towards the text ([`Palette::raise`]), a colour used as a background gets
//! text that can be read on it ([`on`]), and a hue used as text is pushed
//! towards the text colour until it is legible on the canvas ([`legible`]).
//! That last step is what lets a scheme built for a dark terminal keep its
//! colours in a light window.
//!
//! "Default" is not built here: each shell draws it in its own toolkit's
//! colours (the GPUI component library's built-in theme, the system colours
//! on macOS). Solarized, Gruvbox, One, Catppuccin, GitHub and Tokyo Night are
//! published by their authors in both modes, and Dracula's light companion
//! is its own Alucard. Nord and Monokai are dark schemes: their light
//! variants are built here from the same hues over the family's lightest
//! background.
//!
//! Plain data and arithmetic, no UI toolkit: the GPUI shell turns a
//! [`Palette`] into its theme file, the FFI layer hands it to Swift.

/// The scheme a fresh installation starts in, and the name an unknown theme
/// in the settings file falls back to. The shells draw both themselves.
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
pub struct Palette {
    pub name: &'static str,
    pub dark: bool,
    /// The canvas: window and file-list background.
    pub bg: &'static str,
    /// Raised surfaces: title bar, tab bar, status line, popovers, cards.
    pub chrome: &'static str,
    /// Body text.
    pub fg: &'static str,
    /// Secondary text: comments, sizes, dates, placeholder text.
    pub dim: &'static str,
    pub border: &'static str,
    /// The scheme's signature colour: cursor row, focus ring, primary button.
    pub accent: &'static str,
    pub red: &'static str,
    pub green: &'static str,
    pub yellow: &'static str,
    pub blue: &'static str,
    pub magenta: &'static str,
    pub cyan: &'static str,
}

/// The nine schemes built here; "Default" is each shell's own.
pub const PALETTES: &[Palette] = &[
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

/// `#rrggbb` as three channels in `0.0..=1.0`. An unparsable colour is
/// black, which no palette here produces: the tests check every one of them
/// round-trips.
pub fn rgb(hex: &str) -> [f32; 3] {
    let hex = hex.trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0) as f32 / 255.;
    [byte(0), byte(2), byte(4)]
}

/// Three channels back as `#rrggbb`.
pub fn hex(c: [f32; 3]) -> String {
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}", byte(c[0]), byte(c[1]), byte(c[2]))
}

/// `a` with `t` of `b` mixed into it.
pub fn mix(a: &str, b: &str, t: f32) -> String {
    let (a, b) = (rgb(a), rgb(b));
    hex([
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ])
}

/// `color` at `a` opacity, as eight-digit hex (`#rrggbbaa`).
pub fn alpha(color: &str, a: f32) -> String {
    format!("{}{:02x}", hex(rgb(color)), (a.clamp(0., 1.) * 255.).round() as u8)
}

/// Relative luminance (WCAG), `0.0` for black to `1.0` for white.
pub fn luminance(c: [f32; 3]) -> f32 {
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
pub fn contrast(a: &str, b: &str) -> f32 {
    let (a, b) = (luminance(rgb(a)), luminance(rgb(b)));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Text that can be read on `bg`: the palette's own canvas or text colour,
/// whichever stands out more. Used for a label on a coloured button.
pub fn on(bg: &str, palette: &Palette) -> String {
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
pub fn legible(color: &str, palette: &Palette, min: f32) -> String {
    let mut shifted = color.to_string();
    for step in 0..=16 {
        shifted = mix(color, palette.fg, step as f32 / 16.);
        if contrast(&shifted, palette.bg) >= min {
            break;
        }
    }
    shifted
}

impl Palette {
    /// The canvas mixed `t` of the way towards the text colour: darker on a
    /// light scheme, lighter on a dark one, always in the right direction.
    pub fn raise(&self, t: f32) -> String {
        mix(self.bg, self.fg, t)
    }

    /// A hue as text in the file list or the editor.
    pub fn text(&self, color: &str) -> String {
        legible(color, self, 4.0)
    }

    /// A hue as text where it only has to be noticed, not read at length.
    pub fn mark(&self, color: &str) -> String {
        legible(color, self, 3.0)
    }

    /// The colours of a file list in this scheme, the same the GPUI shell
    /// paints its panes in.
    pub fn list(&self) -> ListColors {
        let blue = self.mark(self.blue);
        ListColors {
            stripe: self.raise(0.035),
            hover: self.raise(0.07),
            selection: alpha(&blue, 0.42),
            selection_inactive: self.raise(0.1),
            marked: self.mark(self.yellow),
            marked_background: alpha(self.yellow, 0.14),
            dim: self.mark(self.dim),
            folder: blue,
            executable: self.mark(self.green),
        }
    }
}

/// What a file list is painted in, derived from a [`Palette`]. Colours are
/// `#rrggbb`, or `#rrggbbaa` where they lie over the canvas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListColors {
    /// Every other row.
    pub stripe: String,
    /// The row under the pointer.
    pub hover: String,
    /// The cursor row in the pane with the keyboard.
    pub selection: String,
    /// The cursor row in the other pane.
    pub selection_inactive: String,
    /// The name of a marked row.
    pub marked: String,
    /// Behind a marked row.
    pub marked_background: String,
    /// Secondary text: sizes, dates, permissions.
    pub dim: String,
    /// A folder's icon.
    pub folder: String,
    /// An executable's icon.
    pub executable: String,
}

// ----- the catalogue -------------------------------------------------------

/// The light or dark themes, in catalogue order: `(theme name, scheme name)`.
pub fn choices(dark: bool) -> Vec<(&'static str, &'static str)> {
    SCHEMES
        .iter()
        .map(|s| (if dark { s.dark } else { s.light }, s.name))
        .collect()
}

/// A theme built here, by its name. `None` for the two defaults, which
/// each shell draws itself, and for names the catalogue does not have.
pub fn palette(name: &str) -> Option<&'static Palette> {
    PALETTES.iter().find(|p| p.name == name)
}

/// Whether the catalogue lists `name`, built here or a default.
pub fn is_known(name: &str) -> bool {
    SCHEMES.iter().any(|s| s.light == name || s.dark == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_is_ten_schemes_in_both_modes() {
        assert_eq!(SCHEMES.len(), 10);
        assert_eq!(choices(false).len(), 10);
        assert_eq!(choices(true).len(), 10);
        // Every name in the catalogue is either built here or a default.
        for scheme in SCHEMES {
            for (name, dark) in [(scheme.light, false), (scheme.dark, true)] {
                match palette(name) {
                    Some(palette) => assert_eq!(palette.dark, dark, "{name}"),
                    None => assert!(
                        name == DEFAULT_LIGHT || name == DEFAULT_DARK,
                        "{name} is neither built here nor a default"
                    ),
                }
                assert!(is_known(name));
            }
        }
        // And every palette is in the catalogue, once.
        for p in PALETTES {
            let listed = SCHEMES.iter().filter(|s| s.light == p.name || s.dark == p.name).count();
            assert_eq!(listed, 1, "{}", p.name);
        }
        assert!(palette(DEFAULT_DARK).is_none());
        assert!(!is_known("Nope"));
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
                assert!(contrast(&text, palette.bg) >= 4., "{}: {color} became {text}", palette.name);
            }
            // A label on a coloured button.
            for color in [palette.accent, palette.red, palette.yellow] {
                let label = on(color, palette);
                assert!(contrast(color, &label) >= 3., "{}: {label} on {color}", palette.name);
            }
        }
    }

    #[test]
    fn list_colours_stand_out_from_the_canvas() {
        for palette in PALETTES {
            let list = palette.list();
            for color in [&list.marked, &list.dim, &list.folder, &list.executable] {
                assert!(contrast(color, palette.bg) >= 3., "{}: {color}", palette.name);
            }
            for color in [&list.stripe, &list.hover, &list.selection_inactive] {
                assert_eq!(color.len(), 7, "{}: {color}", palette.name);
                assert_ne!(color, palette.bg, "{}", palette.name);
            }
            for color in [&list.selection, &list.marked_background] {
                assert_eq!(color.len(), 9, "{}: {color}", palette.name);
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
