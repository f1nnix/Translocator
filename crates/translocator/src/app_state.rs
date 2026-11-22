//! The shell's one piece of process-wide state: the running core, and the
//! view model it last rendered.
//!
//! The core runs on the runtime's thread. When it has something new to
//! show, the runtime calls [`Ui::render`] there; that, like every other
//! request from the runtime (a toast, the clipboard, opening a file), is
//! sent over a channel to a task on GPUI's main thread, which drains the
//! channel and replaces [`Shell::view`] once per batch. Views observe the
//! [`Shell`] global and redraw only when their part of the view changed.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use tl_app::model::Command;
use tl_app::{Event, ToastKind, ViewModel};
use tl_runtime::{Config, Runtime, Ui};

/// The running core and what it last showed.
pub struct Shell {
    pub runtime: Runtime,
    /// Replaced (never edited) on every render, through `update_global`, so
    /// `observe_global::<Shell>` observers hear of it.
    pub view: Rc<ViewModel>,
    pub main_window: Option<AnyWindowHandle>,
    /// The settings window, while one is open: a second would edit the same
    /// settings from two places.
    pub settings_window: Option<AnyWindowHandle>,
    /// The one-shot requests in the view (counters the core bumps) that have
    /// been carried out. Cells, because writing a field through
    /// `global_mut` would wake every observer again.
    pub seen: Seen,
    /// A way into the queue of runtime requests, kept so the tests can tell
    /// when everything the runtime sent has been carried out.
    #[cfg(test)]
    ui_queue: smol::channel::Sender<UiMsg>,
}

impl Global for Shell {}

#[derive(Default)]
pub struct Seen {
    pub focus_request: Cell<u64>,
    /// By side: left, right.
    pub locations_request: [Cell<u64>; 2],
    pub settings_requests: Cell<u64>,
    pub cmdline_clears: Cell<u64>,
    pub console_version: Cell<u64>,
}

/// The view model the core rendered last.
pub fn view(cx: &App) -> Rc<ViewModel> {
    cx.global::<Shell>().view.clone()
}

/// Sends an event to the core.
pub fn dispatch(event: Event, cx: &App) {
    cx.global::<Shell>().runtime.dispatch(event);
}

/// A keyboard, menu or button command, acting on the active pane.
pub fn command(cmd: Command, cx: &App) {
    dispatch(Event::Command(cmd), cx);
}

/// What the runtime asks of the UI, carried to the main thread.
enum UiMsg {
    Render,
    Toast(ToastKind, String),
    Clipboard(String),
    Open(PathBuf),
    Reveal(PathBuf),
    OpenUrl(String),
    Quit,
}

/// The runtime's handle on the UI. Every method is called on the runtime's
/// thread and only queues a message.
struct ChannelUi {
    tx: smol::channel::Sender<UiMsg>,
}

impl ChannelUi {
    fn send(&self, msg: UiMsg) {
        // Fails only once the app is gone, when nothing is left to show.
        let _ = self.tx.try_send(msg);
    }
}

impl Ui for ChannelUi {
    fn render(&self) {
        self.send(UiMsg::Render);
    }

    fn toast(&self, kind: ToastKind, message: String) {
        self.send(UiMsg::Toast(kind, message));
    }

    fn write_clipboard(&self, text: String) {
        self.send(UiMsg::Clipboard(text));
    }

    fn open(&self, path: &Path) {
        self.send(UiMsg::Open(path.to_path_buf()));
    }

    fn reveal(&self, path: &Path) {
        self.send(UiMsg::Reveal(path.to_path_buf()));
    }

    fn open_url(&self, url: &str) {
        self.send(UiMsg::OpenUrl(url.to_string()));
    }

    fn quit(&self) {
        self.send(UiMsg::Quit);
    }
}

pub fn init(cx: &mut App) {
    start(Config::standard(), cx);
}

/// For the UI tests: a real runtime whose files all live under `dir` — the
/// settings (written there first), bookmarks, cache and sockets — so the
/// user's own files are never read or written. No askpass server.
#[cfg(test)]
pub fn init_for_tests(cx: &mut App, dir: &Path, settings: &tl_app::settings::Settings) -> Runtime {
    let state = dir.join("state");
    std::fs::create_dir_all(&state).expect("the test's state folder");
    let json = serde_json::to_vec_pretty(settings).expect("settings serialize");
    std::fs::write(state.join("settings.json"), json).expect("the test's settings file");
    let config = Config {
        data_dir: state,
        cache_dir: dir.join("cache"),
        runtime_dir: Some(dir.join("run")),
        askpass_exe: None,
        plugins: None,
    };
    start(config, cx);
    cx.global::<Shell>().runtime.clone()
}

/// For the UI tests: nothing the runtime sent is still waiting for the main
/// thread. The task below drains the queue and carries a batch out within
/// one poll, so an empty queue means it has all been done.
#[cfg(test)]
pub fn ui_idle(cx: &App) -> bool {
    cx.global::<Shell>().ui_queue.is_empty()
}

fn start(config: Config, cx: &mut App) {
    let (tx, rx) = smol::channel::unbounded::<UiMsg>();
    #[cfg(test)]
    let ui_queue = tx.clone();
    let runtime = Runtime::start(config, Arc::new(ChannelUi { tx }));
    let view = Rc::new(runtime.view());
    cx.set_global(Shell {
        runtime,
        view,
        main_window: None,
        settings_window: None,
        seen: Seen::default(),
        #[cfg(test)]
        ui_queue,
    });

    cx.spawn(async move |cx| {
        while let Ok(first) = rx.recv().await {
            // Everything queued so far goes in one batch, and however many
            // renders it holds, the view is fetched once.
            let mut batch = vec![first];
            while let Ok(msg) = rx.try_recv() {
                batch.push(msg);
            }
            cx.update(|cx| {
                let mut render = false;
                let mut quit = false;
                for msg in batch {
                    match msg {
                        UiMsg::Render => render = true,
                        UiMsg::Quit => quit = true,
                        other => perform(other, cx),
                    }
                }
                if render {
                    refresh(cx);
                }
                if quit {
                    cx.quit();
                }
            });
        }
    })
    .detach();

    // Tells the core, lets the settings and bookmarks reach the disk and
    // closes the connections before the process goes.
    cx.on_app_quit(|cx| {
        cx.global::<Shell>().runtime.shutdown();
        async {}
    })
    .detach();
}

fn perform(msg: UiMsg, cx: &mut App) {
    match msg {
        UiMsg::Toast(kind, message) => toast(kind, message, cx),
        UiMsg::Clipboard(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
        UiMsg::Open(path) => cx.open_with_system(&path),
        UiMsg::Reveal(path) => cx.reveal_path(&path),
        UiMsg::OpenUrl(url) => cx.open_url(&url),
        UiMsg::Render | UiMsg::Quit => {}
    }
}

/// A notification in the main window.
fn toast(kind: ToastKind, message: String, cx: &mut App) {
    let Some(window) = cx.global::<Shell>().main_window else { return };
    let note = match kind {
        ToastKind::Info => Notification::info(message),
        ToastKind::Success => Notification::success(message),
        ToastKind::Error => Notification::error(message),
    };
    let _ = window.update(cx, |_, window, cx| window.push_notification(note, cx));
}

/// Takes in the core's latest view and carries out what is app-wide in it:
/// the theme and key map when the settings changed, the settings window,
/// the editor windows. The main window's parts observe the global.
fn refresh(cx: &mut App) {
    let new = Rc::new(cx.global::<Shell>().runtime.view());
    let old = cx.global::<Shell>().view.clone();
    cx.update_global::<Shell, _>(|shell, _| shell.view = new.clone());

    if !Arc::ptr_eq(&old.settings, &new.settings) {
        let (was, now) = (&old.settings, &new.settings);
        if was.appearance != now.appearance {
            match cx.global::<Shell>().main_window {
                Some(window) => {
                    let _ = window.update(cx, |_, window, cx| crate::themes::apply(Some(window), cx));
                }
                None => crate::themes::apply(None, cx),
            }
        } else if was.general.font_size != now.general.font_size {
            crate::themes::tweaks(cx);
        }
        if was.keys != now.keys {
            crate::keymap::apply(cx);
        }
    }

    let seen = &cx.global::<Shell>().seen;
    if seen.settings_requests.get() != new.settings_requests {
        seen.settings_requests.set(new.settings_requests);
        crate::settings_window::open(cx);
    }

    crate::editor::sync(&new, cx);
}
