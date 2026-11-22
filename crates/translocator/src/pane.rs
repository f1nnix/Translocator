//! One file pane, drawn from its `PaneView`: the location line, tabs, the
//! row of saved folders, column headers, the virtualized file list and the
//! status line. Keys, clicks, drags and menus become core events; what the
//! pane shows changes only when the core says so.
//!
//! The list is a `uniform_list` over the view's row order, so only the rows
//! on screen are built, and dates and sizes are formatted for those alone.
//! The pane redraws when its own part of the view changed: a job's progress
//! elsewhere in the window costs it nothing.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, Theme, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::{prelude::*, *};
use tl_app::conn::LOCAL_PLUGIN;
use tl_app::model::Command;
use tl_app::pane::{Dragged, PARENT, PaneInput};
use tl_app::settings::{Settings, SortKey};
use tl_app::tl_model::transfer::TransferMode;
use tl_app::tl_model::util::human_size;
use tl_app::tl_model::{Entry, EntryKind, VPath};
use tl_app::view::{LocationKind, PaneView, ViewModel};
use tl_app::{ConnId, Event, Side, TabId};

use crate::actions::*;
use crate::app_state::{Shell, dispatch, view};
use crate::util::{entry_icon, format_date};

const EXT_W: f32 = 52.;
const SIZE_W: f32 = 74.;
const DATE_W: f32 = 116.;
const MODE_W: f32 = 84.;

/// Size of the text in the list, which [`crate::themes::tweaks`] keeps in
/// step with the setting.
fn font_size(cx: &App) -> Pixels {
    cx.theme().font_size
}

/// Sends a command for `side`: the core acts on its active pane, so the
/// side is made the active one first if it is not. A menu of one pane can
/// be used while the keyboard is in the other.
pub fn pane_command(side: Side, cmd: Command, cx: &App) {
    if view(cx).active != side {
        dispatch(Event::Focus(side), cx);
    }
    dispatch(Event::Command(cmd), cx);
}

fn pane_input(side: Side, input: PaneInput, cx: &App) {
    dispatch(Event::Pane(side, input), cx);
}

/// What a drag from a pane carries: the marked rows if the dragged row is
/// marked, else that row.
#[derive(Clone)]
pub struct DraggedFiles {
    side: Side,
    conn: ConnId,
    dir: VPath,
    names: Rc<Vec<Vec<u8>>>,
    /// Which of `names` are folders, for the system's drag payload.
    dirs: Rc<Vec<bool>>,
    elevated: bool,
    /// On the local disk, so other applications can take the files.
    local: bool,
}

impl DraggedFiles {
    fn dragged(&self) -> Dragged {
        Dragged {
            side: self.side,
            conn: self.conn,
            dir: self.dir.clone(),
            names: self.names.to_vec(),
            elevated: self.elevated,
        }
    }
}

struct DragPreview {
    label: SharedString,
}

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .shadow_md()
            .text_size(font_size(cx))
            .child(Icon::new(Lucide::Files).small())
            .child(self.label.clone())
    }
}

pub struct FilePane {
    pub side: Side,
    pub focus: FocusHandle,
    /// This side of the view, as last drawn.
    view: PaneView,
    settings: Arc<Settings>,
    /// Scroll position per tab, so switching tabs keeps each one's.
    scrolls: HashMap<TabId, UniformListScrollHandle>,
    /// The marked rows in list order, with which are folders: what a drag
    /// of a marked row carries. Rebuilt when marks or rows change.
    drag_names: Rc<Vec<Vec<u8>>>,
    drag_dirs: Rc<Vec<bool>>,
    /// The ".." row.
    parent: Entry,
    narrow: u8,
    /// How many characters of path fit in the header.
    crumb_chars: usize,
    /// Top-left of the file list, where the keyboard locations menu opens.
    list_origin: Point<Pixels>,
    popup: Option<(Entity<PopupMenu>, Subscription)>,
    _subs: Vec<Subscription>,
}

impl Focusable for FilePane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Whether two views of a pane draw alike. The big parts are shared
/// `Arc`s, so this compares pointers for them.
fn same(a: &PaneView, b: &PaneView) -> bool {
    a.tab_id == b.tab_id
        && Arc::ptr_eq(&a.listing.entries, &b.listing.entries)
        && Arc::ptr_eq(&a.order, &b.order)
        && a.cursor == b.cursor
        && Arc::ptr_eq(&a.marked, &b.marked)
        && Arc::ptr_eq(&a.dir_sizes, &b.dir_sizes)
        && a.filter == b.filter
        && a.unfiltered == b.unfiltered
        && a.loading == b.loading
        && a.error == b.error
        && a.disconnected == b.disconnected
        && a.can_reconnect == b.can_reconnect
        && a.conn == b.conn
        && a.path == b.path
        && a.elevated == b.elevated
        && a.caps == b.caps
        && a.kind == b.kind
        && a.location_label == b.location_label
        && a.sort == b.sort
        && a.status == b.status
        && a.free == b.free
        && a.active_tab == b.active_tab
        && a.tab_titles == b.tab_titles
        && a.places == b.places
        && a.place_saved == b.place_saved
}

impl FilePane {
    pub fn new(side: Side, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let subs = vec![
            cx.on_focus(&focus, window, move |_this: &mut Self, _window, cx| {
                dispatch(Event::Focus(side), cx);
                cx.notify();
            }),
            cx.on_blur(&focus, window, |_this: &mut Self, _window, cx| cx.notify()),
            cx.observe_global_in::<Shell>(window, Self::view_changed),
        ];
        let shell = view(cx);
        let mut pane = FilePane {
            side,
            focus,
            view: shell.panes[side.index()].clone(),
            settings: shell.settings.clone(),
            scrolls: HashMap::new(),
            drag_names: Rc::default(),
            drag_dirs: Rc::default(),
            parent: Entry::new(b"..".to_vec(), EntryKind::Dir),
            narrow: 0,
            crumb_chars: 60,
            list_origin: Point::default(),
            popup: None,
            _subs: subs,
        };
        pane.collect_marked();
        pane
    }

    fn view_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shell = view(cx);
        let new = &shell.panes[self.side.index()];
        let settings_changed = !Arc::ptr_eq(&self.settings, &shell.settings);
        if !same(&self.view, new) || settings_changed {
            let moved = new.cursor != self.view.cursor
                || !Arc::ptr_eq(&new.order, &self.view.order)
                || new.tab_id != self.view.tab_id;
            let marks = !Arc::ptr_eq(&new.marked, &self.view.marked) || !Arc::ptr_eq(&new.order, &self.view.order);
            self.view = new.clone();
            self.settings = shell.settings.clone();
            if marks {
                self.collect_marked();
            }
            if moved {
                let cursor = self.view.cursor;
                self.scroll().scroll_to_item(cursor, ScrollStrategy::Nearest);
            }
            cx.notify();
        }
        // ⌘K, ⌥F1, ⌥F2: the core asks for this side's locations menu.
        if let Some((side, n)) = shell.locations_request
            && side == self.side
        {
            let seen = &cx.global::<Shell>().seen.locations_request[self.side.index()];
            if seen.get() != n {
                seen.set(n);
                self.open_locations(window, cx);
            }
        }
    }

    /// Where the file list starts in the window, as last laid out: what
    /// says whether a row above it is really drawn.
    #[cfg(test)]
    pub fn list_top(&self) -> f32 {
        f32::from(self.list_origin.y)
    }

    fn collect_marked(&mut self) {
        let v = &self.view;
        let (mut names, mut dirs) = (Vec::new(), Vec::new());
        if !v.marked.is_empty() {
            for &ix in v.order.iter().filter(|ix| **ix != PARENT) {
                let entry = &v.listing.entries[ix as usize];
                if v.marked.contains(&entry.name) {
                    names.push(entry.name.clone());
                    dirs.push(entry.is_dir());
                }
            }
        }
        self.drag_names = Rc::new(names);
        self.drag_dirs = Rc::new(dirs);
    }

    fn scroll(&mut self) -> UniformListScrollHandle {
        self.scrolls.entry(self.view.tab_id).or_default().clone()
    }

    /// The entry behind a row; ".." for the parent row.
    fn row(&self, ix: usize) -> Option<(&Entry, &str, bool)> {
        let v = &self.view;
        match *v.order.get(ix)? {
            PARENT => Some((&self.parent, "..", true)),
            i => Some((v.listing.entries.get(i as usize)?, v.listing.names.get(i as usize)?.as_str(), false)),
        }
    }

    /// A command from this pane's keys or menus.
    fn cmd(&self, cmd: Command, cx: &App) {
        pane_command(self.side, cmd, cx);
    }

    fn input(&self, input: PaneInput, cx: &App) {
        pane_input(self.side, input, cx);
    }

    /// How many rows fit in the list, for PageUp / PageDown.
    fn page_rows(&mut self) -> usize {
        let rows = self.view.order.len();
        let scroll = self.scroll();
        let state = scroll.0.borrow();
        match state.last_item_size {
            Some(size) if rows > 0 => {
                let row_h = size.contents.height / rows as f32;
                ((size.item.height / row_h).floor() as usize).saturating_sub(1).max(1)
            }
            _ => 20,
        }
    }

    /// The location menu as a keyboard-driven popup (⌘K, ⌥F1/⌥F2).
    fn open_locations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (side, focus) = (self.side, self.focus.clone());
        let menu = PopupMenu::build(window, cx, move |menu, window, cx| {
            location_menu(menu, side, focus.clone(), window, cx)
        });
        let handle = menu.focus_handle(cx);
        window.focus(&handle, cx);
        let sub = cx.subscribe_in(&menu, window, |this: &mut Self, _, _: &DismissEvent, window, cx| {
            this.popup = None;
            // Give focus back unless a menu item opened a dialog.
            if !window.has_active_dialog(cx) {
                window.focus(&this.focus, cx);
            }
            cx.notify();
        });
        self.popup = Some((menu, sub));
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let mods = ks.modifiers;
        if ks.key == "escape" {
            // Only a filter to clear stops Escape here; otherwise it goes
            // on to whatever else wants it.
            if !self.view.filter.is_empty() {
                self.cmd(Command::ClearFilter, cx);
                cx.stop_propagation();
            }
            return;
        }
        if mods.platform || mods.control || mods.function || mods.alt {
            return;
        }
        let Some(ch) = ks.key_char.as_deref() else { return };
        if ch.is_empty() || ch.chars().any(char::is_control) {
            return;
        }
        // The type-ahead filter; with no filter yet, + and - ask for a mask.
        self.cmd(Command::Type(ch.to_string()), cx);
        cx.stop_propagation();
    }

    // ----- rendering ------------------------------------------------------

    fn render_row(&self, ix: usize, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some((entry, name, parent)) = self.row(ix) else {
            return div().into_any_element();
        };
        let v = &self.view;
        let general = &self.settings.general;
        let theme = cx.theme().clone();
        let is_cursor = ix == v.cursor;
        let marked = !parent && v.marked.contains(&entry.name);
        let hidden = !parent && entry.is_hidden();
        let is_dir = entry.is_dir();

        let icon = if parent { Lucide::CornerLeftUp } else { entry_icon(entry) };
        let icon_color = if is_dir {
            theme.blue
        } else if entry.is_executable() {
            theme.green
        } else {
            theme.muted_foreground
        };
        let accent = accent(&theme);
        let fg = if marked { marked_fg(&theme) } else { theme.foreground };
        let muted = theme.muted_foreground;

        let ext: SharedString = if is_dir || parent {
            SharedString::default()
        } else {
            entry.extension().map(|e| String::from_utf8_lossy(e).into_owned()).unwrap_or_default().into()
        };
        let size: SharedString = if parent {
            "UP".into()
        } else if is_dir {
            match v.dir_sizes.get(&entry.name) {
                Some(size) => human_size(*size).into(),
                None => "DIR".into(),
            }
        } else {
            human_size(entry.size).into()
        };
        let date: SharedString = if parent {
            SharedString::default()
        } else {
            format_date(entry.mtime, general.date_format).into()
        };
        let mode: SharedString = if parent { SharedString::default() } else { entry.mode_string().into() };
        let link: Option<SharedString> = entry
            .link_target
            .as_ref()
            .map(|t| format!("→ {}", String::from_utf8_lossy(t)).into());

        let bg = row_bg(&theme, is_cursor, focused, marked);
        let narrow = self.narrow;
        let (names, dirs) = if marked && !self.drag_names.is_empty() {
            (self.drag_names.clone(), self.drag_dirs.clone())
        } else {
            (Rc::new(vec![entry.name.clone()]), Rc::new(vec![is_dir]))
        };
        let drag = DraggedFiles {
            side: self.side,
            conn: v.conn,
            dir: v.path.clone(),
            names,
            dirs,
            elevated: v.elevated,
            local: v.kind == LocationKind::Local,
        };
        let name: SharedString = SharedString::from(name.to_string());

        h_flex()
            .id(("row", ix))
            .h(px(general.row_height()))
            .w_full()
            .px_2()
            .gap_2()
            .text_size(font_size(cx))
            .text_color(fg)
            .when_some(bg, |el, bg| el.bg(bg))
            // Always the same 2px, so the row does not shift sideways as
            // the cursor arrives.
            .border_l_2()
            .border_color(if is_cursor && focused { accent } else { accent.opacity(0.) })
            .when(hidden, |el| el.opacity(0.6))
            .when(general.show_icons, |el| {
                el.child(Icon::new(icon).size(px(15.)).flex_shrink_0().text_color(icon_color))
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_shrink(1.)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .when(marked, |d| d.font_weight(FontWeight::SEMIBOLD))
                            .child(name),
                    )
                    .when_some(link, |el, link| {
                        el.child(div().flex_shrink_0().max_w(px(220.)).truncate().text_color(muted).text_xs().child(link))
                    }),
            )
            .when(narrow < 3, |el| {
                el.child(div().w(px(EXT_W)).flex_shrink_0().truncate().text_color(muted).child(ext))
            })
            .child(
                div()
                    .w(px(SIZE_W))
                    .flex_shrink_0()
                    .text_right()
                    .truncate()
                    .when(is_dir || parent, |d| d.text_color(muted).text_xs())
                    .child(size),
            )
            .when(narrow < 2, |el| {
                el.child(div().w(px(DATE_W)).flex_shrink_0().text_right().truncate().text_color(muted).child(date))
            })
            .when(narrow < 1, |el| {
                el.child(
                    div()
                        .w(px(MODE_W))
                        .flex_shrink_0()
                        .font_family(theme.mono_font_family.clone())
                        .text_xs()
                        .text_color(muted)
                        .child(mode),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                    let mods = ev.modifiers;
                    this.input(
                        PaneInput::Click {
                            row: ix,
                            toggle: mods.platform,
                            range: !mods.platform && mods.shift,
                        },
                        cx,
                    );
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _ev: &MouseDownEvent, window, cx| {
                    // The context menu's commands act on the pane with the
                    // keyboard: make sure that is this one.
                    window.focus(&this.focus, cx);
                    this.input(PaneInput::ContextClick { row: ix }, cx);
                }),
            )
            .on_click(cx.listener(move |this, ev: &ClickEvent, _window, cx| {
                if ev.click_count() == 2 {
                    this.input(PaneInput::DoubleClick { row: ix }, cx);
                }
            }))
            .when(!parent, |el| {
                el.on_drag(drag, |drag: &DraggedFiles, _offset, _window, cx| {
                    let label = match drag.names.as_slice() {
                        [one] => String::from_utf8_lossy(one).into_owned(),
                        many => format!("{} items", many.len()),
                    };
                    cx.new(|_| DragPreview { label: label.into() })
                })
                .external_drag_payload(|drag: &DraggedFiles, _window, _cx| {
                    if !drag.local {
                        return None;
                    }
                    let files = drag
                        .names
                        .iter()
                        .zip(drag.dirs.iter())
                        .map(|(name, is_dir)| (drag.dir.child(name).to_local(), *is_dir));
                    Some(ExternalDragPayload::Files(FileDragPaths::new(files)))
                })
            })
            .into_any_element()
    }

    fn render_header(&self, focused: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let v = &self.view;
        let in_archive = v.kind == LocationKind::Archive;
        let is_local = v.kind == LocationKind::Local;
        let label: SharedString = v.location_label.clone().into();
        let elevated = v.elevated;
        let (side, focus) = (self.side, self.focus.clone());

        // Breadcrumb; leading parts collapse into "…" when space runs out.
        let mut crumbs: Vec<(SharedString, VPath)> = vec![("/".into(), VPath::root())];
        crumbs.extend(v.crumbs.iter().map(|(name, path)| (name.clone().into(), path.clone())));
        let last = crumbs.len() - 1;
        let mut budget = self.crumb_chars;
        let mut first_shown = crumbs.len();
        for (i, (name, _)) in crumbs.iter().enumerate().rev() {
            let cost = name.chars().count() + 3;
            if i != last && cost > budget {
                break;
            }
            budget = budget.saturating_sub(cost);
            first_shown = i;
        }
        let ellipsis = first_shown > 0;
        let crumb_els = crumbs.into_iter().enumerate().skip(first_shown).map(|(i, (name, path))| {
            h_flex()
                .flex_shrink_0()
                .when(i > 1 || (ellipsis && i == first_shown), |el| {
                    el.child(div().px(px(2.)).text_color(theme.muted_foreground).child(if ellipsis && i == first_shown {
                        "… /"
                    } else {
                        "/"
                    }))
                })
                .child(
                    div()
                        .id(("crumb", i))
                        .px(px(3.))
                        .rounded_sm()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.list_hover))
                        .when(i == last, |d| {
                            d.font_weight(FontWeight::SEMIBOLD).text_color(if elevated { theme.danger } else { theme.foreground })
                        })
                        .when(i != last, |d| d.text_color(theme.muted_foreground))
                        .child(name)
                        .on_click(move |_: &ClickEvent, _window, cx| pane_input(side, PaneInput::Navigate(path.clone()), cx)),
                )
        });

        h_flex()
            .h(px(32.))
            .flex_shrink_0()
            .px_1()
            .gap_1()
            .border_b_1()
            // The active pane carries a tinted location line, so which one
            // it is does not depend on a cursor row being in view.
            .when(focused, |el| el.bg(accent(&theme).opacity(0.10)))
            .border_color(if focused { accent(&theme) } else { theme.border })
            .child(
                Button::new("location")
                    .icon(if in_archive {
                        Lucide::FileArchive
                    } else if is_local {
                        Lucide::HardDrive
                    } else {
                        Lucide::Server
                    })
                    .label(label)
                    .ghost()
                    .small()
                    .compact()
                    .tab_stop(false)
                    .dropdown_caret(true)
                    .dropdown_menu(move |menu, window, cx| {
                        pane_input(side, PaneInput::LocationsShown, cx);
                        location_menu(menu, side, focus.clone(), window, cx)
                    }),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_size(font_size(cx))
                    .children(crumb_els),
            )
            .when(v.loading, |el| {
                el.child(Icon::new(IconName::LoaderCircle).small().text_color(theme.muted_foreground))
            })
            .when(v.caps.elevation, |el| {
                el.child(
                    Button::new("sudo")
                        .icon(if elevated { Lucide::LockOpen } else { Lucide::Lock })
                        .when(elevated, |b| b.label("sudo").danger())
                        .when(!elevated, |b| b.ghost())
                        .xsmall()
                        .tab_stop(false)
                        .tooltip(if elevated {
                            "Running as root (sudo). Click to drop privileges."
                        } else {
                            "Elevate this pane with sudo (⌘⇧S)"
                        })
                        .on_click(move |_, _, cx| pane_command(side, Command::ToggleSudo, cx)),
                )
            })
    }

    /// The quick-switch row: the folders saved for whatever host this pane
    /// is connected to, so switching connection switches the row.
    fn render_places(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let v = &self.view;
        let side = self.side;
        let last = v.places.len().saturating_sub(1);
        let saved = v.place_saved;
        let in_archive = v.kind == LocationKind::Archive;

        let chips = v.places.iter().enumerate().map(|(ix, place)| {
            let full = place.path.display();
            // The row can hold two folders of the same name, so the path is
            // what tells them apart.
            let tip: SharedString = match ix < 9 {
                true => format!("{full}  ⌘{}", ix + 1).into(),
                false => full.into(),
            };
            let (go, id) = (place.path.clone(), place.id.clone());
            div()
                .id(("place", ix))
                .flex_shrink_0()
                .px(px(6.))
                .rounded_sm()
                .cursor_pointer()
                .when(place.current, |d| d.bg(accent(&theme).opacity(0.22)).text_color(theme.foreground))
                .when(!place.current, |d| d.text_color(theme.muted_foreground).hover(|s| s.bg(theme.list_hover)))
                .child(SharedString::from(place.name.clone()))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .on_click(move |_: &ClickEvent, _window, cx| pane_input(side, PaneInput::Navigate(go.clone()), cx))
                .context_menu(move |menu, _window, _cx| {
                    let (i1, i2, i3, i4) = (id.clone(), id.clone(), id.clone(), id.clone());
                    menu.min_w(px(180.))
                        .item(
                            PopupMenuItem::new("Rename…")
                                .icon(Lucide::PenLine)
                                .on_click(move |_, _, cx| pane_input(side, PaneInput::RenamePlace(i1.clone()), cx)),
                        )
                        .item(
                            PopupMenuItem::new("Move Left")
                                .icon(Lucide::ChevronLeft)
                                .disabled(ix == 0)
                                .on_click(move |_, _, cx| {
                                    pane_input(side, PaneInput::MovePlace { id: i2.clone(), delta: -1 }, cx)
                                }),
                        )
                        .item(
                            PopupMenuItem::new("Move Right")
                                .icon(Lucide::ChevronRight)
                                .disabled(ix == last)
                                .on_click(move |_, _, cx| {
                                    pane_input(side, PaneInput::MovePlace { id: i3.clone(), delta: 1 }, cx)
                                }),
                        )
                        .separator()
                        .item(
                            PopupMenuItem::new("Remove")
                                .icon(Lucide::BookmarkX)
                                .on_click(move |_, _, cx| pane_input(side, PaneInput::RemovePlace(i4.clone()), cx)),
                        )
                })
        });

        h_flex()
            .h(px(24.))
            .flex_shrink_0()
            .px_1()
            .gap(px(3.))
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(12.))
            .child(Icon::new(Lucide::Bookmark).xsmall().text_color(theme.muted_foreground))
            .child(
                h_flex()
                    .id(("places", self.side.index()))
                    .flex_1()
                    .min_w_0()
                    .gap(px(3.))
                    // More folders than fit scroll sideways; ⌘1…⌘9 and the
                    // location popup reach the ones past the edge.
                    .overflow_x_scroll()
                    .children(chips),
            )
            .child(
                Button::new("toggle-place")
                    .icon(if saved { Lucide::BookmarkMinus } else { Lucide::BookmarkPlus })
                    .ghost()
                    .xsmall()
                    .tab_stop(false)
                    .disabled(in_archive)
                    .tooltip(if saved {
                        "Take this folder off the row (⌘D)"
                    } else {
                        "Save this folder on the row (⌘D)"
                    })
                    .on_click(move |_, _, cx| pane_command(side, Command::TogglePlace, cx)),
            )
    }

    fn render_columns(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let sort = self.view.sort;
        let narrow = self.narrow;
        let side = self.side;
        let cell = |id: &'static str, label: &'static str, key: SortKey| {
            let arrow = if sort.key == key { if sort.desc { " ↓" } else { " ↑" } } else { "" };
            div()
                .id(id)
                .cursor_pointer()
                .truncate()
                .hover(|s| s.text_color(theme.foreground))
                .child(format!("{label}{arrow}"))
                .on_click(move |_: &ClickEvent, _window, cx| pane_input(side, PaneInput::SortBy(key), cx))
        };
        h_flex()
            .h(px(22.))
            .flex_shrink_0()
            .px_2()
            .gap_2()
            .text_size(px(11.))
            .text_color(theme.muted_foreground)
            .border_b_1()
            .border_color(theme.border)
            .child(div().w(px(15.)).flex_shrink_0())
            .child(cell("col-name", "Name", SortKey::Name).flex_1().min_w_0())
            .when(narrow < 3, |el| el.child(cell("col-ext", "Ext", SortKey::Ext).w(px(EXT_W)).flex_shrink_0()))
            .child(cell("col-size", "Size", SortKey::Size).w(px(SIZE_W)).flex_shrink_0().text_right())
            .when(narrow < 2, |el| {
                el.child(cell("col-date", "Modified", SortKey::Modified).w(px(DATE_W)).flex_shrink_0().text_right())
            })
            .when(narrow < 1, |el| el.child(div().w(px(MODE_W)).flex_shrink_0().child("Permissions")))
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let text: SharedString = self.view.status.clone().into();
        let free: SharedString = self.view.free.clone().unwrap_or_default().into();
        h_flex()
            .h(px(22.))
            .flex_shrink_0()
            .px_2()
            .gap_2()
            .text_size(px(11.))
            .text_color(theme.muted_foreground)
            .border_t_1()
            .border_color(theme.border)
            .child(div().flex_1().min_w_0().truncate().child(text))
            .child(div().flex_shrink_0().child(free))
    }

    fn render_tabs(&self) -> impl IntoElement + use<> {
        let side = self.side;
        TabBar::new(("tabs", side.index()))
            .small()
            .selected_index(self.view.active_tab)
            .children(self.view.tab_titles.iter().enumerate().map(|(ix, title)| {
                Tab::new().label(SharedString::from(title.clone())).suffix(
                    Button::new(("close-tab", ix))
                        .icon(IconName::Close)
                        .ghost()
                        .xsmall()
                        .tab_stop(false)
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            pane_input(side, PaneInput::CloseTab(ix), cx);
                        }),
                )
            }))
            .on_click(move |ix: &usize, _, cx| pane_input(side, PaneInput::SelectTab(*ix), cx))
    }
}

/// What marks out the active pane.
///
/// Not `theme.primary`, which is what this used to use and cannot do the
/// job: it is a near-black in the light theme and a near-white in the dark
/// one, so it comes out the same colour as ordinary text and both panes
/// looked alike. The blue and the amber below are the theme's own accents
/// and read as accents in either.
pub(crate) fn accent(theme: &Theme) -> Hsla {
    theme.blue
}

/// The background of one row.
///
/// One pane out of the two is the active one, and the cursor is what says
/// which: a solid band of colour in that pane, a whisper of grey in the
/// other.
pub(crate) fn row_bg(theme: &Theme, is_cursor: bool, focused: bool, marked: bool) -> Option<Hsla> {
    if is_cursor && focused {
        Some(accent(theme).opacity(0.42))
    } else if is_cursor {
        Some(theme.muted)
    } else if marked {
        Some(marked_fg(theme).opacity(0.14))
    } else {
        None
    }
}

/// Marked rows, which an operation acts on, in the theme's amber.
pub(crate) fn marked_fg(theme: &Theme) -> Hsla {
    theme.warning
}

/// The icon of a folder in the locations menu.
fn folder_icon(label: &str, volume: bool) -> Lucide {
    match label {
        _ if volume => Lucide::HardDrive,
        "Home" => Lucide::House,
        "Desktop" => Lucide::Monitor,
        "Documents" => Lucide::FileText,
        "Downloads" => Lucide::Download,
        _ => Lucide::Folder,
    }
}

/// The location dropdown: this host's saved folders, this Mac, bookmarks,
/// connection commands. Built from the view when it opens.
fn location_menu(
    menu: PopupMenu,
    side: Side,
    focus: FocusHandle,
    _window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let shell: Rc<ViewModel> = view(cx);
    let pane = &shell.panes[side.index()];
    let in_archive = pane.kind == LocationKind::Archive;
    // Shortcuts are looked up where the pane's keys live.
    let mut menu = menu.min_w(px(240.)).action_context(focus);

    // This host's own folders first: on a server, the items below are
    // somewhere else entirely.
    if !pane.places.is_empty() {
        menu = menu.label("Places");
        for (ix, place) in pane.places.iter().enumerate() {
            menu = menu.item(
                PopupMenuItem::new(place.name.clone())
                    .icon(Lucide::Folder)
                    .action(Box::new(GoToPlace { index: ix }))
                    .on_click(move |_, _, cx| pane_command(side, Command::GoToPlace(ix), cx)),
            );
        }
    }
    menu = menu
        .item(
            PopupMenuItem::new(if pane.place_saved { "Remove This Folder" } else { "Save This Folder" })
                .action(Box::new(TogglePlace))
                .disabled(in_archive)
                .on_click(move |_, _, cx| pane_command(side, Command::TogglePlace, cx)),
        )
        .separator()
        .label("This Mac");
    for folder in shell.local_folders.iter() {
        let path = folder.path.clone();
        menu = menu.item(
            PopupMenuItem::new(folder.label.clone())
                .icon(folder_icon(&folder.label, folder.volume))
                .on_click(move |_, _, cx| pane_input(side, PaneInput::OpenLocal(path.clone()), cx)),
        );
    }

    menu = menu.separator().label("Bookmarks");
    if shell.bookmarks.bookmarks.is_empty() {
        menu = menu.item(PopupMenuItem::new("No bookmarks yet").disabled(true));
    }
    for bookmark in &shell.bookmarks.bookmarks {
        let id = bookmark.id.clone();
        let icon = if bookmark.plugin == LOCAL_PLUGIN { Lucide::FolderHeart } else { Lucide::Server };
        menu = menu.item(
            PopupMenuItem::new(bookmark.name.clone())
                .icon(icon)
                .checked(shell.connected.contains(&bookmark.id))
                .on_click(move |_, _, cx| pane_input(side, PaneInput::OpenBookmark(id.clone()), cx)),
        );
    }
    menu = menu
        .separator()
        .item(
            PopupMenuItem::new("New Bookmark…")
                .icon(Lucide::BookmarkPlus)
                .on_click(move |_, _, cx| pane_command(side, Command::NewBookmark, cx)),
        )
        .item(
            PopupMenuItem::new("New Bookmark for This Folder…")
                .action(Box::new(BookmarkConnection))
                .disabled(in_archive)
                .on_click(move |_, _, cx| pane_command(side, Command::BookmarkConnection, cx)),
        )
        .item(
            PopupMenuItem::new("Manage Bookmarks…")
                .icon(Lucide::Bookmark)
                .on_click(move |_, _, cx| pane_command(side, Command::ManageBookmarks, cx)),
        );
    if pane.kind != LocationKind::Local {
        menu = menu.separator().item(
            PopupMenuItem::new("Disconnect")
                .icon(Lucide::Unplug)
                .on_click(move |_, _, cx| pane_input(side, PaneInput::Disconnect, cx)),
        );
    }
    menu
}

/// Registers the pane's own commands: each sends its core command.
macro_rules! pane_actions {
    ($el:expr, $cx:expr, [$($action:ident => $cmd:expr),* $(,)?]) => {
        $el$(.on_action($cx.listener(|this: &mut FilePane, _: &$action, _, cx| this.cmd($cmd, cx))))*
    };
}

impl Render for FilePane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.contains_focused(window, cx);
        let theme = cx.theme().clone();
        let side = self.side;
        let v = &self.view;
        let count = v.order.len();
        let error = v.error.clone();
        let disconnected = v.disconnected;
        let filter: SharedString = v.filter.clone().into();
        // What the filter is hiding, so the count is the reason the list
        // looks short.
        let shown = v.order.iter().filter(|ix| **ix != PARENT).count();
        let counts: SharedString = format!("{shown} of {}", v.unfiltered).into();
        let cursor_entry = match v.order.get(v.cursor) {
            Some(&PARENT) | None => None,
            Some(&ix) => v.listing.entries.get(ix as usize),
        };
        let selection_count = if v.marked.is_empty() {
            usize::from(cursor_entry.is_some())
        } else {
            (v.marked_totals.dirs + v.marked_totals.files) as usize
        };
        let is_local = v.kind == LocationKind::Local;
        // Reveal works for local archives too (it shows the archive file).
        let local_host = v.host_local;
        let on_archive =
            cursor_entry.is_some_and(|e| tl_app::pane::is_archive(e) && v.kind != LocationKind::Archive);
        let show_places = !v.places.is_empty();
        let show_tabs = v.tab_titles.len() > 1;
        let scroll = self.scroll();
        let this = cx.weak_entity();

        let list = div()
            .id("list-area")
            .relative()
            .flex_1()
            .min_h_0()
            .child(
                uniform_list(
                    "rows",
                    count,
                    cx.processor(move |this, range: Range<usize>, _window, cx| {
                        range.map(|ix| this.render_row(ix, focused, cx)).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&scroll)
                .size_full(),
            )
            .vertical_scrollbar(&scroll)
            .child(
                // Measure the width to drop columns in narrow panes.
                canvas(
                    move |bounds, _window, cx| {
                        let width = f32::from(bounds.size.width);
                        let narrow = if width < 360. {
                            3
                        } else if width < 470. {
                            2
                        } else if width < 590. {
                            1
                        } else {
                            0
                        };
                        let crumb_chars = (((width - 150.) / 7.5).max(8.) as usize / 4) * 4;
                        let origin = bounds.origin;
                        let _ = this.update(cx, |this, cx| {
                            this.list_origin = origin;
                            if this.narrow != narrow || this.crumb_chars != crumb_chars {
                                this.narrow = narrow;
                                this.crumb_chars = crumb_chars;
                                cx.notify();
                            }
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .when_some(error.filter(|_| count <= 1), |el, error| {
                el.child(
                    v_flex()
                        .absolute()
                        .top(px(40.))
                        .left_0()
                        .right_0()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .child(Icon::new(IconName::TriangleAlert).text_color(theme.warning))
                        .child(div().text_sm().text_center().text_color(theme.muted_foreground).child(error))
                        .child(
                            Button::new("retry")
                                .label("Retry")
                                .small()
                                .on_click(move |_, _, cx| pane_input(side, PaneInput::Retry, cx)),
                        ),
                )
            })
            .when(disconnected, |el| {
                el.child(
                    v_flex()
                        .absolute()
                        .inset_0()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .bg(theme.background.opacity(0.85))
                        .child(Icon::new(Lucide::Unplug).text_color(theme.warning))
                        .child(div().text_sm().child("Connection closed"))
                        .child(
                            Button::new("reconnect")
                                .label("Reconnect")
                                .primary()
                                .small()
                                .on_click(move |_, _, cx| pane_input(side, PaneInput::Reconnect, cx)),
                        ),
                )
            })
            .when(!filter.is_empty() && shown == 0, |el| {
                el.child(
                    v_flex()
                        .absolute()
                        .top(px(40.))
                        .left_0()
                        .right_0()
                        .items_center()
                        .gap_1()
                        .px_4()
                        .child(Icon::new(IconName::Search).text_color(theme.muted_foreground))
                        .child(
                            div()
                                .text_sm()
                                .text_center()
                                .text_color(theme.muted_foreground)
                                .child(format!("Nothing here matches “{filter}”")),
                        )
                        .child(div().text_xs().text_color(theme.muted_foreground).child("Esc shows everything again")),
                )
            })
            .when(!filter.is_empty(), |el| {
                el.child(
                    h_flex()
                        .absolute()
                        .bottom(px(6.))
                        .left(px(8.))
                        .gap_1()
                        .px_2()
                        .py(px(2.))
                        .rounded_md()
                        .bg(theme.popover)
                        .border_1()
                        .border_color(accent(&theme))
                        .shadow_md()
                        .text_size(px(12.))
                        .child(Icon::new(IconName::Search).xsmall().text_color(theme.muted_foreground))
                        .child(filter.clone())
                        .child(div().text_color(theme.muted_foreground).child(counts)),
                )
            })
            .drag_over::<DraggedFiles>(move |style, drag, _, cx| {
                if drag.side != side { style.bg(cx.theme().drop_target) } else { style }
            })
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.bg(cx.theme().drop_target))
            .on_drop(move |drag: &DraggedFiles, window: &mut Window, cx: &mut App| {
                if drag.side != side {
                    // ⇧ or ⌘ held at the drop moves instead of copying.
                    let mods = window.modifiers();
                    let mode = if mods.shift || mods.platform { TransferMode::Move } else { TransferMode::Copy };
                    pane_input(side, PaneInput::Drop { drag: drag.dragged(), mode }, cx);
                }
            })
            .on_drop(move |paths: &ExternalPaths, _window: &mut Window, cx: &mut App| {
                let paths = paths.paths().iter().map(|p| VPath::from_local(p)).collect();
                dispatch(Event::DropExternal { side, paths }, cx);
            })
            .context_menu(move |menu, _window, _cx| {
                let one = selection_count == 1;
                menu.min_w(px(220.))
                    .menu("Open", Box::new(Open))
                    .menu_with_disabled("View", Box::new(ViewFile), !one)
                    .menu_with_disabled("Edit", Box::new(EditFile), !one)
                    .menu("Open Externally", Box::new(OpenExternally))
                    .separator()
                    .menu_with_disabled("Copy to Other Pane…", Box::new(CopyFiles), selection_count == 0)
                    .menu_with_disabled("Move to Other Pane…", Box::new(MoveFiles), selection_count == 0)
                    .menu_with_disabled("Rename…", Box::new(RenameFile), !one)
                    .menu_with_disabled("Multi-Rename…", Box::new(MultiRename), selection_count == 0)
                    .separator()
                    .when(on_archive, |m| {
                        m.menu("Open Archive", Box::new(OpenArchive))
                            .menu("Decompress…", Box::new(Decompress))
                            .menu("Extract to Other Pane…", Box::new(ExtractArchive))
                    })
                    .menu_with_disabled("Compress…", Box::new(Compress), selection_count == 0)
                    .menu_with_disabled("Pack into Archive in Other Pane…", Box::new(PackFiles), selection_count == 0)
                    .separator()
                    .menu("Copy", Box::new(ClipboardCopy))
                    .menu("Cut", Box::new(ClipboardCut))
                    .menu("Paste", Box::new(ClipboardPaste))
                    .menu("Copy Path", Box::new(CopyPath))
                    .separator()
                    .menu("New Folder…", Box::new(NewFolder))
                    .menu("New File…", Box::new(NewFile))
                    .menu("Calculate Size", Box::new(CalcSize))
                    .menu("Properties…", Box::new(Properties))
                    .separator()
                    .when(local_host, |m| m.menu(crate::util::REVEAL_LABEL, Box::new(RevealInFinder)))
                    .menu("Open Terminal Here", Box::new(OpenTerminal))
                    .separator()
                    .when(is_local, |m| {
                        m.menu_with_disabled("Move to Trash", Box::new(TrashFiles), selection_count == 0)
                    })
                    .menu_with_disabled("Delete Permanently…", Box::new(DeleteFiles), selection_count == 0)
            });

        let el = v_flex()
            .id(("pane", side.index()))
            .key_context(PANE)
            .track_focus(&self.focus)
            .size_full()
            .bg(theme.background)
            .on_action(cx.listener(|this, _: &PageUp, _, cx| {
                let rows = this.page_rows();
                this.cmd(Command::PageUp { rows }, cx);
            }))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| {
                let rows = this.page_rows();
                this.cmd(Command::PageDown { rows }, cx);
            }))
            .on_action(cx.listener(|this, action: &GoToPlace, _, cx| this.cmd(Command::GoToPlace(action.index), cx)))
            .on_key_down(cx.listener(Self::on_key));
        pane_actions!(el, cx, [
            CursorUp => Command::CursorUp,
            CursorDown => Command::CursorDown,
            CursorHome => Command::CursorHome,
            CursorEnd => Command::CursorEnd,
            MarkUp => Command::MarkUp,
            MarkDown => Command::MarkDown,
            ToggleMark => Command::ToggleMark,
            MarkAll => Command::MarkAll,
            UnmarkAll => Command::UnmarkAll,
            InvertMarks => Command::InvertMarks,
            MarkByMask => Command::MarkByMask,
            UnmarkByMask => Command::UnmarkByMask,
            Open => Command::Open,
            OpenArchive => Command::OpenArchive,
            GoParent => Command::GoParent,
            FilterBackspace => Command::FilterBackspace,
            GoBack => Command::GoBack,
            GoForward => Command::GoForward,
            GoHome => Command::GoHome,
            GoToPath => Command::GoToPath,
            Refresh => Command::Refresh,
            Locations => Command::Locations(None),
            TogglePlace => Command::TogglePlace,
            NewTab => Command::NewTab,
            CloseTab => Command::CloseTab,
            NextTab => Command::NextTab,
            PrevTab => Command::PrevTab,
        ])
        .child(self.render_header(focused, cx))
        .when(show_tabs, |el| el.child(self.render_tabs()))
        .when(show_places, |el| el.child(self.render_places(cx)))
        .child(self.render_columns(cx))
        .child(list)
        .child(self.render_status(cx))
        .when_some(self.popup.as_ref().map(|(m, _)| m.clone()), |el, menu| {
            el.child(deferred(anchored().position(self.list_origin).snap_to_window().child(menu)).with_priority(1))
        })
    }
}
