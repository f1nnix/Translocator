//! The application, its one window and the snapshot loop.
//!
//! [`Shell`] owns the running core and the latest snapshot. Inputs go to
//! the core with [`send`] (never blocking); when the host hears
//! `changed()`, [`Shell::refresh`] takes a new snapshot on the main loop
//! (at most one per turn, see [`crate::host`]) and hands each part of the
//! window the old and new values, so only what changed is redrawn. The
//! one-shot requests a snapshot carries (where the keyboard goes, the
//! locations menu, the preferences) are counters: the shell acts when one
//! moves.
//!
//! There is one window, so the shell lives in a thread-local and widget
//! callbacks reach it with [`with`].

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use tl_ffi::Core;
use tl_ffi::input::{Command, Input};
use tl_ffi::keys::{KeyContext, KeyTarget};
use tl_ffi::snapshot::{FocusTarget, Snapshot};
use tl_ffi::types::{Side, ToastKind};

use crate::actions::{self, Actions};
use crate::areas::editor::Editors;
use crate::areas::quick_look::QuickLookView;
use crate::bottom::Bottom;
use crate::dialogs::Dialogs;
use crate::file_list::fill_rows;
use crate::header::Header;
use crate::host::{self, HostEvent};
use crate::keys::{self, KeyMap};
use crate::menus::{self, MenuActions};
use crate::pane::PaneView;
use crate::theme::Theme;

/// The application id: reverse DNS for the desktop file, D-Bus and the
/// icon (`packaging/dev.translocator.Translocator.desktop` once the GTK
/// app is what the packages ship).
pub const APP_ID: &str = "dev.translocator.Translocator";

const SIDES: [Side; 2] = [Side::Left, Side::Right];

fn ix(side: Side) -> usize {
    match side {
        Side::Left => 0,
        Side::Right => 1,
    }
}

thread_local! {
    static SHELL: OnceCell<Rc<Shell>> = const { OnceCell::new() };
}

/// Runs `f` with the shell. Panics before the window is up, which no
/// callback can be.
pub fn with<R>(f: impl FnOnce(&Shell) -> R) -> R {
    let shell = SHELL.with(|s| s.get().cloned()).expect("the shell is running");
    f(&shell)
}

fn try_shell() -> Option<Rc<Shell>> {
    SHELL.with(|s| s.get().cloned())
}

/// Queues an input for the core.
pub fn send(input: Input) {
    with(|s| s.core.send(input));
}

pub fn core() -> Arc<Core> {
    with(|s| s.core.clone())
}

/// A command for the active pane (or the window). Page Up / Down move by
/// what the active list shows.
pub fn command(command: Command) {
    with(|s| {
        let active = s.snapshot.borrow().active;
        let rows = s.panes[ix(active)].list.page_rows();
        s.core.send(Input::Command {
            command: fill_rows(command, rows),
        });
    });
}

/// A command for `side`: the core acts on the active pane, so the side is
/// made active first. A pane's menu can be used while the keyboard is in
/// the other one.
pub fn command_on(side: Side, cmd: Command) {
    focus_pane(side);
    command(cmd);
}

/// Makes `side` the active pane, if it is not.
pub fn focus_pane(side: Side) {
    with(|s| {
        if s.snapshot.borrow().active != side {
            s.core.send(Input::Focus { side });
        }
    });
}

/// A key in a file list: the key table for the pane, the window and the
/// app, then the type-ahead filter. Escape clears the filter.
pub fn pane_key(
    side: Side,
    display: &gdk::Display,
    keyval: gdk::Key,
    keycode: u32,
    state: gdk::ModifierType,
    group: u32,
) -> glib::Propagation {
    use gdk::Key as K;
    if matches!(
        keyval,
        K::Shift_L
            | K::Shift_R
            | K::Control_L
            | K::Control_R
            | K::Alt_L
            | K::Alt_R
            | K::Super_L
            | K::Super_R
            | K::Meta_L
            | K::Meta_R
            | K::ISO_Level3_Shift
            | K::Caps_Lock
    ) {
        return glib::Propagation::Proceed;
    }
    if keyval == K::Escape {
        let filtering = with(|s| !s.snapshot.borrow().pane(side).filter.is_empty());
        if filtering {
            command_on(side, Command::ClearFilter);
        }
        return glib::Propagation::Stop;
    }
    let spellings = keys::spellings(display, keyval, keycode, state, group);
    let target = with(|s| {
        s.keys
            .borrow()
            .target(&spellings, &[KeyContext::Pane, KeyContext::Workspace, KeyContext::Global])
            .cloned()
    });
    if let Some(KeyTarget::Command { command }) = target {
        command_on(side, command);
        return glib::Propagation::Stop;
    }
    if let Some(text) = keys::typed_text(keyval, state) {
        // The type-ahead filter; with no filter yet, + and - ask for a mask.
        command_on(side, Command::Type { text });
        return glib::Propagation::Stop;
    }
    glib::Propagation::Proceed
}

/// A menu or popover closed: the keyboard goes back to the active pane's
/// list, unless something that takes text has it. (GTK gives it to the
/// button that opened the menu, where no file command would reach.)
pub fn restore_focus_soon() {
    glib::idle_add_local_once(|| {
        let Some(shell) = try_shell() else { return };
        let focus = gtk::prelude::GtkWindowExt::focus(&shell.window);
        let lost = match &focus {
            None => true,
            Some(f) => {
                f.is::<gtk::Button>()
                    || f.is::<gtk::MenuButton>()
                    || f.ancestor(gtk::MenuButton::static_type()).is_some()
                    || f.ancestor(adw::HeaderBar::static_type()).is_some()
            }
        };
        if lost && shell.snapshot.borrow().dialogs.is_empty() {
            let active = shell.snapshot.borrow().active;
            shell.panes[ix(active)].list.grab_focus();
        }
    });
}

/// The file list's context menu, at a point of `parent`.
pub fn show_context_menu(side: Side, parent: &gtk::Widget, x: f64, y: f64) {
    let mut actions = MenuActions::new("ctx");
    let menu = with(|s| menus::context(side, s.snapshot.borrow().pane(side), &s.keys.borrow(), &mut actions));
    menus::popup_at(parent, &menu, actions, x, y);
}

/// The side a drag of rows started on, while it lasts.
pub fn dragging_from() -> Option<Side> {
    with(|s| s.dragging.borrow().0)
}

pub fn dragging_label() -> String {
    with(|s| s.dragging.borrow().1.clone())
}

pub fn set_dragging_from(side: Option<Side>, label: String) {
    with(|s| s.dragging.replace((side, label)));
}

/// What the one-shot requests were when the shell last acted on them.
#[derive(Default)]
struct Seen {
    focus: u64,
    locations: [u64; 2],
    settings: u64,
    console: u64,
    dark: Option<bool>,
}

pub struct Shell {
    pub core: Arc<Core>,
    pub app: adw::Application,
    pub window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    /// The latest snapshot.
    pub snapshot: RefCell<Snapshot>,
    /// The key table with the user's keys.
    pub keys: RefCell<KeyMap>,
    panes: [Rc<PaneView>; 2],
    /// Each side shows its pane, or the preview.
    slots: [gtk::Stack; 2],
    quick_looks: [QuickLookView; 2],
    bottom: Bottom,
    header: Header,
    actions: Actions,
    dialogs: Dialogs,
    editors: Editors,
    theme: Theme,
    /// The pane whose list has the keyboard, if any.
    focused_pane: Cell<Option<Side>>,
    seen: RefCell<Seen>,
    dragging: RefCell<(Option<Side>, String)>,
}

pub fn run() -> i32 {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| gtk::Window::set_default_icon_name(APP_ID));
    app.connect_activate(|app| match try_shell() {
        Some(shell) => shell.window.present(),
        None => Shell::start(app),
    });
    app.connect_shutdown(|_| {
        if let Some(shell) = try_shell() {
            // Writes what is pending, stops jobs, closes connections.
            shell.core.shutdown();
        }
    });
    // GTK's own options are not this app's: pass no arguments.
    i32::from(app.run_with_args::<&str>(&[]))
}

impl Shell {
    fn start(app: &adw::Application) {
        let (host, receiver) = host::new();
        let core = Core::start(host);
        let first = core.snapshot();
        let keys = KeyMap::new(core.key_table());

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(1280)
            .default_height(820)
            .title(&first.title)
            .build();
        window.set_size_request(760, 480);
        let theme = Theme::install(&WidgetExt::display(&window));

        let header = Header::new();
        let panes = [PaneView::new(core.clone(), Side::Left), PaneView::new(core.clone(), Side::Right)];
        let quick_looks = [QuickLookView::new(), QuickLookView::new()];
        let slots = [gtk::Stack::new(), gtk::Stack::new()];
        for side in SIDES {
            let slot = &slots[ix(side)];
            slot.add_named(&panes[ix(side)].root, Some("pane"));
            slot.add_named(&quick_looks[ix(side)].root, Some("quick-look"));
            slot.set_visible_child_name("pane");
            slot.set_hexpand(true);
        }
        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_start_child(Some(&slots[0]));
        paned.set_end_child(Some(&slots[1]));
        paned.set_shrink_start_child(false);
        paned.set_shrink_end_child(false);
        paned.set_vexpand(true);
        paned.connect_map(|paned| {
            let paned = paned.clone();
            glib::idle_add_local_once(move || paned.set_position(paned.width() / 2));
        });

        let bottom = Bottom::new();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&paned);
        content.append(&bottom.root);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&content));
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header.bar);
        view.set_content(Some(&toasts));
        window.set_content(Some(&view));

        let actions = Actions::install(&window, app, &keys, &first);
        app.set_accels_for_action("win.show-help-overlay", &["<Control>question"]);

        let shell = Rc::new(Shell {
            core,
            app: app.clone(),
            window: window.clone(),
            toasts,
            snapshot: RefCell::new(first.clone()),
            keys: RefCell::new(keys),
            panes,
            slots,
            quick_looks,
            bottom,
            header,
            actions,
            dialogs: Dialogs::default(),
            editors: Editors::default(),
            theme,
            focused_pane: Cell::new(None),
            seen: RefCell::new(Seen {
                settings: first.settings_requests,
                ..Seen::default()
            }),
            dragging: RefCell::new((None, String::new())),
        });
        SHELL.with(|s| {
            let _ = s.set(shell.clone());
        });

        window.connect_focus_widget_notify(|_| with(|s| s.focus_changed()));
        window.connect_close_request(|_| {
            // The core writes what is pending, then asks the host to quit.
            send(Input::Command { command: Command::Quit });
            glib::Propagation::Stop
        });
        adw::StyleManager::default().connect_dark_notify(|_| with(|s| s.dark_changed()));
        host::spawn_receiver(receiver, |event| {
            if let Some(shell) = try_shell() {
                shell.perform(event);
            }
        });

        shell.apply(&first, &first, true);
        window.present();
        glib::idle_add_local_once(|| {
            with(|s| {
                let active = s.snapshot.borrow().active;
                s.panes[ix(active)].list.grab_focus();
            })
        });
    }

    fn perform(&self, event: HostEvent) {
        match event {
            HostEvent::Changed => self.refresh(),
            HostEvent::Toast(kind, message) => {
                let toast = adw::Toast::builder()
                    .title(glib::markup_escape_text(&message))
                    .timeout(if kind == ToastKind::Error { 6 } else { 3 })
                    .priority(if kind == ToastKind::Error {
                        adw::ToastPriority::High
                    } else {
                        adw::ToastPriority::Normal
                    })
                    .build();
                self.toasts.add_toast(toast);
            }
            HostEvent::Clipboard(text) => WidgetExt::display(&self.window).clipboard().set_text(&text),
            HostEvent::Open(path) => {
                gtk::FileLauncher::new(Some(&gio::File::for_path(&path))).launch(
                    Some(&self.window),
                    gio::Cancellable::NONE,
                    move |result| {
                        if let Err(err) = result {
                            log::warn!("cannot open {path}: {err}");
                        }
                    },
                );
            }
            HostEvent::Reveal(path) => {
                gtk::FileLauncher::new(Some(&gio::File::for_path(&path))).open_containing_folder(
                    Some(&self.window),
                    gio::Cancellable::NONE,
                    move |result| {
                        if let Err(err) = result {
                            log::warn!("cannot show {path}: {err}");
                        }
                    },
                );
            }
            HostEvent::OpenUrl(url) => {
                gtk::UriLauncher::new(&url).launch(Some(&self.window), gio::Cancellable::NONE, move |result| {
                    if let Err(err) = result {
                        log::warn!("cannot open {url}: {err}");
                    }
                });
            }
            HostEvent::Quit => self.app.quit(),
        }
    }

    /// Takes a new snapshot and redraws what changed.
    fn refresh(&self) {
        let next = self.core.snapshot();
        if *self.snapshot.borrow() == next {
            return;
        }
        let old = self.snapshot.replace(next.clone());
        self.apply(&old, &next, false);
    }

    fn apply(&self, old: &Snapshot, next: &Snapshot, initial: bool) {
        let keys_changed = initial || old.settings.keys_version != next.settings.keys_version;
        if keys_changed {
            if !initial {
                self.keys.replace(KeyMap::new(self.core.key_table()));
            }
            let keys = self.keys.borrow();
            self.actions.set_accels(&self.app, &keys);
            self.header.set_keys(&keys);
            self.bottom.set_keys(&keys);
            self.window.set_help_overlay(Some(&actions::shortcuts_window(&keys)));
        }
        {
            let mut seen = self.seen.borrow_mut();
            if initial || next.console_version != seen.console {
                seen.console = next.console_version;
                self.bottom.set_console_text(&self.core.console_text());
            }
        }
        if initial || old.settings != next.settings {
            self.theme.apply(&next.settings);
            self.seen.borrow_mut().dark = Some(adw::StyleManager::default().is_dark());
            self.header.set_dark(adw::StyleManager::default().is_dark());
            let general = &next.settings.general;
            for pane in &self.panes {
                pane.list.set_general(general.row_height, general.show_icons);
            }
            crate::areas::settings::update(&next.settings, &self.keys.borrow());
        }
        if initial || old.title != next.title {
            self.window.set_title(Some(&next.title));
        }

        {
            let keys = self.keys.borrow();
            for side in SIDES {
                let pane = next.pane(side);
                if initial || keys_changed || pane != old.pane(side) {
                    self.panes[ix(side)].apply(pane, &keys, next.settings.keys_version);
                }
            }
        }
        for side in SIDES {
            let slot = &self.slots[ix(side)];
            match &next.quick_look {
                Some(preview) if preview.side == side => {
                    self.quick_looks[ix(side)].update(preview);
                    if slot.visible_child_name().as_deref() != Some("quick-look") {
                        slot.set_visible_child_name("quick-look");
                    }
                }
                _ => {
                    if slot.visible_child_name().as_deref() != Some("pane") {
                        slot.set_visible_child_name("pane");
                    }
                }
            }
        }

        self.header.apply(old, next, initial);
        self.bottom.apply(old, next, initial);
        self.actions.set_states(next);
        self.actions
            .set_enabled(self.focused_pane.get().is_some(), !next.dialogs.is_empty());
        if initial || old.dialogs != next.dialogs {
            self.dialogs.apply(&next.dialogs, &self.window);
        }
        if initial || old.editors != next.editors {
            self.editors.sync(&next.editors, &self.window);
        }

        // One-shot requests.
        if let Some(request) = &next.focus_request {
            let act = self.seen.borrow().focus != request.serial;
            if act {
                self.seen.borrow_mut().focus = request.serial;
                match request.target {
                    FocusTarget::Pane { side } => self.panes[ix(side)].list.grab_focus(),
                    FocusTarget::CommandLine => self.bottom.focus_command_line(),
                }
            }
        }
        if let Some(request) = &next.locations_request {
            let side = request.side;
            let act = self.seen.borrow().locations[ix(side)] != request.serial;
            if act {
                self.seen.borrow_mut().locations[ix(side)] = request.serial;
                // After this snapshot is in place, so the menu is built from it.
                let pane = self.panes[ix(side)].clone();
                glib::idle_add_local_once(move || pane.popup_locations());
            }
        }
        if next.settings_requests != self.seen.borrow().settings {
            self.seen.borrow_mut().settings = next.settings_requests;
            crate::areas::settings::present(next, &self.window);
        }
    }

    /// The window's keyboard focus moved: which list has it, if any. A
    /// menu or popover taking the focus leaves it as it was, so a pane's
    /// commands stay enabled in the menu that runs them.
    fn focus_changed(&self) {
        let focus = gtk::prelude::GtkWindowExt::focus(&self.window);
        if focus
            .as_ref()
            .is_some_and(|f| f.ancestor(gtk::Popover::static_type()).is_some())
        {
            return;
        }
        let side = focus.as_ref().and_then(|focus| {
            SIDES.into_iter().find(|side| {
                let view = &self.panes[ix(*side)].list.view;
                focus == view.upcast_ref::<gtk::Widget>() || focus.is_ancestor(view)
            })
        });
        if self.focused_pane.replace(side) != side {
            for s in SIDES {
                self.panes[ix(s)].set_focused(side == Some(s));
            }
            let dialogs = !self.snapshot.borrow().dialogs.is_empty();
            self.actions.set_enabled(side.is_some(), dialogs);
        }
        if let Some(side) = side
            && self.snapshot.borrow().active != side
        {
            self.core.send(Input::Focus { side });
        }
    }

    /// The system (or the setting) switched between light and dark: the
    /// content theme follows.
    fn dark_changed(&self) {
        let dark = adw::StyleManager::default().is_dark();
        if self.seen.borrow().dark == Some(dark) {
            return;
        }
        self.seen.borrow_mut().dark = Some(dark);
        self.theme.apply(&self.snapshot.borrow().settings);
        self.header.set_dark(dark);
    }
}
