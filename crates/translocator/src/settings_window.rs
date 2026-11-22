//! The Settings window: General, Keys and Themes.
//!
//! [`open`] is called by the shell ([`crate::app_state`]) whenever the core
//! bumps `view.settings_requests` (the OpenSettings command: ⌘, the menu,
//! the title bar button). There is only ever one settings window: a live
//! handle in `Shell::settings_window` is activated, a dead one cleared.
//!
//! There is no OK button: every control sends its
//! `Event::Settings(SettingsEdit::..)` at once, and each page's reset button
//! puts what it shows back to the defaults. The settings drawn are the
//! core's, as last rendered; the shell re-applies the key map and the theme
//! when they change. Recording a key combination is this window's own
//! state, and so is the note about a clash or an unusable key.

use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::setting::{
    NumberFieldOptions, RenderOptions, SettingField, SettingGroup, SettingItem, SettingPage,
    Settings as SettingsPanel,
};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, TitleBar, h_flex, v_flex};
use gpui_kit::{prelude::*, *};
use tl_app::Event;
use tl_app::model::SettingsEdit;
use tl_app::settings::{
    Appearance, AppearanceMode, DateFormat, FONT_SIZE, General, ROW_HEIGHT, Settings, SortKey,
};

use crate::actions::OpenSettings;
use crate::app_state::{Shell, dispatch, view};
use crate::keymap::{self, Command};
use crate::themes;

/// Opens the settings window, or brings the open one forward.
pub fn open(cx: &mut App) {
    if let Some(handle) = cx.global::<Shell>().settings_window {
        if handle.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return;
        }
        // The window is gone without having said so; open a new one.
        cx.global_mut::<Shell>().settings_window = None;
    }
    let mut chrome = TitleBar::window_options();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(900.), px(640.)), cx)),
        window_min_size: Some(size(px(620.), px(420.))),
        // The title bar drawn in the window, with "Settings" as the title the
        // system shows (the window menu, the switcher).
        titlebar: Some(TitlebarOptions {
            title: Some("Settings".into()),
            ..chrome.titlebar.take().unwrap_or_default()
        }),
        ..chrome
    };
    match gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| SettingsView::new(window, cx))) {
        Ok((handle, _)) => cx.global_mut::<Shell>().settings_window = Some(handle),
        Err(err) => log::error!("cannot open the settings window: {err}"),
    }
}

fn edit(edit: SettingsEdit, cx: &App) {
    dispatch(Event::Settings(edit), cx);
}

fn settings(cx: &App) -> Arc<Settings> {
    view(cx).settings.clone()
}

fn general(cx: &App) -> General {
    settings(cx).general.clone()
}

fn appearance(cx: &App) -> Appearance {
    settings(cx).appearance.clone()
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
    /// The settings as last drawn, to redraw only when they change.
    drawn: Arc<Settings>,
    _subs: Vec<Subscription>,
}

impl SettingsView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subs = vec![
            cx.observe_window_appearance(window, |_, window, cx| themes::apply(Some(window), cx)),
            cx.observe_global_in::<Shell>(window, |this: &mut Self, _, cx| {
                let now = settings(cx);
                if !Arc::ptr_eq(&this.drawn, &now) {
                    this.drawn = now;
                    cx.notify();
                }
            }),
        ];
        SettingsView {
            focus: cx.focus_handle(),
            recording: None,
            note: None,
            drawn: settings(cx),
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
                this.recorded(&keystroke, cx);
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
    fn recorded(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        let Some(recording) = self.recording.take() else { return };
        cx.notify();
        let Some(command) = keymap::command(recording.command) else { return };
        if keystroke.key == "escape" && !keystroke.modifiers.modified() {
            return;
        }
        let keys = keystroke.unparse();
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
        let mut bound = current_keys(command, cx);
        if !bound.contains(&keys) {
            bound.push(keys);
        }
        set_keys(command, Some(bound), cx);
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        self.recording = None;
        cx.notify();
    }

    /// Takes one key away from a command, leaving it with none if it was the
    /// last: a command with no key is still in the menus.
    fn unbind(&mut self, command: &'static Command, keys: &str, cx: &mut Context<Self>) {
        self.note = None;
        let mut bound = current_keys(command, cx);
        bound.retain(|k| k != keys);
        set_keys(command, Some(bound), cx);
        cx.notify();
    }

    fn reset_command(&mut self, command: &'static Command, cx: &mut Context<Self>) {
        self.note = None;
        set_keys(command, None, cx);
        cx.notify();
    }

    // ----- pages ----------------------------------------------------------

    fn general_page(&self) -> SettingPage {
        let defaults = General::default();
        let sort_options = SortKey::ALL.map(|key| (key, key.label()));
        SettingPage::new("General")
            .icon(Lucide::Settings)
            .description("How the panes list files, and what the commands ask before they act.")
            .group(
                SettingGroup::new().title("Startup").item(
                    SettingItem::new(
                        "Reopen the last folders",
                        SettingField::switch(
                            |cx| general(cx).restore_folders,
                            |on, cx| edit(SettingsEdit::RestoreFolders(on), cx),
                        )
                        .default_value(defaults.restore_folders),
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
                                |on, cx| edit(SettingsEdit::ShowHidden(on), cx),
                            )
                            .default_value(defaults.show_hidden),
                        )
                        .description("Names that begin with a dot. ⌘⇧. toggles it for this session too.")
                        .keywords(["dotfiles", "hidden"]),
                    )
                    .item(
                        SettingItem::new(
                            "Folders first",
                            SettingField::switch(
                                |cx| general(cx).folders_first,
                                |on, cx| edit(SettingsEdit::FoldersFirst(on), cx),
                            )
                            .default_value(defaults.folders_first),
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
                                |value, cx| edit(SettingsEdit::Sort(SortKey::from_id(&value)), cx),
                            )
                            .default_value(defaults.sort.id()),
                        )
                        .description("Clicking a column header still sorts one tab by hand.")
                        .keywords(["order", "column"]),
                    )
                    .item(
                        SettingItem::new(
                            "Largest or newest first",
                            SettingField::switch(
                                |cx| general(cx).sort_descending,
                                |on, cx| edit(SettingsEdit::SortDescending(on), cx),
                            )
                            .default_value(defaults.sort_descending),
                        )
                        .keywords(["sort", "descending", "reverse"]),
                    )
                    .item(
                        SettingItem::new(
                            "File icons",
                            SettingField::switch(
                                |cx| general(cx).show_icons,
                                |on, cx| edit(SettingsEdit::ShowIcons(on), cx),
                            )
                            .default_value(defaults.show_icons),
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
                                |value, cx| edit(SettingsEdit::DateFormat(DateFormat::from_id(&value)), cx),
                            )
                            .default_value(defaults.date_format.id()),
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
                                |value, cx| edit(SettingsEdit::FontSize(value as f32), cx),
                            )
                            .default_value(defaults.font_size as f64),
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
                                |value, cx| edit(SettingsEdit::RowHeight(value as f32), cx),
                            )
                            .default_value(defaults.row_height as f64),
                        )
                        .description("Pixels. Lower fits more files on the screen.")
                        .keywords(["density", "compact", "spacing"]),
                    ),
            )
            .group(
                SettingGroup::new().title("Safety").item(
                    SettingItem::new(
                        "Ask before moving to Trash",
                        SettingField::switch(
                            |cx| general(cx).confirm_trash,
                            |on, cx| edit(SettingsEdit::ConfirmTrash(on), cx),
                        )
                        .default_value(defaults.confirm_trash),
                    )
                    .description("Deleting permanently always asks, whatever this says.")
                    .keywords(["confirm", "delete", "trash"]),
                ),
            )
            .group(
                SettingGroup::new().title("Tools").item(
                    SettingItem::new(
                        "Terminal",
                        SettingField::input(
                            |cx| general(cx).terminal.into(),
                            |value, cx| edit(SettingsEdit::Terminal(value.to_string()), cx),
                        )
                        .default_value(defaults.terminal),
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
        let mut page = SettingPage::new("Keys").icon(Lucide::Keyboard).description(
            "Press the plus to add a key combination, or the cross on one to take it away. Search above to find a command.",
        );
        for group in keymap::groups() {
            let mut section = SettingGroup::new().title(group);
            for command in keymap::COMMANDS.iter().filter(|c| c.group == group) {
                let row_this = this.clone();
                let reset_this = this.clone();
                section = section.item(
                    SettingItem::render(move |_, _, cx| key_row(command, row_this.clone(), cx))
                        .keywords(search_words(command))
                        .on_reset(
                            move |cx| command.is_changed(&settings(cx).keys),
                            move |_, cx| {
                                let _ = reset_this.update(cx, |this, cx| this.reset_command(command, cx));
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
                                    edit(SettingsEdit::AppearanceMode(AppearanceMode::from_id(&value)), cx)
                                },
                            )
                            .default_value(Appearance::default().mode.id()),
                        )
                        .description("“System” follows the desktop's light and dark setting.")
                        .keywords(["dark", "light", "mode", "appearance"]),
                    )
                    .item(
                        SettingItem::new("Light theme", theme_dropdown(false))
                            .description("Used while the appearance is light.")
                            .keywords(["theme", "light", "colors", "scheme"]),
                    )
                    .item(
                        SettingItem::new("Dark theme", theme_dropdown(true))
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

/// The keys a command answers to now: the user's, or the defaults.
fn current_keys(command: &Command, cx: &App) -> Vec<String> {
    command.keys(&settings(cx).keys).into_iter().map(str::to_string).collect()
}

/// `None` puts the command back to its default keys.
fn set_keys(command: &Command, keys: Option<Vec<String>>, cx: &App) {
    edit(
        SettingsEdit::Keys {
            command: command.id.to_string(),
            keys,
        },
        cx,
    );
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
    let defaults = Appearance::default();
    let default = if dark { defaults.dark } else { defaults.light };
    SettingField::scrollable_dropdown(
        options,
        move |cx| {
            let appearance = appearance(cx);
            if dark { appearance.dark.into() } else { appearance.light.into() }
        },
        move |value, cx| {
            let name = value.to_string();
            edit(
                if dark {
                    SettingsEdit::DarkTheme(name)
                } else {
                    SettingsEdit::LightTheme(name)
                },
                cx,
            );
        },
    )
    .default_value(default)
}

/// One command and its keys.
fn key_row(command: &'static Command, this: WeakEntity<SettingsView>, cx: &mut App) -> AnyElement {
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
    let overrides = settings(cx).keys.clone();
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
                                            .on_click(move |_, _, cx| {
                                                let _ = this.update(cx, |this, cx| this.unbind(command, &keys, cx));
                                            }),
                                    )
                            }))
                            .when(keys.is_empty(), |el| {
                                el.child(div().text_xs().text_color(theme.muted_foreground).child("no key"))
                            })
                            .child({
                                let this = this.clone();
                                Button::new(button_id("key-add", command.id))
                                    .icon(Lucide::Plus)
                                    .ghost()
                                    .xsmall()
                                    .tooltip("Add a key combination")
                                    .on_click(move |_, window, cx| {
                                        let _ = this.update(cx, |this, cx| this.record(command.id, window, cx));
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
                                        let _ = this.update(cx, |this, cx| this.reset_command(command, cx));
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
            .when(selected, |el| el.bg(theme.list_active).border_1().border_color(theme.list_active_border))
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
