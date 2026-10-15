//! Bookmark editor and manager. The editor is generic: it renders whatever
//! fields the bookmark's plugin declares, so new connection types need no
//! UI code.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::form::{field, v_form};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{prelude::*, *};
use tl_core::plugin::FieldKind;
use tl_core::{Bookmark, BookmarkConfig};

use crate::app_state::AppState;
use crate::dialogs;
use crate::pane::Side;
use crate::workspace::Workspace;

struct Form {
    id: String,
    is_new: bool,
    plugin: RefCell<String>,
    name: Entity<InputState>,
    inputs: HashMap<(String, &'static str), Entity<InputState>>,
    toggles: RefCell<HashMap<(String, &'static str), bool>>,
    advanced: Cell<bool>,
    error: RefCell<Option<SharedString>>,
}

impl Form {
    fn config(&self, cx: &App) -> BookmarkConfig {
        let plugin_id = self.plugin.borrow().clone();
        let mut config = BookmarkConfig::new();
        let Some(plugin) = cx.global::<AppState>().registry.get(&plugin_id) else {
            return config;
        };
        for spec in plugin.fields() {
            let key = (plugin_id.clone(), spec.key);
            let value = if spec.kind == FieldKind::Toggle {
                self.toggles.borrow().get(&key).copied().unwrap_or(false).to_string()
            } else {
                self.inputs
                    .get(&key)
                    .map(|i| i.read(cx).value().trim().to_string())
                    .unwrap_or_default()
            };
            if !value.is_empty() && value != "false" {
                config.insert(spec.key.to_string(), value);
            }
        }
        config
    }
}

/// Opens the editor for `template` (or a new SSH bookmark). With
/// `connect_to`, "Save & Connect" opens it in that pane.
pub fn edit(
    template: Option<Bookmark>,
    connect_to: Option<Side>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    let state = cx.global::<AppState>();
    let is_new = template
        .as_ref()
        .is_none_or(|t| state.bookmarks.get(&t.id).is_none());
    let bookmark = template.unwrap_or_else(|| Bookmark::new("", tl_ssh::PLUGIN_ID));
    let plugins: Vec<(String, &'static str)> = state
        .registry
        .all()
        .iter()
        .map(|p| (p.id().to_string(), p.name()))
        .collect();
    let specs: Vec<(String, tl_core::FieldSpec)> = state
        .registry
        .all()
        .iter()
        .flat_map(|p| p.fields().into_iter().map(move |f| (p.id().to_string(), f)))
        .collect();

    let mut inputs = HashMap::new();
    let mut toggles = HashMap::new();
    for (plugin, spec) in &specs {
        let current = if *plugin == bookmark.plugin {
            bookmark.config.get(spec.key).cloned()
        } else {
            None
        };
        if spec.kind == FieldKind::Toggle {
            let on = current.as_deref().map(|v| v == "true").unwrap_or(spec.default == "true");
            toggles.insert((plugin.clone(), spec.key), on);
        } else {
            let value = current.unwrap_or_else(|| spec.default.to_string());
            let placeholder = spec.placeholder;
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value));
            inputs.insert((plugin.clone(), spec.key), input);
        }
    }
    let name_value = bookmark.name.clone();
    let name = cx.new(|cx| InputState::new(window, cx).placeholder("Shown in menus").default_value(name_value));
    let form = Rc::new(Form {
        id: bookmark.id.clone(),
        is_new,
        plugin: RefCell::new(bookmark.plugin.clone()),
        name,
        inputs,
        toggles: RefCell::new(toggles),
        advanced: Cell::new(false),
        error: RefCell::new(None),
    });
    let first = Cell::new(true);

    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            let key = (form.plugin.borrow().clone(), "host");
            let target = form.inputs.get(&key).cloned().unwrap_or_else(|| form.name.clone());
            target.update(cx, |s, cx| s.focus(window, cx));
        }
        let plugin_id = form.plugin.borrow().clone();
        let state = cx.global::<AppState>();
        let plugin = state.registry.get(&plugin_id);
        let description: SharedString = plugin.as_ref().map(|p| p.description()).unwrap_or("").into();
        let theme = cx.theme().clone();

        let mut fields = v_form();
        fields = fields.child(field().label("Name").child(Input::new(&form.name).small()));
        let mut has_advanced = false;
        for (plugin, spec) in specs.iter().filter(|(p, _)| *p == plugin_id) {
            if spec.advanced {
                has_advanced = true;
                if !form.advanced.get() {
                    continue;
                }
            }
            let key = (plugin.clone(), spec.key);
            let mut f = field().label(spec.label);
            if !spec.help.is_empty() {
                f = f.description(spec.help);
            }
            let control: AnyElement = match &spec.kind {
                FieldKind::Toggle => {
                    let on = form.toggles.borrow().get(&key).copied().unwrap_or(false);
                    let form = form.clone();
                    let key = key.clone();
                    Switch::new(SharedString::from(format!("toggle-{}", spec.key)))
                        .checked(on)
                        .small()
                        .on_click(move |checked: &bool, window, _cx| {
                            form.toggles.borrow_mut().insert(key.clone(), *checked);
                            window.refresh();
                        })
                        .into_any_element()
                }
                FieldKind::LocalPath => {
                    let input = form.inputs[&key].clone();
                    let dirs_only = plugin == tl_local::PLUGIN_ID;
                    h_flex()
                        .gap_1()
                        .child(div().flex_1().child(Input::new(&input).small()))
                        .child(
                            Button::new(SharedString::from(format!("browse-{}", spec.key)))
                                .label("Choose…")
                                .small()
                                .on_click(move |_, window, cx| {
                                    let rx = cx.prompt_for_paths(PathPromptOptions {
                                        files: !dirs_only,
                                        directories: dirs_only,
                                        multiple: false,
                                        prompt: None,
                                    });
                                    let input = input.clone();
                                    window
                                        .spawn(cx, async move |cx| {
                                            if let Ok(Ok(Some(paths))) = rx.await
                                                && let Some(p) = paths.first() {
                                                    let text = crate::pane::tilde(&tl_core::VPath::from_local(p));
                                                    let _ = cx.update(|window, cx| {
                                                        input.update(cx, |s, cx| s.set_value(text, window, cx))
                                                    });
                                                }
                                        })
                                        .detach();
                                }),
                        )
                        .into_any_element()
                }
                _ => Input::new(&form.inputs[&key]).small().into_any_element(),
            };
            fields = fields.child(f.child(control));
        }

        let type_picker = form.is_new.then(|| {
            let form = form.clone();
            let ids: Vec<String> = plugins.iter().map(|(id, _)| id.clone()).collect();
            ButtonGroup::new("plugin-type")
                .small()
                .outline()
                .children(plugins.iter().map(|(id, name)| {
                    Button::new(SharedString::from(format!("type-{id}")))
                        .label(*name)
                        .selected(*id == plugin_id)
                }))
                .on_click(move |selected: &Vec<usize>, window, _cx| {
                    if let Some(ix) = selected.first() {
                        *form.plugin.borrow_mut() = ids[*ix].clone();
                        *form.error.borrow_mut() = None;
                        window.refresh();
                    }
                })
        });

        let save = {
            let (form, workspace) = (form.clone(), workspace.clone());
            move |connect: bool, window: &mut Window, cx: &mut App| -> bool {
                let plugin_id = form.plugin.borrow().clone();
                let Some(plugin) = cx.global::<AppState>().registry.get(&plugin_id) else {
                    return false;
                };
                let config = form.config(cx);
                if let Err(err) = plugin.validate(&config) {
                    *form.error.borrow_mut() = Some(err.into());
                    window.refresh();
                    return false;
                }
                let mut bookmark = Bookmark::new("", plugin_id.clone());
                bookmark.id = form.id.clone();
                bookmark.config = config;
                let name = form.name.read(cx).value().trim().to_string();
                bookmark.name = if name.is_empty() { plugin.summary(&bookmark) } else { name };
                let id = bookmark.id.clone();
                if let Err(err) = cx.global_mut::<AppState>().bookmarks.upsert(bookmark) {
                    *form.error.borrow_mut() = Some(format!("Cannot save bookmarks: {err}").into());
                    window.refresh();
                    return false;
                }
                if connect {
                    let side = connect_to.unwrap_or(Side::Left);
                    let _ = workspace.update(cx, |ws, cx| ws.open_bookmark(id, side, window, cx));
                }
                true
            }
        };
        let (save_btn, connect_btn, save_ok) = (save.clone(), save.clone(), save);
        let error = form.error.borrow().clone();
        let form_adv = form.clone();

        dialog
            .title(if form.is_new { "New Bookmark" } else { "Edit Bookmark" })
            .width(px(560.))
            .child(
                v_flex()
                    .gap_3()
                    .children(type_picker)
                    .child(div().text_sm().text_color(theme.muted_foreground).child(description))
                    .child(fields)
                    .when(has_advanced, |el| {
                        el.child(
                            Button::new("advanced")
                                .label(if form.advanced.get() { "Hide advanced options" } else { "Show advanced options" })
                                .ghost()
                                .xsmall()
                                .on_click(move |_, window, _| {
                                    form_adv.advanced.set(!form_adv.advanced.get());
                                    window.refresh();
                                }),
                        )
                    })
                    .when_some(error, |el, error| {
                        el.child(div().text_sm().text_color(theme.danger).child(error))
                    }),
            )
            .footer(
                DialogFooter::new()
                    .child(Button::new("bm-cancel").label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("bm-save").label("Save").on_click(move |_, window, cx| {
                        if save_btn(false, window, cx) {
                            window.close_dialog(cx);
                        }
                    }))
                    .child(
                        Button::new("bm-connect")
                            .label("Save & Open")
                            .primary()
                            .on_click(move |_, window, cx| {
                                if connect_btn(true, window, cx) {
                                    window.close_dialog(cx);
                                }
                            }),
                    ),
            )
            .on_ok(move |_, window, cx| save_ok(true, window, cx))
    });
}

/// Lists bookmarks with open / edit / delete / reorder actions.
pub fn manage(workspace: WeakEntity<Workspace>, side: Side, window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, move |dialog, _window, cx| {
        let theme = cx.theme().clone();
        let state = cx.global::<AppState>();
        let rows: Vec<(Bookmark, String, bool)> = state
            .bookmarks
            .all()
            .iter()
            .map(|b| {
                let summary = state
                    .registry
                    .get(&b.plugin)
                    .map(|p| p.summary(b))
                    .unwrap_or_else(|| format!("unknown type “{}”", b.plugin));
                (b.clone(), summary, state.connection(&b.id).is_some())
            })
            .collect();
        let count = rows.len();
        let list = v_flex()
            .gap_1()
            .when(rows.is_empty(), |el| {
                el.child(
                    div()
                        .py_6()
                        .text_center()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("No bookmarks yet. Create one to connect to a server over SSH."),
                )
            })
            .children(rows.into_iter().enumerate().map(|(ix, (b, summary, connected))| {
                let (ws1, ws2) = (workspace.clone(), workspace.clone());
                let (open_id, del_id, up_id, down_id) = (b.id.clone(), b.id.clone(), b.id.clone(), b.id.clone());
                let edit_b = b.clone();
                let del_name = b.name.clone();
                let icon = if b.plugin == tl_local::PLUGIN_ID { Lucide::FolderHeart } else { Lucide::Server };
                h_flex()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .hover(|s| s.bg(theme.list_hover))
                    .child(Icon::new(icon).small().text_color(if connected { theme.success } else { theme.muted_foreground }))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().truncate().child(b.name.clone()))
                            .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(summary)),
                    )
                    .child(
                        Button::new(("bm-up", ix))
                            .icon(Lucide::ArrowUp)
                            .ghost()
                            .xsmall()
                            .disabled(ix == 0)
                            .on_click(move |_, window, cx| {
                                let _ = cx.global_mut::<AppState>().bookmarks.reorder(&up_id, -1);
                                window.refresh();
                            }),
                    )
                    .child(
                        Button::new(("bm-down", ix))
                            .icon(Lucide::ArrowDown)
                            .ghost()
                            .xsmall()
                            .disabled(ix + 1 == count)
                            .on_click(move |_, window, cx| {
                                let _ = cx.global_mut::<AppState>().bookmarks.reorder(&down_id, 1);
                                window.refresh();
                            }),
                    )
                    .child(
                        Button::new(("bm-edit", ix))
                            .icon(Lucide::Pencil)
                            .ghost()
                            .xsmall()
                            .tooltip("Edit")
                            .on_click(move |_, window, cx| edit(Some(edit_b.clone()), Some(side), ws1.clone(), window, cx)),
                    )
                    .child(
                        Button::new(("bm-del", ix))
                            .icon(Lucide::Trash)
                            .ghost()
                            .xsmall()
                            .tooltip("Delete")
                            .on_click(move |_, window, cx| {
                                let answer = dialogs::ask_confirm(
                                    window,
                                    cx,
                                    format!("Delete bookmark “{del_name}”?"),
                                    "Open connections stay open until you disconnect.",
                                    "Delete",
                                    true,
                                );
                                let id = del_id.clone();
                                window
                                    .spawn(cx, async move |cx| {
                                        if answer.await {
                                            let _ = cx.update(|window, cx| {
                                                let _ = cx.global_mut::<AppState>().bookmarks.remove(&id);
                                                window.refresh();
                                            });
                                        }
                                    })
                                    .detach();
                            }),
                    )
                    .child(
                        Button::new(("bm-open", ix))
                            .label(if connected { "Open" } else { "Connect" })
                            .small()
                            .primary()
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                let _ = ws2.update(cx, |ws, cx| ws.open_bookmark(open_id.clone(), side, window, cx));
                            }),
                    )
            }));
        let ws_new = workspace.clone();
        dialog
            .title("Bookmarks")
            .width(px(620.))
            .child(list)
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("bm-new")
                            .icon(Lucide::Plus)
                            .label("New Bookmark…")
                            .on_click(move |_, window, cx| edit(None, Some(side), ws_new.clone(), window, cx)),
                    )
                    .child(Button::new("bm-close").label("Close").primary().on_click(|_, window, cx| window.close_dialog(cx))),
            )
    });
}
