//! The bookmark editor and the bookmarks manager.
//!
//! Both are called by the dialog host ([`crate::dialogs`]) for every render
//! of their dialog in `view.dialogs`: [`render_edit`] for
//! `DialogKind::BookmarkEdit`, [`render_manage`] for `DialogKind::Bookmarks`.
//! The host has already set `on_ok` → `DialogInput::Submit` (the editor's
//! "Save & Open", which the core turns into `BookmarksInput::Save { open:
//! true }`; the manager just closes) and `on_cancel` → `DialogInput::Cancel`,
//! both returning `false`.
//!
//! The editor is generic: it shows whatever fields the plugin declares
//! (`view.plugins`). Validation, summaries and saving are the core's; the
//! shell only asks the system for a local path ("Choose…").

use std::collections::HashMap;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::dialog::{Dialog, DialogFooter};
use gpui_kit::component::form::{field, v_form};
use gpui_kit::component::input::Input;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::{prelude::*, *};
use tl_app::bookmarks::{
    BookmarksInput, EditDialog, FieldId, MANAGER_TITLE, ManageDialog, NAME_PLACEHOLDER,
    NO_BOOKMARKS, open_label, summary,
};
use tl_app::conn::LOCAL_PLUGIN;
use tl_app::dialog::DialogInput;
use tl_app::tl_model::{FieldKind, VPath};
use tl_app::{DialogId, Event};

use crate::app_state::{dispatch, view};
use crate::rename::CoreInput;

fn send(input: BookmarksInput, cx: &App) {
    dispatch(Event::Bookmarks(input), cx);
}

/// The editor's text fields, kept while it is rendered: the name, and one
/// per text field of every plugin (switching the type keeps them).
struct Fields {
    name: CoreInput,
    inputs: HashMap<FieldId, CoreInput>,
}

pub fn render_edit(
    dialog: Dialog,
    state: &EditDialog,
    id: DialogId,
    window: &mut Window,
    cx: &mut App,
) -> Dialog {
    let plugins = view(cx).plugins.clone();
    let mut created = false;
    let fields = window.use_keyed_state(("bookmark-edit", id.0), cx, |window, cx| {
        created = true;
        let name = CoreInput::new(
            &state.name,
            |s| s.placeholder(NAME_PLACEHOLDER),
            move |text, cx| send(BookmarksInput::Name { dialog: id, text }, cx),
            window,
            cx,
        );
        let mut inputs = HashMap::new();
        for plugin in plugins.iter() {
            for spec in plugin.fields.iter().filter(|f| f.kind != FieldKind::Toggle) {
                let key: FieldId = (plugin.id.clone(), spec.key.clone());
                let text = state.values.get(&key).map(String::as_str).unwrap_or("");
                let placeholder = spec.placeholder.clone();
                let masked = spec.kind == FieldKind::Secret;
                let field_key = spec.key.clone();
                let input = CoreInput::new(
                    text,
                    |s| s.placeholder(placeholder).masked(masked),
                    move |value, cx| {
                        let key = field_key.clone();
                        send(
                            BookmarksInput::Field {
                                dialog: id,
                                key,
                                value,
                            },
                            cx,
                        )
                    },
                    window,
                    cx,
                );
                inputs.insert(key, input);
            }
        }
        Fields { name, inputs }
    });
    let (name, inputs) = {
        let f = fields.read(cx);
        (f.name.clone(), f.inputs.clone())
    };
    let input_for = |key: &str| inputs.get(&(state.plugin.clone(), key.to_string()));

    name.sync(&state.name, window, cx);
    for ((plugin, key), input) in &inputs {
        if *plugin == state.plugin {
            input.sync(state.value(key), window, cx);
        }
    }
    if created {
        let target = state
            .autofocus
            .as_deref()
            .and_then(input_for)
            .unwrap_or(&name);
        target.focus(window, cx);
    }

    let theme = cx.theme().clone();
    let mut form = v_form().child(field().label("Name").child(Input::new(&name.input).small()));
    for spec in state.fields(&plugins) {
        let mut f = field().label(SharedString::from(spec.label.clone()));
        if !spec.help.is_empty() {
            f = f.description(SharedString::from(spec.help.clone()));
        }
        let key = spec.key.clone();
        let control: AnyElement = match &spec.kind {
            FieldKind::Toggle => Switch::new(SharedString::from(format!("toggle-{key}")))
                .checked(state.toggle(&key))
                .small()
                .on_click(move |checked: &bool, _, cx| {
                    let key = key.clone();
                    send(
                        BookmarksInput::Toggle {
                            dialog: id,
                            key,
                            on: *checked,
                        },
                        cx,
                    )
                })
                .into_any_element(),
            kind => {
                let Some(input) = input_for(&key) else {
                    continue;
                };
                match kind {
                    FieldKind::LocalPath => {
                        let directories = state.picks_directories();
                        h_flex()
                            .gap_1()
                            .child(div().flex_1().child(Input::new(&input.input).small()))
                            .child(
                                Button::new(SharedString::from(format!("browse-{key}")))
                                    .label("Choose…")
                                    .small()
                                    .on_click(move |_, _, cx| {
                                        choose_path(id, key.clone(), directories, cx)
                                    }),
                            )
                            .into_any_element()
                    }
                    // A secret gets the eye button, so it can be checked once.
                    FieldKind::Secret => Input::new(&input.input)
                        .small()
                        .mask_toggle()
                        .into_any_element(),
                    _ => Input::new(&input.input).small().into_any_element(),
                }
            }
        };
        form = form.child(f.child(control));
    }

    let type_picker = state.is_new.then(|| {
        let ids: Vec<String> = plugins.iter().map(|p| p.id.clone()).collect();
        ButtonGroup::new("plugin-type")
            .small()
            .outline()
            .children(plugins.iter().map(|p| {
                Button::new(SharedString::from(format!("type-{}", p.id)))
                    .label(SharedString::from(p.name.clone()))
                    .selected(p.id == state.plugin)
            }))
            .on_click(move |selected: &Vec<usize>, _, cx| {
                if let Some(plugin) = selected.first().and_then(|ix| ids.get(*ix)) {
                    send(
                        BookmarksInput::Plugin {
                            dialog: id,
                            plugin: plugin.clone(),
                        },
                        cx,
                    );
                }
            })
    });

    dialog
        .title(state.title())
        .width(px(560.))
        .child(
            v_flex()
                .gap_3()
                .children(type_picker)
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(state.description(&plugins).to_string()),
                )
                .child(form)
                .when(state.has_advanced(&plugins), |el| {
                    el.child(
                        Button::new("advanced")
                            .label(state.advanced_label())
                            .ghost()
                            .xsmall()
                            .on_click(move |_, _, cx| {
                                send(BookmarksInput::ToggleAdvanced { dialog: id }, cx)
                            }),
                    )
                })
                .when_some(state.error.clone(), |el, error| {
                    el.child(div().text_sm().text_color(theme.danger).child(error))
                }),
        )
        .footer(
            DialogFooter::new()
                .child(
                    Button::new("bm-cancel")
                        .label("Cancel")
                        .on_click(move |_, _, cx| {
                            dispatch(Event::Dialog(id, DialogInput::Cancel), cx)
                        }),
                )
                .child(
                    Button::new("bm-save")
                        .label("Save")
                        .on_click(move |_, _, cx| {
                            send(
                                BookmarksInput::Save {
                                    dialog: id,
                                    open: false,
                                },
                                cx,
                            )
                        }),
                )
                .child(
                    Button::new("bm-connect")
                        .label("Save & Open")
                        .primary()
                        .on_click(move |_, _, cx| {
                            send(
                                BookmarksInput::Save {
                                    dialog: id,
                                    open: true,
                                },
                                cx,
                            )
                        }),
                ),
        )
}

/// "Choose…": the system's file (or folder) picker, its answer to the core.
fn choose_path(id: DialogId, key: String, directories: bool, cx: &mut App) {
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: !directories,
        directories,
        multiple: false,
        prompt: None,
    });
    cx.spawn(async move |cx| {
        if let Ok(Ok(Some(paths))) = rx.await
            && let Some(path) = paths.first()
        {
            let path = VPath::from_local(path);
            cx.update(|cx| {
                send(
                    BookmarksInput::PathChosen {
                        dialog: id,
                        key,
                        path,
                    },
                    cx,
                )
            });
        }
    })
    .detach();
}

pub fn render_manage(
    dialog: Dialog,
    _state: &ManageDialog,
    id: DialogId,
    _window: &mut Window,
    cx: &mut App,
) -> Dialog {
    let theme = cx.theme().clone();
    let view = view(cx);
    let bookmarks = view.bookmarks.all();
    let count = bookmarks.len();
    let list = v_flex()
        .gap_1()
        .when(bookmarks.is_empty(), |el| {
            el.child(
                div()
                    .py_6()
                    .text_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(NO_BOOKMARKS),
            )
        })
        .children(bookmarks.iter().enumerate().map(|(ix, b)| {
            let connected = view.connected.contains(&b.id);
            let summary = summary(b, &view.plugins, &view.summaries);
            let icon = if b.plugin == LOCAL_PLUGIN {
                Lucide::FolderHeart
            } else {
                Lucide::Server
            };
            let (up, down, edit, delete, open) = (
                b.id.clone(),
                b.id.clone(),
                b.id.clone(),
                b.id.clone(),
                b.id.clone(),
            );
            h_flex()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .hover(|s| s.bg(theme.list_hover))
                .child(Icon::new(icon).small().text_color(if connected {
                    theme.success
                } else {
                    theme.muted_foreground
                }))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().truncate().child(b.name.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(summary),
                        ),
                )
                .child(
                    Button::new(("bm-up", ix))
                        .icon(Lucide::ArrowUp)
                        .ghost()
                        .xsmall()
                        .disabled(ix == 0)
                        .on_click(move |_, _, cx| {
                            send(
                                BookmarksInput::Move {
                                    id: up.clone(),
                                    delta: -1,
                                },
                                cx,
                            )
                        }),
                )
                .child(
                    Button::new(("bm-down", ix))
                        .icon(Lucide::ArrowDown)
                        .ghost()
                        .xsmall()
                        .disabled(ix + 1 == count)
                        .on_click(move |_, _, cx| {
                            send(
                                BookmarksInput::Move {
                                    id: down.clone(),
                                    delta: 1,
                                },
                                cx,
                            )
                        }),
                )
                .child(
                    Button::new(("bm-edit", ix))
                        .icon(Lucide::Pencil)
                        .ghost()
                        .xsmall()
                        .tooltip("Edit")
                        .on_click(move |_, _, cx| {
                            send(
                                BookmarksInput::Edit {
                                    manager: id,
                                    id: edit.clone(),
                                },
                                cx,
                            )
                        }),
                )
                .child(
                    Button::new(("bm-del", ix))
                        .icon(Lucide::Trash)
                        .ghost()
                        .xsmall()
                        .tooltip("Delete")
                        .on_click(move |_, _, cx| {
                            send(BookmarksInput::Delete { id: delete.clone() }, cx)
                        }),
                )
                .child(
                    Button::new(("bm-open", ix))
                        .label(open_label(connected))
                        .small()
                        .primary()
                        .on_click(move |_, _, cx| {
                            send(
                                BookmarksInput::Open {
                                    manager: id,
                                    id: open.clone(),
                                },
                                cx,
                            )
                        }),
                )
        }));
    dialog
        .title(MANAGER_TITLE)
        .width(px(620.))
        .child(list)
        .footer(
            DialogFooter::new()
                .child(
                    Button::new("bm-new")
                        .icon(Lucide::Plus)
                        .label("New Bookmark…")
                        .on_click(move |_, _, cx| send(BookmarksInput::New { manager: id }, cx)),
                )
                .child(Button::new("bm-close").label("Close").primary().on_click(
                    move |_, _, cx| dispatch(Event::Dialog(id, DialogInput::Submit), cx),
                )),
        )
}
