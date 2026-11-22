//! Translocator's foreign-function layer, for native shells (the macOS
//! app in `macos/`).
//!
//! A presentation layer, not a second core: it mirrors the core's inputs
//! ([`input::Input`] → `tl_app::Event`) and turns its view model into
//! small, display-ready records ([`snapshot::Snapshot`], [`rows::Row`]).
//! Every decision stays in `tl-app`; what is here is conversion and
//! formatting that needs the platform (local dates, sizes, icons).
//!
//! The shell starts a [`Core`] with its [`host::Host`], sends inputs with
//! [`Core::send`] (never blocks), and when the host hears `changed()` takes
//! a [`Core::snapshot`]. The snapshot is the same size whatever the
//! folders hold; the rows of a list come from [`Core::rows`] by range, and
//! are fetched again only when the pane's `listing_version` moves.
//!
//! Areas filled in separately (each module says what it still needs):
//! [`rename`], [`sync`], [`properties`], [`bookmarks`], [`editor`],
//! [`quick_look`], [`settings`].

uniffi::setup_scaffolding!();

pub mod bookmarks;
pub mod editor;
pub mod format;
pub mod host;
pub mod input;
pub mod keys;
pub mod properties;
pub mod quick_look;
pub mod rename;
pub mod rows;
pub mod settings;
pub mod snapshot;
pub mod sync;
pub mod types;
mod versions;

use std::sync::Arc;

use parking_lot::Mutex;
use tl_app::ViewModel;
use tl_runtime::{Config, Runtime};

use crate::host::{Host, HostUi};
use crate::input::Input;
use crate::keys::{KeyCommand, KeyConflict};
use crate::rows::{DragItems, Row};
use crate::snapshot::{FoundRow, Snapshot, Trackers};
use crate::types::Side;

/// The running core and its runtime.
#[derive(uniffi::Object)]
pub struct Core {
    runtime: Runtime,
    state: Mutex<State>,
}

struct State {
    /// The view the last snapshot was built from: what rows and other
    /// fetches read, so they agree with the snapshot the shell holds.
    view: Arc<ViewModel>,
    trackers: Trackers,
}

impl Core {
    /// For the tests: a core on a runtime made elsewhere.
    pub fn with_runtime(runtime: Runtime) -> Arc<Core> {
        let view = Arc::new(runtime.view());
        Arc::new(Core {
            runtime,
            state: Mutex::new(State {
                view,
                trackers: Trackers::default(),
            }),
        })
    }

    fn view(&self) -> Arc<ViewModel> {
        self.state.lock().view.clone()
    }
}

#[uniffi::export]
impl Core {
    /// Starts the core with the standard places (this user's settings and
    /// bookmarks, askpass through this executable) and sends `Start`.
    #[uniffi::constructor]
    pub fn start(host: Arc<dyn Host>) -> Arc<Core> {
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).try_init();
        Core::with_runtime(Runtime::start(Config::standard(), Arc::new(HostUi(host))))
    }

    /// Queues an input for the core; returns at once.
    pub fn send(&self, input: Input) {
        self.runtime.dispatch(input.into());
    }

    /// What the window shows now. Also what later `rows`, `drag`,
    /// `find_results`, `console_text` and `key_table` calls read.
    pub fn snapshot(&self) -> Snapshot {
        let view = self.runtime.view();
        let mut state = self.state.lock();
        // The previous view stays cached until the new snapshot is built:
        // the version keys compare against its live `Arc`s.
        let snapshot = snapshot::build(&view, &mut state.trackers);
        state.view = Arc::new(view);
        snapshot
    }

    /// Rows `start..start + count` of a pane's list (fewer at the end).
    pub fn rows(&self, side: Side, start: u32, count: u32) -> Vec<Row> {
        let view = self.view();
        let pane = &view.panes[tl_app::Side::from(side).index()];
        rows::rows(pane, view.settings.general.date_format, start, count)
    }

    /// What dragging `row` carries: the marked rows if it is marked, else
    /// that row. `None` for "..".
    pub fn drag(&self, side: Side, row: u32) -> Option<DragItems> {
        let view = self.view();
        rows::drag(&view.panes[tl_app::Side::from(side).index()], side, row)
    }

    /// Hits `start..start + count` of a find-results dialog.
    pub fn find_results(&self, dialog: u64, start: u32, count: u32) -> Vec<FoundRow> {
        snapshot::find_rows(&self.view(), dialog, start, count)
    }

    /// The console's text (fetch when `Snapshot.console_version` moves).
    pub fn console_text(&self) -> String {
        self.view().console.text.as_str().to_string()
    }

    /// The key table with the user's keys (refetch when
    /// `Snapshot.settings.keys_version` moves).
    pub fn key_table(&self) -> Vec<KeyCommand> {
        keys::table(&self.view().settings.keys)
    }

    /// Other commands of the same context that answer to `keys`.
    pub fn key_conflicts(&self, keys: String, command_id: String) -> Vec<KeyConflict> {
        keys::conflicts(&keys, &command_id, &self.view().settings.keys)
    }

    /// Writes what is pending, stops jobs and closes connections. Blocks
    /// (up to some seconds); later calls do nothing.
    pub fn shutdown(&self) {
        self.runtime.shutdown();
    }
}

/// `ssh` runs the app's executable as its askpass helper with this
/// variable set: the executable must then call [`askpass_client`] and exit
/// with its result instead of starting the UI.
#[uniffi::export]
pub fn askpass_socket_env() -> String {
    tl_core::askpass::ENV_SOCKET.to_string()
}

/// Forwards an askpass prompt to the running app; the exit code.
#[uniffi::export]
pub fn askpass_client(prompt: String) -> i32 {
    tl_core::askpass::run_client(&prompt)
}

/// The workspace version, `0.2.3`.
#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg(test)]
mod tests;
