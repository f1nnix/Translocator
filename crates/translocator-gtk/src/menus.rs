//! The menus, as `gio::Menu` models: the primary menu (the GPUI shell's
//! menu bar, as submenus and sections), a pane's locations menu, the file
//! list's context menu and a saved folder's menu.
//!
//! The primary menu's items are the window's actions (`win.<command id>`,
//! see [`crate::actions`]), so they show the accelerators they answer to.
//! The other menus are built from the snapshot when they open, with a
//! throwaway action group of their own ([`MenuActions`]): their items act
//! on one pane, and some are disabled or checked by what that pane shows.

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};
use tl_ffi::input::{Command, Input, PaneInput};
use tl_ffi::snapshot::{PaneSnapshot, Snapshot};
use tl_ffi::types::{LocationKind, Side};

use crate::keys::KeyMap;
use crate::shell;

/// A menu item for `action`, showing `accel` when there is one.
fn item(label: &str, action: &str, accel: Option<String>) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), Some(action));
    if let Some(accel) = accel {
        item.set_attribute_value("accel", Some(&accel.to_variant()));
    }
    item
}

/// The primary menu: every command of the GPUI shell's menu bar.
pub fn primary(keys: &KeyMap) -> gio::Menu {
    let win = |label: &str, id: &str| item(label, &format!("win.{id}"), keys.menu_accel(id));
    let section = |items: &[(&str, &str)]| {
        let menu = gio::Menu::new();
        for (label, id) in items {
            menu.append_item(&win(label, id));
        }
        menu
    };
    let submenu = |sections: &[&[(&str, &str)]]| {
        let menu = gio::Menu::new();
        for items in sections {
            menu.append_section(None, &section(items));
        }
        menu
    };

    let root = gio::Menu::new();
    let menus = gio::Menu::new();
    menus.append_submenu(
        Some("File"),
        &submenu(&[
            &[("New Tab", "new-tab"), ("New Folder…", "new-folder"), ("New File…", "new-file")],
            &[
                ("View", "view-file"),
                ("Edit", "edit-file"),
                ("Open Externally", "open-externally"),
                ("Show in File Manager", "reveal-in-finder"),
            ],
            &[
                ("Copy to Other Pane…", "copy-files"),
                ("Move to Other Pane…", "move-files"),
                ("Rename…", "rename-file"),
                ("Multi-Rename…", "multi-rename"),
                ("Move to Trash", "trash-files"),
                ("Delete Permanently…", "delete-files"),
            ],
            &[
                ("Properties…", "properties"),
                ("Calculate Folder Size", "calc-size"),
                ("Copy Path", "copy-path"),
            ],
            &[("Copy", "clipboard-copy"), ("Cut", "clipboard-cut"), ("Paste", "clipboard-paste")],
            &[("Close Tab", "close-tab")],
        ]),
    );
    menus.append_submenu(
        Some("Commands"),
        &submenu(&[
            &[
                ("Compare Directories", "compare-dirs"),
                ("Synchronize Directories…", "sync-dirs"),
                ("Multi-Rename…", "multi-rename"),
            ],
            &[
                ("Open Archive", "open-archive"),
                ("Compress…", "compress"),
                ("Decompress…", "decompress"),
                ("Pack into Archive in Other Pane…", "pack-files"),
                ("Extract Archive to Other Pane…", "extract-archive"),
            ],
            &[("Find Files…", "find-files"), ("Calculate Folder Size", "calc-size")],
        ]),
    );
    menus.append_submenu(
        Some("Go"),
        &submenu(&[
            &[
                ("Back", "go-back"),
                ("Forward", "go-forward"),
                ("Enclosing Folder", "go-parent"),
                ("Home", "go-home"),
                ("Go to Folder…", "go-to-path"),
                ("Find Files…", "find-files"),
                ("Locations…", "locations"),
            ],
            &[("Open Terminal Here", "open-terminal"), ("Command Line", "focus-command-line")],
        ]),
    );
    menus.append_submenu(
        Some("View"),
        &submenu(&[
            &[("Show Hidden Files", "toggle-hidden"), ("Refresh", "refresh")],
            &[
                ("Quick Look", "quick-look"),
                ("Swap Panes", "swap-panes"),
                ("Same Folder in Other Pane", "mirror-pane"),
                ("Other Pane", "switch-pane"),
            ],
            &[("Next Tab", "next-tab"), ("Previous Tab", "prev-tab")],
            &[("Jobs", "toggle-jobs"), ("Console", "toggle-console")],
        ]),
    );
    menus.append_submenu(
        Some("Connection"),
        &submenu(&[
            &[("Save This Folder", "toggle-place")],
            &[
                ("New Bookmark…", "new-bookmark"),
                ("New Bookmark for This Folder…", "bookmark-connection"),
                ("Manage Bookmarks…", "manage-bookmarks"),
            ],
            &[("Work as Root (sudo)", "toggle-sudo"), ("Disconnect", "disconnect")],
        ]),
    );
    menus.append_submenu(
        Some("Selection"),
        &submenu(&[&[
            ("Mark / Unmark", "toggle-mark"),
            ("Mark All", "mark-all"),
            ("Unmark All", "unmark-all"),
            ("Invert Marks", "invert-marks"),
            ("Mark by Mask…", "mark-by-mask"),
            ("Unmark by Mask…", "unmark-by-mask"),
        ]]),
    );
    root.append_section(None, &menus);

    let app = gio::Menu::new();
    app.append_item(&win("Preferences", "open-settings"));
    app.append_item(&item("Keyboard Shortcuts", "win.show-help-overlay", Some("<Control>question".into())));
    app.append_item(&item("Help", "app.help", None));
    app.append_item(&item("About Translocator", "app.about", None));
    root.append_section(None, &app);
    let quit = gio::Menu::new();
    quit.append_item(&win("Quit", "quit"));
    root.append_section(None, &quit);
    root
}

/// The actions of a menu built for one opening: `prefix.a<n>`, inserted
/// on the widget that shows the menu.
pub struct MenuActions {
    pub prefix: &'static str,
    pub group: gio::SimpleActionGroup,
    next: usize,
}

impl MenuActions {
    pub fn new(prefix: &'static str) -> MenuActions {
        MenuActions {
            prefix,
            group: gio::SimpleActionGroup::new(),
            next: 0,
        }
    }

    /// An action that runs `f`; `checked` makes it a check item.
    pub fn add(&mut self, enabled: bool, checked: Option<bool>, f: impl Fn() + 'static) -> String {
        let name = format!("a{}", self.next);
        self.next += 1;
        let action = match checked {
            Some(on) => gio::SimpleAction::new_stateful(&name, None, &on.to_variant()),
            None => gio::SimpleAction::new(&name, None),
        };
        action.set_enabled(enabled);
        let f = Rc::new(f);
        action.connect_activate(move |_, _| f());
        self.group.add_action(&action);
        format!("{}.{name}", self.prefix)
    }
}

/// A command acting on `side`.
fn on(side: Side, command: Command) -> impl Fn() + 'static {
    move || shell::command_on(side, command.clone())
}

fn pane_input(side: Side, input: PaneInput) -> impl Fn() + 'static {
    move || shell::send(Input::Pane { side, input: input.clone() })
}

/// The locations menu of a pane: this host's saved folders, this
/// computer's folders, the bookmarks and the connection commands.
pub fn locations(side: Side, snapshot: &Snapshot, keys: &KeyMap, actions: &mut MenuActions) -> gio::Menu {
    let pane = snapshot.pane(side);
    let in_archive = pane.kind == LocationKind::Archive;
    let menu = gio::Menu::new();

    let places = gio::Menu::new();
    for (ix, place) in pane.places.iter().enumerate() {
        let action = actions.add(true, Some(place.current), on(side, Command::GoToPlace { index: ix as u32 }));
        places.append_item(&item(&place.name, &action, keys.menu_accel(&format!("go-to-place-{}", ix + 1))));
    }
    let toggle = actions.add(!in_archive, None, on(side, Command::TogglePlace));
    let label = if pane.place_saved { "Remove This Folder" } else { "Save This Folder" };
    places.append_item(&item(label, &toggle, keys.menu_accel("toggle-place")));
    menu.append_section((!pane.places.is_empty()).then_some("Places"), &places);

    let local = gio::Menu::new();
    for folder in &snapshot.local_folders {
        let action = actions.add(true, None, pane_input(side, PaneInput::OpenLocal { path: folder.path.clone() }));
        local.append_item(&item(&folder.label, &action, None));
    }
    menu.append_section(Some("This Computer"), &local);

    let bookmarks = gio::Menu::new();
    if snapshot.bookmarks.is_empty() {
        let action = actions.add(false, None, || {});
        bookmarks.append_item(&item("No bookmarks yet", &action, None));
    }
    for bookmark in &snapshot.bookmarks {
        let action = actions.add(
            true,
            Some(bookmark.connected),
            pane_input(side, PaneInput::OpenBookmark { id: bookmark.id.clone() }),
        );
        bookmarks.append_item(&item(&bookmark.name, &action, None));
    }
    menu.append_section(Some("Bookmarks"), &bookmarks);

    let manage = gio::Menu::new();
    let new = actions.add(true, None, on(side, Command::NewBookmark));
    manage.append_item(&item("New Bookmark…", &new, keys.menu_accel("new-bookmark")));
    let this = actions.add(!in_archive, None, on(side, Command::BookmarkConnection));
    manage.append_item(&item("New Bookmark for This Folder…", &this, keys.menu_accel("bookmark-connection")));
    let all = actions.add(true, None, on(side, Command::ManageBookmarks));
    manage.append_item(&item("Manage Bookmarks…", &all, keys.menu_accel("manage-bookmarks")));
    menu.append_section(None, &manage);

    if pane.kind != LocationKind::Local {
        let disconnect = gio::Menu::new();
        let action = actions.add(true, None, pane_input(side, PaneInput::Disconnect));
        disconnect.append_item(&item("Disconnect", &action, None));
        menu.append_section(None, &disconnect);
    }
    menu
}

/// The file list's context menu: the GPUI shell's items.
pub fn context(side: Side, pane: &PaneSnapshot, keys: &KeyMap, actions: &mut MenuActions) -> gio::Menu {
    let count = pane.selection_count;
    let one = count == 1;
    let menu = gio::Menu::new();
    let mut section = gio::Menu::new();
    let mut add = |section: &gio::Menu, label: &str, command: Command, enabled: bool| {
        let accel = keys.command_for(&command).and_then(|c| keys.menu_accel(&c.id));
        let action = actions.add(enabled, None, on(side, command));
        section.append_item(&item(label, &action, accel));
    };
    let next = |section: &mut gio::Menu| {
        menu.append_section(None, section);
        *section = gio::Menu::new();
    };

    add(&section, "Open", Command::Open, true);
    add(&section, "View", Command::ViewFile, one);
    add(&section, "Edit", Command::EditFile, one);
    add(&section, "Open Externally", Command::OpenExternally, true);
    next(&mut section);
    add(&section, "Copy to Other Pane…", Command::CopyFiles, count > 0);
    add(&section, "Move to Other Pane…", Command::MoveFiles, count > 0);
    add(&section, "Rename…", Command::RenameFile, one);
    add(&section, "Multi-Rename…", Command::MultiRename, count > 0);
    next(&mut section);
    if pane.cursor_on_archive {
        add(&section, "Open Archive", Command::OpenArchive, true);
        add(&section, "Decompress…", Command::Decompress, true);
        add(&section, "Extract to Other Pane…", Command::ExtractArchive, true);
    }
    add(&section, "Compress…", Command::Compress, count > 0);
    add(&section, "Pack into Archive in Other Pane…", Command::PackFiles, count > 0);
    next(&mut section);
    add(&section, "Copy", Command::ClipboardCopy, true);
    add(&section, "Cut", Command::ClipboardCut, true);
    add(&section, "Paste", Command::ClipboardPaste, true);
    add(&section, "Copy Path", Command::CopyPath, true);
    next(&mut section);
    add(&section, "New Folder…", Command::NewFolder, true);
    add(&section, "New File…", Command::NewFile, true);
    add(&section, "Calculate Size", Command::CalcSize, true);
    add(&section, "Properties…", Command::Properties, true);
    next(&mut section);
    if pane.host_local {
        add(&section, "Show in File Manager", Command::RevealInFinder, true);
    }
    add(&section, "Open Terminal Here", Command::OpenTerminal, true);
    next(&mut section);
    if pane.kind == LocationKind::Local {
        add(&section, "Move to Trash", Command::TrashFiles, count > 0);
    }
    add(&section, "Delete Permanently…", Command::DeleteFiles, count > 0);
    next(&mut section);
    menu
}

/// A saved folder's menu on the row of places.
pub fn place(side: Side, ix: usize, count: usize, id: &str, actions: &mut MenuActions) -> gio::Menu {
    let menu = gio::Menu::new();
    let edit = gio::Menu::new();
    let input = |input: PaneInput| pane_input(side, input);
    let rename = actions.add(true, None, input(PaneInput::RenamePlace { id: id.into() }));
    edit.append_item(&item("Rename…", &rename, None));
    let left = actions.add(ix > 0, None, input(PaneInput::MovePlace { id: id.into(), delta: -1 }));
    edit.append_item(&item("Move Left", &left, None));
    let right = actions.add(ix + 1 < count, None, input(PaneInput::MovePlace { id: id.into(), delta: 1 }));
    edit.append_item(&item("Move Right", &right, None));
    menu.append_section(None, &edit);
    let remove = gio::Menu::new();
    let action = actions.add(true, None, input(PaneInput::RemovePlace { id: id.into() }));
    remove.append_item(&item("Remove", &action, None));
    menu.append_section(None, &remove);
    menu
}

/// Shows `menu` as a popover at a point of `parent`, with its actions.
pub fn popup_at(parent: &gtk::Widget, menu: &gio::Menu, actions: MenuActions, x: f64, y: f64) {
    let popover = gtk::PopoverMenu::from_model(Some(menu));
    popover.insert_action_group(actions.prefix, Some(&actions.group));
    popover.set_parent(parent);
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    popover.connect_closed(|popover| {
        // After the item's action has run.
        let popover = popover.clone();
        glib::idle_add_local_once(move || popover.unparent());
        shell::restore_focus_soon();
    });
    popover.popup();
}
