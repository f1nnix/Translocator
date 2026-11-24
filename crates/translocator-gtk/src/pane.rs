//! One file pane: the location line (locations menu, breadcrumbs, spinner,
//! sudo), the tab strip, the row of saved folders, the file list with its
//! overlays, and the status line.
//!
//! Everything is drawn from the pane's snapshot and redrawn only where the
//! snapshot changed; every click becomes core input.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use tl_ffi::Core;
use tl_ffi::input::{Command, Input, PaneInput};
use tl_ffi::snapshot::PaneSnapshot;
use tl_ffi::types::{LocationKind, Side, ViewMode};

use crate::file_list::FileList;
use crate::keys::KeyMap;
use crate::menus::{self, MenuActions};
use crate::shell;

pub struct PaneView {
    pub side: Side,
    pub root: gtk::Box,
    header: gtk::Box,
    location: gtk::MenuButton,
    location_icon: gtk::Image,
    location_label: gtk::Label,
    crumbs: gtk::Box,
    spinner: gtk::Spinner,
    sudo: gtk::Button,
    /// List, columns, icons, and whether they are being set from a
    /// snapshot.
    view_buttons: (Vec<(ViewMode, gtk::ToggleButton)>, Rc<Cell<bool>>),
    tabs_bar: gtk::ScrolledWindow,
    tabs: gtk::Box,
    places_bar: gtk::Box,
    places: gtk::Box,
    place_toggle: gtk::Button,
    pub list: Rc<FileList>,
    filter_chip: gtk::Box,
    filter_text: gtk::Label,
    filter_count: gtk::Label,
    empty: gtk::Box,
    empty_title: gtk::Label,
    error: adw::StatusPage,
    disconnected: gtk::Box,
    status: gtk::Label,
    free: gtk::Label,
    /// The snapshot the pane shows.
    shown: RefCell<Option<PaneSnapshot>>,
    /// The key labels the tooltips were written with.
    keys_seen: RefCell<u64>,
}

impl PaneView {
    pub fn new(core: Arc<Core>, side: Side) -> Rc<PaneView> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("tl-pane");
        root.set_size_request(280, -1);
        root.update_property(&[gtk::accessible::Property::Label(match side {
            Side::Left => "Left pane",
            Side::Right => "Right pane",
        })]);

        // ----- the location line -----
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        header.add_css_class("tl-pane-header");
        let location_icon = gtk::Image::from_icon_name("drive-harddisk-symbolic");
        let location_label = gtk::Label::new(Some("Local"));
        let location_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        location_content.append(&location_icon);
        location_content.append(&location_label);
        let location = gtk::MenuButton::builder()
            .child(&location_content)
            .always_show_arrow(true)
            .tooltip_text("Locations")
            .build();
        location.add_css_class("flat");
        location.set_focus_on_click(false);
        location.connect_active_notify(|button| {
            if !button.is_active() {
                shell::restore_focus_soon();
            }
        });
        location.update_property(&[gtk::accessible::Property::Label("Location")]);

        let root_crumb = gtk::Button::with_label("/");
        root_crumb.add_css_class("flat");
        root_crumb.add_css_class("tl-crumb");
        root_crumb.set_focus_on_click(false);
        root_crumb.connect_clicked(move |_| {
            shell::send(Input::Pane {
                side,
                input: PaneInput::Navigate { path: b"/".to_vec() },
            })
        });
        let crumbs = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let crumbs_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(true)
            .hexpand(true)
            .child(&crumbs)
            .build();
        // Long paths lose their beginning, not their end: keep the strip
        // scrolled to the last component.
        let hadj = crumbs_scroll.hadjustment();
        hadj.connect_changed(|adj| adj.set_value(adj.upper() - adj.page_size()));
        let path_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        path_box.set_hexpand(true);
        path_box.append(&root_crumb);
        path_box.append(&crumbs_scroll);
        path_box.update_property(&[gtk::accessible::Property::Label("Path")]);

        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        spinner.update_property(&[gtk::accessible::Property::Label("Loading")]);
        let sudo = gtk::Button::from_icon_name("changes-prevent-symbolic");
        sudo.add_css_class("flat");
        sudo.set_focus_on_click(false);
        sudo.set_visible(false);
        sudo.connect_clicked(move |_| shell::command_on(side, Command::ToggleSudo));
        // How the pane shows its folder.
        let views = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        views.add_css_class("linked");
        let mut view_buttons: Vec<(ViewMode, gtk::ToggleButton)> = Vec::new();
        // Set while the buttons are made to show the snapshot's view, so
        // that is not taken for a choice.
        let showing_view = Rc::new(Cell::new(false));
        for (mode, icons, label) in [
            (ViewMode::List, &["view-list-symbolic"][..], "as List"),
            (ViewMode::Columns, &["view-columns-symbolic", "view-dual-symbolic"][..], "as Columns"),
            (ViewMode::Grid, &["view-grid-symbolic"][..], "as Icons"),
        ] {
            let button = gtk::ToggleButton::new();
            button.set_child(Some(&gtk::Image::from_gicon(&gtk::gio::ThemedIcon::from_names(icons))));
            button.add_css_class("flat");
            button.set_focus_on_click(false);
            button.set_tooltip_text(Some(&format!("View {label}")));
            button.update_property(&[gtk::accessible::Property::Label(&format!("View {label}"))]);
            if let Some((_, first)) = view_buttons.first() {
                button.set_group(Some(first));
            }
            let showing = showing_view.clone();
            button.connect_toggled(move |button| {
                if button.is_active() && !showing.get() {
                    shell::send(Input::Pane {
                        side,
                        input: PaneInput::ViewAs { mode },
                    });
                    shell::restore_focus_soon();
                }
            });
            views.append(&button);
            view_buttons.push((mode, button));
        }
        let view_buttons = (view_buttons, showing_view);
        header.append(&location);
        header.append(&path_box);
        header.append(&spinner);
        header.append(&views);
        header.append(&sudo);

        // ----- tabs -----
        let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        tabs.add_css_class("tl-tabs");
        let tabs_bar = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&tabs)
            .visible(false)
            .build();

        // ----- saved folders -----
        let places_bar = gtk::Box::new(gtk::Orientation::Horizontal, 3);
        places_bar.add_css_class("tl-places");
        places_bar.set_visible(false);
        let places_icon = gtk::Image::from_icon_name("user-bookmarks-symbolic");
        places_icon.add_css_class("dim-label");
        let places = gtk::Box::new(gtk::Orientation::Horizontal, 3);
        let places_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .child(&places)
            .build();
        let place_toggle = gtk::Button::from_icon_name("non-starred-symbolic");
        place_toggle.add_css_class("flat");
        place_toggle.set_focus_on_click(false);
        place_toggle.connect_clicked(move |_| shell::command_on(side, Command::TogglePlace));
        places_bar.append(&places_icon);
        places_bar.append(&places_scroll);
        places_bar.append(&place_toggle);

        // ----- the list and what lies over it -----
        let list = FileList::new(core, side);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&list.root));
        overlay.set_vexpand(true);

        let filter_chip = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        filter_chip.add_css_class("osd");
        filter_chip.add_css_class("tl-chip");
        filter_chip.set_halign(gtk::Align::Start);
        filter_chip.set_valign(gtk::Align::End);
        filter_chip.set_margin_start(8);
        filter_chip.set_margin_bottom(8);
        filter_chip.set_can_target(false);
        filter_chip.set_visible(false);
        let filter_text = gtk::Label::new(None);
        let filter_count = gtk::Label::new(None);
        filter_count.add_css_class("dim-label");
        filter_chip.append(&gtk::Image::from_icon_name("system-search-symbolic"));
        filter_chip.append(&filter_text);
        filter_chip.append(&filter_count);

        // "Nothing matches": small, under the ".." row.
        let empty = gtk::Box::new(gtk::Orientation::Vertical, 4);
        empty.set_valign(gtk::Align::Start);
        empty.set_margin_top(64);
        empty.set_can_target(false);
        empty.set_visible(false);
        let empty_icon = gtk::Image::from_icon_name("system-search-symbolic");
        empty_icon.set_pixel_size(32);
        empty_icon.add_css_class("dim-label");
        let empty_title = gtk::Label::new(None);
        empty_title.add_css_class("title-4");
        empty_title.set_wrap(true);
        empty_title.set_justify(gtk::Justification::Center);
        let empty_hint = gtk::Label::new(Some("Esc shows everything again"));
        empty_hint.add_css_class("dim-label");
        empty.append(&empty_icon);
        empty.append(&empty_title);
        empty.append(&empty_hint);

        let retry = gtk::Button::with_label("Retry");
        retry.add_css_class("pill");
        retry.set_halign(gtk::Align::Center);
        retry.connect_clicked(move |_| {
            shell::send(Input::Pane {
                side,
                input: PaneInput::Retry,
            })
        });
        let error = adw::StatusPage::builder()
            .icon_name("dialog-warning-symbolic")
            .valign(gtk::Align::Start)
            .child(&retry)
            .visible(false)
            .build();
        error.add_css_class("compact");

        let reconnect = gtk::Button::with_label("Reconnect");
        reconnect.add_css_class("pill");
        reconnect.add_css_class("suggested-action");
        reconnect.set_halign(gtk::Align::Center);
        reconnect.connect_clicked(move |_| {
            shell::send(Input::Pane {
                side,
                input: PaneInput::Reconnect,
            })
        });
        let closed = adw::StatusPage::builder()
            .icon_name("network-offline-symbolic")
            .title("Connection closed")
            .child(&reconnect)
            .vexpand(true)
            .build();
        closed.add_css_class("compact");
        let disconnected = gtk::Box::new(gtk::Orientation::Vertical, 0);
        disconnected.add_css_class("tl-cover");
        disconnected.append(&closed);
        disconnected.set_visible(false);

        overlay.add_overlay(&empty);
        overlay.add_overlay(&error);
        overlay.add_overlay(&filter_chip);
        overlay.add_overlay(&disconnected);

        // ----- the status line -----
        let status_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        status_bar.add_css_class("tl-status");
        let status = gtk::Label::new(None);
        status.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        status.set_xalign(0.0);
        status.set_hexpand(true);
        let free = gtk::Label::new(None);
        status_bar.append(&status);
        status_bar.append(&free);

        root.append(&header);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&tabs_bar);
        root.append(&places_bar);
        root.append(&overlay);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&status_bar);

        let pane = Rc::new(PaneView {
            side,
            root,
            header,
            location,
            location_icon,
            location_label,
            crumbs,
            spinner,
            sudo,
            view_buttons,
            tabs_bar,
            tabs,
            places_bar,
            places,
            place_toggle,
            list,
            filter_chip,
            filter_text,
            filter_count,
            empty,
            empty_title,
            error,
            disconnected,
            status,
            free,
            shown: RefCell::new(None),
            keys_seen: RefCell::new(0),
        });

        // The locations menu is built from the snapshot each time it opens.
        let weak = Rc::downgrade(&pane);
        pane.location.set_create_popup_func(move |button| {
            if weak.upgrade().is_none() {
                return;
            }
            shell::send(Input::Pane {
                side,
                input: PaneInput::LocationsShown,
            });
            let mut actions = MenuActions::new("loc");
            let menu = shell::with(|s| menus::locations(side, &s.snapshot.borrow(), &s.keys.borrow(), &mut actions));
            button.insert_action_group("loc", Some(&actions.group));
            button.set_menu_model(Some(&menu));
        });
        pane
    }

    /// Opens the locations menu (⌘K, ⌥F1, ⌥F2).
    pub fn popup_locations(&self) {
        self.location.popup();
    }

    pub fn set_focused(&self, focused: bool) {
        if focused {
            self.header.add_css_class("focused");
        } else {
            self.header.remove_css_class("focused");
        }
        self.list.set_focused(focused);
    }

    /// Takes in the pane's snapshot.
    pub fn apply(&self, pane: &PaneSnapshot, keys: &KeyMap, keys_version: u64) {
        let old = self.shown.replace(Some(pane.clone()));
        let old = old.as_ref();
        let keys_changed = self.keys_seen.replace(keys_version) != keys_version;

        if old.is_none_or(|o| o.location_label != pane.location_label || o.kind != pane.kind) {
            self.location_label.set_text(&pane.location_label);
            self.location_icon.set_icon_name(Some(match pane.kind {
                LocationKind::Local => "drive-harddisk-symbolic",
                LocationKind::Remote => "network-server-symbolic",
                LocationKind::Archive => "package-x-generic-symbolic",
            }));
            self.location
                .update_property(&[gtk::accessible::Property::Label(&format!("Location: {}", pane.location_label))]);
        }
        if old.is_none_or(|o| o.crumbs != pane.crumbs || o.elevated != pane.elevated) {
            self.show_crumbs(pane);
        }
        self.spinner.set_visible(pane.loading);
        self.spinner.set_spinning(pane.loading);

        // sudo
        self.sudo.set_visible(pane.caps.elevation);
        if old.is_none_or(|o| o.elevated != pane.elevated) || keys_changed {
            let key = keys.label("toggle-sudo").map(|k| format!(" ({k})")).unwrap_or_default();
            if pane.elevated {
                self.sudo.set_icon_name("changes-allow-symbolic");
                self.sudo.add_css_class("error");
                self.sudo
                    .set_tooltip_text(Some("Running as root (sudo). Click to drop privileges."));
                self.sudo
                    .update_property(&[gtk::accessible::Property::Label("Drop root privileges")]);
            } else {
                self.sudo.set_icon_name("changes-prevent-symbolic");
                self.sudo.remove_css_class("error");
                self.sudo
                    .set_tooltip_text(Some(&format!("Elevate this pane with sudo{key}")));
                self.sudo
                    .update_property(&[gtk::accessible::Property::Label("Work as root with sudo")]);
            }
        }

        // tabs
        if old.is_none_or(|o| o.tab_titles != pane.tab_titles || o.active_tab != pane.active_tab) {
            self.show_tabs(pane);
        }
        // saved folders
        if old.is_none_or(|o| o.places != pane.places || o.place_saved != pane.place_saved || o.kind != pane.kind)
            || keys_changed
        {
            self.show_places(pane, keys);
        }

        let (buttons, showing) = &self.view_buttons;
        showing.set(true);
        for (mode, button) in buttons {
            if *mode == pane.view_mode && !button.is_active() {
                button.set_active(true);
            }
        }
        showing.set(false);
        self.list.apply(old, pane);

        // overlays
        let filtering = !pane.filter.is_empty();
        self.filter_chip.set_visible(filtering);
        if filtering {
            self.filter_text.set_text(&pane.filter);
            self.filter_count.set_text(&format!("{} of {}", pane.shown, pane.unfiltered));
            self.filter_chip.update_property(&[gtk::accessible::Property::Label(&format!(
                "Filter {}, {} of {}",
                pane.filter, pane.shown, pane.unfiltered
            ))]);
        }
        let nothing = filtering && pane.shown == 0;
        self.empty.set_visible(nothing);
        if nothing {
            self.empty_title
                .set_text(&format!("Nothing here matches “{}”", pane.filter));
        }
        let error = pane.error.as_ref().filter(|_| pane.row_count <= 1);
        self.error.set_visible(error.is_some());
        if let Some(error) = error {
            self.error.set_description(Some(&glib::markup_escape_text(error)));
        }
        self.disconnected.set_visible(pane.disconnected);

        // status line
        self.status.set_text(&pane.status);
        self.free.set_text(pane.free.as_deref().unwrap_or(""));
    }

    fn show_crumbs(&self, pane: &PaneSnapshot) {
        while let Some(child) = self.crumbs.first_child() {
            self.crumbs.remove(&child);
        }
        let side = self.side;
        let last = pane.crumbs.len().saturating_sub(1);
        for (ix, crumb) in pane.crumbs.iter().enumerate() {
            if ix > 0 {
                let sep = gtk::Label::new(Some("/"));
                sep.add_css_class("dim-label");
                self.crumbs.append(&sep);
            }
            let button = gtk::Button::with_label(&crumb.name);
            button.add_css_class("flat");
            button.add_css_class("tl-crumb");
            button.set_focus_on_click(false);
            if ix == last {
                button.add_css_class("current");
                if pane.elevated {
                    button.add_css_class("error");
                }
            }
            let path = crumb.path.clone();
            button.connect_clicked(move |_| {
                shell::send(Input::Pane {
                    side,
                    input: PaneInput::Navigate { path: path.clone() },
                })
            });
            self.crumbs.append(&button);
        }
    }

    fn show_tabs(&self, pane: &PaneSnapshot) {
        while let Some(child) = self.tabs.first_child() {
            self.tabs.remove(&child);
        }
        let side = self.side;
        self.tabs_bar.set_visible(pane.tab_titles.len() > 1);
        if pane.tab_titles.len() <= 1 {
            return;
        }
        for (ix, title) in pane.tab_titles.iter().enumerate() {
            let tab = gtk::Box::new(gtk::Orientation::Horizontal, 2);
            tab.add_css_class("tl-tab");
            if ix as u32 == pane.active_tab {
                tab.add_css_class("current");
            }
            let select = gtk::Button::with_label(title);
            select.add_css_class("flat");
            select.set_focus_on_click(false);
            select.connect_clicked(move |_| {
                shell::send(Input::Pane {
                    side,
                    input: PaneInput::SelectTab { index: ix as u32 },
                })
            });
            let close = gtk::Button::from_icon_name("window-close-symbolic");
            close.add_css_class("flat");
            close.add_css_class("circular");
            close.add_css_class("tl-tab-close");
            close.set_focus_on_click(false);
            close.update_property(&[gtk::accessible::Property::Label(&format!("Close {title}"))]);
            close.connect_clicked(move |_| {
                shell::send(Input::Pane {
                    side,
                    input: PaneInput::CloseTab { index: ix as u32 },
                })
            });
            tab.append(&select);
            tab.append(&close);
            self.tabs.append(&tab);
        }
    }

    fn show_places(&self, pane: &PaneSnapshot, keys: &KeyMap) {
        while let Some(child) = self.places.first_child() {
            self.places.remove(&child);
        }
        let side = self.side;
        self.places_bar.set_visible(!pane.places.is_empty());
        let count = pane.places.len();
        for (ix, place) in pane.places.iter().enumerate() {
            let button = gtk::Button::with_label(&place.name);
            button.add_css_class("flat");
            button.add_css_class("tl-place");
            button.set_focus_on_click(false);
            if place.current {
                button.add_css_class("current");
            }
            let key = (ix < 9).then(|| keys.label(&format!("go-to-place-{}", ix + 1))).flatten();
            let tip = match key {
                Some(key) => format!("{}  {key}", place.display),
                None => place.display.clone(),
            };
            button.set_tooltip_text(Some(&tip));
            let path = place.path.clone();
            button.connect_clicked(move |_| {
                shell::send(Input::Pane {
                    side,
                    input: PaneInput::Navigate { path: path.clone() },
                })
            });
            let menu_click = gtk::GestureClick::new();
            menu_click.set_button(gdk::BUTTON_SECONDARY);
            let id = place.id.clone();
            menu_click.connect_pressed(move |gesture, _, x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let Some(widget) = gesture.widget() else { return };
                let mut actions = MenuActions::new("place");
                let menu = menus::place(side, ix, count, &id, &mut actions);
                menus::popup_at(&widget, &menu, actions, x, y);
            });
            button.add_controller(menu_click);
            self.places.append(&button);
        }
        let key = keys.label("toggle-place").map(|k| format!(" ({k})")).unwrap_or_default();
        if pane.place_saved {
            self.place_toggle.set_icon_name("starred-symbolic");
            self.place_toggle
                .set_tooltip_text(Some(&format!("Take this folder off the row{key}")));
            self.place_toggle.update_property(&[gtk::accessible::Property::Label(
                "Remove this folder from the saved folders",
            )]);
        } else {
            self.place_toggle.set_icon_name("non-starred-symbolic");
            self.place_toggle
                .set_tooltip_text(Some(&format!("Save this folder on the row{key}")));
            self.place_toggle
                .update_property(&[gtk::accessible::Property::Label("Save this folder")]);
        }
        self.place_toggle.set_sensitive(pane.kind != LocationKind::Archive);
    }
}
