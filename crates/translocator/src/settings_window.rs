//! The settings window: General, Keys and Themes.
//!
//! Every control writes straight through to the settings file and takes
//! effect at once — there is no OK button to forget to press — and each one
//! can be put back to its default from the page's reset button.

use std::collections::BTreeMap;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::setting::{
    NumberFieldOptions, RenderOptions, SettingField, SettingGroup, SettingItem, SettingPage,
    Settings as SettingsPanel,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, TitleBar, h_flex, v_flex,
};
use gpui_kit::{prelude::*, *};

use crate::actions::OpenSettings;
use crate::app_state::AppState;
use crate::keymap::{self, Command};
use crate::pane::SortKey;
use crate::settings::{self, AppearanceMode, DateFormat, FONT_SIZE, ROW_HEIGHT};
use crate::themes;

/// Opens the settings window, or brings the open one forward.
pub fn open(cx: &mut App) {
    if let Some(handle) = cx.global::<AppState>().settings_window {
        if handle.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return;
        }
        // The window is gone without having said so; open a new one.
        cx.global_mut::<AppState>().settings_window = None;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(900.), px(640.)), cx)),
        window_min_size: Some(size(px(620.), px(420.))),
        titlebar: Some(TitlebarOptions {
            title: Some("Settings".into()),
            ..Default::default()
        }),
        ..TitleBar::window_options()
    };
    match gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| SettingsView::new(window, cx))) {
        Ok((handle, _)) => cx.global_mut::<AppState>().settings_window = Some(handle),
        Err(err) => log::error!("cannot open the settings window: {err}"),
    }
}

/// Recording in progress: the command waiting for a key, and the hook that
/// takes keystrokes before anything else in the application can act on them.
struct Recording {
    command: &'static str,
    _interceptor: Subscription,
}

pub struct SettingsView {
    focus: FocusHandle,
    recording: Option<Recording>,
    /// A note under one command's row: a clash, or a key that cannot be used.
    note: Option<(&'static str, SharedString)>,
    _subs: Vec<Subscription>,
}

impl SettingsView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subs = vec![cx.observe_window_appearance(window, |_, window, cx| {
            themes::apply(Some(window), cx);
        })];
        SettingsView {
            focus: cx.focus_handle(),
            recording: None,
            note: None,
            _subs: subs,
        }
    }

    // ----- keys -----------------------------------------------------------

    /// Starts listening for a key combination for `command`.
    ///
    /// The hook is an application-wide keystroke interceptor, which is the
    /// only place a key can be caught before the key map turns it into an
    /// action: without it, recording ⌘Q would quit.
    fn record(&mut self, command: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.note = None;
        let handle = window.window_handle();
        let this = cx.weak_entity();
        let interceptor = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() != handle {
                return;
            }
            let keystroke = event.keystroke.clone();
            // A modifier on its own arrives as a keystroke of its own; wait
            // for the key it is held for.
            if matches!(
                keystroke.key.as_str(),
                "shift" | "control" | "alt" | "platform" | "function" | ""
            ) {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                // Checked here rather than by dropping the hook alone: a
                // hook that outlived its recording and still swallowed
                // keystrokes would leave the window unable to type.
                if this.recording.is_none() {
                    return;
                }
                cx.stop_propagation();
                this.recorded(&keystroke, window, cx);
            });
        });
        window.focus(&self.focus, cx);
        self.recording = Some(Recording {
            command,
            _interceptor: interceptor,
        });
        cx.notify();
    }

    /// A key combination arrived while recording.
    fn recorded(&mut self, keystroke: &Keystroke, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(recording) = self.recording.take() else { return };
        cx.notify();
        let Some(command) = keymap::command(recording.command) else { return };
        let keys = keystroke.unparse();
        if keystroke.key == "escape" && !keystroke.modifiers.modified() {
            return;
        }
        if let Err(err) = keymap::validate(&keys) {
            self.note = Some((command.id, err.into()));
            return;
        }
        let clashes = keymap::conflicts(&keys, command, cx);
        self.note = (!clashes.is_empty()).then(|| {
            let names: Vec<&str> = clashes.iter().map(|c| c.label).collect();
            (
                command.id,
                format!("{} is also {}.", keymap::pretty(&keys), names.join(", ")).into(),
            )
        });
        let id = command.id.to_string();
        let defaults = command.defaults;
        settings::update(cx, |settings| {
            let bound = settings
                .keys
                .entry(id)
                .or_insert_with(|| defaults.iter().map(|k| k.to_string()).collect());
            if !bound.iter().any(|k| *k == keys) {
                bound.push(keys);
            }
        });
        keymap::apply(cx);
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        self.recording = None;
        cx.notify();
    }

    /// Takes one key away from a command, leaving it with none if it was the
    /// last: a command with no key is still in the menus.
    fn unbind(&mut self, command: &'static Command, keys: &str, cx: &mut Context<Self>) {
        self.note = None;
        let id = command.id.to_string();
        let defaults = command.defaults;
        let keys = keys.to_string();
        settings::update(cx, |settings| {
            let bound = settings
                .keys
                .entry(id)
                .or_insert_with(|| defaults.iter().map(|k| k.to_string()).collect());
            bound.retain(|k| *k != keys);
        });
        keymap::apply(cx);
        cx.notify();
    }

    fn reset_command(&mut self, command: &'static Command, cx: &mut Context<Self>) {
        self.note = None;
        let id = command.id.to_string();
        settings::update(cx, |settings| {
            settings.keys.remove(&id);
        });
        keymap::apply(cx);
        cx.notify();
    }

    // ----- pages ----------------------------------------------------------

    fn general_page(&self) -> SettingPage {
        let sort_options = [
            (SortKey::Name, "Name"),
            (SortKey::Ext, "Extension"),
            (SortKey::Size, "Size"),
            (SortKey::Modified, "Date modified"),
        ];
        SettingPage::new("General")
            .icon(Lucide::Settings)
            .description("How the panes list files, and what the commands ask before they act.")
            .group(
                SettingGroup::new()
                    .title("Startup")
                    .item(
                        SettingItem::new(
                            "Reopen the last folders",
                            SettingField::switch(
                                |cx| general(cx).restore_folders,
                                |on, cx| settings::update(cx, |s| s.general.restore_folders = on),
                            )
                            .default_value(true),
                        )
                        .description("Start where the panes were when you quit, instead of in your home folder.")
                        .keywords(["session", "folders", "restore"]),
                    ),
            )
            .group(
                SettingGroup::new()
                    .title("File list")
                    .item(
                        SettingItem::new(
                            "Show hidden files",
                            SettingField::switch(
                                |cx| general(cx).show_hidden,
                                |on, cx| {
                                    settings::update(cx, |s| s.general.show_hidden = on);
                                },
                            )
                            .default_value(false),
                        )
                        .description("Names that begin with a dot. ⌘⇧. toggles it for this session too.")
                        .keywords(["dotfiles", "hidden"]),
                    )
                    .item(
                        SettingItem::new(
                            "Folders first",
                            SettingField::switch(
                                |cx| general(cx).folders_first,
                                |on, cx| settings::update(cx, |s| s.general.folders_first = on),
                            )
                            .default_value(true),
                        )
                        .description("Keep folders above files whatever the list is sorted by.")
                        .keywords(["sort", "directories"]),
                    )
                    .item(
                        SettingItem::new(
                            "Sort new tabs by",
                            SettingField::dropdown(
                                sort_options
                                    .iter()
                                    .map(|(key, label)| (key.id().into(), (*label).into()))
                                    .collect(),
                                |cx| general(cx).sort.id().into(),
                                |value, cx| {
                                    let key = SortKey::from_id(&value);
                                    settings::update(cx, |s| s.general.sort = key);
                                },
                            )
                            .default_value(SortKey::Name.id()),
                        )
                        .description("Clicking a column header still sorts one tab by hand.")
                        .keywords(["order", "column"]),
                    )
                    .item(
                        SettingItem::new(
                            "Largest or newest first",
                            SettingField::switch(
                                |cx| general(cx).sort_descending,
                                |on, cx| settings::update(cx, |s| s.general.sort_descending = on),
                            )
                            .default_value(false),
                        )
                        .keywords(["sort", "descending", "reverse"]),
                    )
                    .item(
                        SettingItem::new(
                            "File icons",
                            SettingField::switch(
                                |cx| general(cx).show_icons,
                                |on, cx| settings::update(cx, |s| s.general.show_icons = on),
                            )
                            .default_value(true),
                        )
                        .description("Without them a row is all name, which fits more of a long one.")
                        .keywords(["icons"]),
                    )
                    .item(
                        SettingItem::new(
                            "Dates",
                            SettingField::dropdown(
                                DateFormat::ALL
                                    .iter()
                                    .map(|f| (f.id().into(), f.label().into()))
                                    .collect(),
                                |cx| general(cx).date_format.id().into(),
                                |value, cx| {
                                    let format = DateFormat::from_id(&value);
                                    settings::update(cx, |s| s.general.date_format = format);
                                },
                            )
                            .default_value(DateFormat::Iso.id()),
                        )
                        .keywords(["date", "time", "format"]),
                    )
                    .item(
                        SettingItem::new(
                            "Text size",
                            SettingField::number_input(
                                NumberFieldOptions {
                                    min: FONT_SIZE.0 as f64,
                                    max: FONT_SIZE.1 as f64,
                                    step: 1.,
                                },
                                |cx| general(cx).font_size() as f64,
                                |value, cx| {
                                    settings::update(cx, |s| s.general.font_size = value as f32);
                                    themes::tweaks(cx);
                                },
                            )
                            .default_value(13.),
                        )
                        .description("Pixels. Everything in the window follows it.")
                        .keywords(["font", "size", "zoom"]),
                    )
                    .item(
                        SettingItem::new(
                            "Row height",
                            SettingField::number_input(
                                NumberFieldOptions {
                                    min: ROW_HEIGHT.0 as f64,
                                    max: ROW_HEIGHT.1 as f64,
                                    step: 1.,
                                },
                                |cx| general(cx).row_height() as f64,
                                |value, cx| {
                                    settings::update(cx, |s| s.general.row_height = value as f32);
                                },
                            )
                            .default_value(22.),
                        )
                        .description("Pixels. Lower fits more files on the screen.")
                        .keywords(["density", "compact", "spacing"]),
                    ),
            )
            .group(
                SettingGroup::new()
                    .title("Safety")
                    .item(
                        SettingItem::new(
                            "Ask before moving to Trash",
                            SettingField::switch(
                                |cx| general(cx).confirm_trash,
                                |on, cx| settings::update(cx, |s| s.general.confirm_trash = on),
                            )
                            .default_value(true),
                        )
                        .description("Deleting permanently always asks, whatever this says.")
                        .keywords(["confirm", "delete", "trash"]),
                    ),
            )
            .group(
                SettingGroup::new()
                    .title("Tools")
                    .item(
                        SettingItem::new(
                            "Terminal",
                            SettingField::input(
                                |cx| general(cx).terminal.clone().into(),
                                |value, cx| {
                                    settings::update(cx, |s| s.general.terminal = value.trim().to_string());
                                },
                            )
                            .default_value(""),
                        )
                        .description(terminal_help())
                        .layout(Axis::Vertical)
                        .keywords(["terminal", "shell", "console"]),
                    ),
            )
    }

    fn keys_page(&self, cx: &mut Context<Self>) -> SettingPage {
        let this = cx.weak_entity();
        // The page's own reset button puts every command it is showing back
        // to its defaults, through the handler on each row.
        let mut page = SettingPage::new("Keys")
            .icon(Lucide::Keyboard)
            .description("Press the plus to add a key combination, or the cross on one to take it away. Search above to find a command.");
        for group in keymap::groups() {
            let mut section = SettingGroup::new().title(group);
            for command in keymap::COMMANDS.iter().filter(|c| c.group == group) {
                let row_this = this.clone();
                section = section.item(
                    SettingItem::render(move |_, _, cx| key_row(command, row_this.clone(), cx))
                    .keywords(search_words(command))
                    .on_reset(
                        move |cx| command.is_changed(&cx.global::<AppState>().settings.keys),
                        {
                            let this = this.clone();
                            move |_, cx| {
                                let _ = this.update(cx, |this, cx| this.reset_command(command, cx));
                            }
                        },
                    ),
                );
            }
            page = page.group(section);
        }
        page
    }

    fn themes_page(&self) -> SettingPage {
        SettingPage::new("Themes")
            .icon(Lucide::Palette)
            .description("Ten colour schemes, each in a light and a dark variant. The one in use changes with the system, or stays as you set it.")
            .group(
                SettingGroup::new()
                    .title("Appearance")
                    .item(
                        SettingItem::new(
                            "Use",
                            SettingField::dropdown(
                                AppearanceMode::ALL
                                    .iter()
                                    .map(|m| (m.id().into(), m.label().into()))
                                    .collect(),
                                |cx| appearance(cx).mode.id().into(),
                                |value, cx| {
                                    let mode = AppearanceMode::from_id(&value);
                                    settings::update(cx, |s| s.appearance.mode = mode);
                                    themes::apply(None, cx);
                                },
                            )
                            .default_value(AppearanceMode::System.id()),
                        )
                        .description("“System” follows the desktop's light and dark setting.")
                        .keywords(["dark", "light", "mode", "appearance"]),
                    )
                    .item(
                        SettingItem::new(
                            "Light theme",
                            theme_dropdown(false),
                        )
                        .description("Used while the appearance is light.")
                        .keywords(["theme", "light", "colors", "scheme"]),
                    )
                    .item(
                        SettingItem::new(
                            "Dark theme",
                            theme_dropdown(true),
                        )
                        .description("Used while the appearance is dark.")
                        .keywords(["theme", "dark", "colors", "scheme"]),
                    ),
            )
            .group(
                SettingGroup::new()
                    .title("Preview")
                    .item(SettingItem::render(preview).keywords(["preview", "sample"])),
            )
    }
}

fn general(cx: &App) -> crate::settings::General {
    cx.global::<AppState>().settings.general.clone()
}

fn appearance(cx: &App) -> crate::settings::Appearance {
    cx.global::<AppState>().settings.appearance.clone()
}

fn terminal_help() -> &'static str {
    if cfg!(target_os = "macos") {
        "A command to run instead of Terminal.app, for example `open -a iTerm`. Empty uses Terminal.app."
    } else {
        "A command to run instead of the desktop's terminal, for example `kitty`. Empty uses $TERMINAL and then whatever is installed."
    }
}

/// Words the settings search matches a command on: its name, its group and
/// the keys it answers to, so both "copy" and "F5" find the right row.
fn search_words(command: &'static Command) -> Vec<SharedString> {
    let mut words: Vec<SharedString> = vec![command.label.into(), command.group.into()];
    for keys in command.defaults {
        words.push((*keys).into());
        words.push(keymap::pretty(keys).into());
    }
    words
}

fn theme_dropdown(dark: bool) -> SettingField<SharedString> {
    let options: Vec<(SharedString, SharedString)> = themes::choices(dark)
        .into_iter()
        .map(|(theme, scheme)| (theme.into(), scheme.into()))
        .collect();
    let default = if dark { themes::DEFAULT_DARK } else { themes::DEFAULT_LIGHT };
    SettingField::scrollable_dropdown(
        options,
        move |cx| {
            let appearance = appearance(cx);
            if dark { appearance.dark.into() } else { appearance.light.into() }
        },
        move |value, cx| {
            let name = value.to_string();
            settings::update(cx, |s| {
                if dark {
                    s.appearance.dark = name;
                } else {
                    s.appearance.light = name;
                }
            });
            themes::apply(None, cx);
        },
    )
    .default_value(default)
}

/// One command and its keys.
fn key_row(
    command: &'static Command,
    this: WeakEntity<SettingsView>,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme().clone();
    let state = this.upgrade().map(|view| view.read(cx));
    let recording = state
        .as_ref()
        .and_then(|view| view.recording.as_ref())
        .is_some_and(|r| r.command == command.id);
    let note = state
        .as_ref()
        .and_then(|view| view.note.clone())
        .filter(|(id, _)| *id == command.id)
        .map(|(_, text)| text);
    let overrides: BTreeMap<String, Vec<String>> = cx.global::<AppState>().settings.keys.clone();
    let keys: Vec<String> = command.keys(&overrides).iter().map(|k| k.to_string()).collect();
    let changed = command.is_changed(&overrides);

    v_flex()
        .w_full()
        .gap_1()
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .justify_between()
                .child(div().flex_1().min_w_0().text_sm().truncate().child(command.label))
                .child(
                    h_flex()
                        .gap_1()
                        .when(recording, |el| {
                            let this = this.clone();
                            el.child(
                                h_flex()
                                    .gap_1()
                                    .px_2()
                                    .py_0p5()
                                    .rounded(theme.radius)
                                    .border_1()
                                    .border_color(theme.ring)
                                    .bg(theme.list_active)
                                    .text_xs()
                                    .child("Press a key combination…"),
                            )
                            .child(
                                Button::new(button_id("key-stop", command.id))
                                    .label("Cancel")
                                    .ghost()
                                    .xsmall()
                                    .on_click(move |_, _, cx| {
                                        let _ = this.update(cx, |this, cx| this.stop_recording(cx));
                                    }),
                            )
                        })
                        .when(!recording, |el| {
                            el.children(keys.iter().enumerate().map(|(ix, keys)| {
                                let this = this.clone();
                                let keys = keys.clone();
                                h_flex()
                                    .gap_1()
                                    .pl_1p5()
                                    .pr_0p5()
                                    .py_0p5()
                                    .rounded(theme.radius)
                                    .border_1()
                                    .border_color(theme.border)
                                    .bg(theme.secondary)
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_family(theme.mono_font_family.clone())
                                            .child(keymap::pretty(&keys)),
                                    )
                                    .child(
                                        Button::new(button_id(&format!("key-del-{ix}"), command.id))
                                            .icon(Lucide::Close)
                                            .ghost()
                                            .xsmall()
                                            .tooltip("Take this key away")
                                            .on_click({
                                                let this = this.clone();
                                                move |_, _, cx| {
                                                    let keys = keys.clone();
                                                    let _ = this.update(cx, |this, cx| {
                                                        this.unbind(command, &keys, cx)
                                                    });
                                                }
                                            }),
                                    )
                            }))
                            .when(keys.is_empty(), |el| {
                                el.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("no key"),
                                )
                            })
                            .child({
                                let this = this.clone();
                                Button::new(button_id("key-add", command.id))
                                    .icon(Lucide::Plus)
                                    .ghost()
                                    .xsmall()
                                    .tooltip("Add a key combination")
                                    .on_click(move |_, window, cx| {
                                        let _ = this.update(cx, |this, cx| {
                                            this.record(command.id, window, cx)
                                        });
                                    })
                            })
                            .child({
                                let this = this.clone();
                                Button::new(button_id("key-reset", command.id))
                                    .icon(Lucide::Undo2)
                                    .ghost()
                                    .xsmall()
                                    .disabled(!changed)
                                    .tooltip("Back to the default")
                                    .on_click(move |_, _, cx| {
                                        let _ = this.update(cx, |this, cx| {
                                            this.reset_command(command, cx)
                                        });
                                    })
                            })
                        }),
                ),
        )
        .when_some(note, |el, note| {
            el.child(
                h_flex()
                    .gap_1()
                    .text_xs()
                    .text_color(theme.warning)
                    .child(Icon::new(Lucide::TriangleAlert).xsmall())
                    .child(note),
            )
        })
        .into_any_element()
}

/// One element id per button: a tuple of two strings is not one.
fn button_id(what: &str, command: &'static str) -> SharedString {
    format!("{what}-{command}").into()
}

/// A few rows painted with the live theme, so the effect of a change can be
/// seen without closing the window.
fn preview(_: &RenderOptions, _: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    let row = |name: &'static str, size: &'static str, icon: Lucide, selected: bool, marked: bool| {
        h_flex()
            .h(px(22.))
            .px_2()
            .gap_2()
            .text_size(px(12.))
            .when(selected, |el| {
                el.bg(theme.list_active).border_1().border_color(theme.list_active_border)
            })
            .when(!selected, |el| el.border_1().border_color(theme.transparent))
            .child(
                Icon::new(icon)
                    .xsmall()
                    .text_color(if marked { theme.danger } else { theme.muted_foreground }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if marked { theme.danger } else { theme.foreground })
                    .child(name),
            )
            .child(div().text_color(theme.muted_foreground).child(size))
    };
    v_flex()
        .w_full()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .overflow_hidden()
        .child(
            h_flex()
                .h(px(24.))
                .px_2()
                .gap_2()
                .bg(theme.title_bar)
                .border_b_1()
                .border_color(theme.title_bar_border)
                .text_xs()
                .child(div().font_weight(FontWeight::SEMIBOLD).child("Translocator"))
                .child(div().flex_1())
                .child(div().text_color(theme.muted_foreground).child("~/Projects")),
        )
        .child(
            h_flex()
                .h(px(20.))
                .px_2()
                .gap_2()
                .bg(theme.list_head)
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(div().flex_1().child("Name"))
                .child("Size"),
        )
        .child(row("..", "", Lucide::Folder, false, false))
        .child(row("translocator", "folder", Lucide::Folder, true, false))
        .child(row("README.md", "2.1 kB", Lucide::FileText, false, false))
        .child(
            div()
                .bg(theme.list_even)
                .child(row("install.sh", "870 B", Lucide::FileTerminal, false, true)),
        )
        .child(
            h_flex()
                .h(px(22.))
                .px_2()
                .gap_2()
                .bg(theme.status_bar)
                .border_t_1()
                .border_color(theme.status_bar_border)
                .text_xs()
                .child(div().text_color(theme.danger).child("1 marked"))
                .child(div().flex_1())
                .child(div().text_color(theme.success).child("12 GB free")),
        )
        .into_any_element()
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let general = self.general_page();
        let keys = self.keys_page(cx);
        let themes = self.themes_page();
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(cx.listener(|_, _: &OpenSettings, window, _| window.activate_window()))
            .child(TitleBar::new().child(
                div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child("Settings"),
            ))
            .child(
                div().flex_1().min_h_0().child(
                    SettingsPanel::new("settings")
                        .sidebar_width(px(190.))
                        .small()
                        .pages([general, keys, themes]),
                ),
            )
    }
}
