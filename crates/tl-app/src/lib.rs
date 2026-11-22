//! Translocator's Crux core.
//!
//! Every piece of application state lives in [`Model`], and every change to
//! it happens in [`Translocator::update`], in answer to an [`Event`]: a key
//! the user pressed, a dialog they answered, a listing the shell finished.
//! The core never touches a file, a socket or a clock itself. It asks the
//! shell through [`Effect`]s, and the shell answers with more events. What
//! the UI draws is [`view::ViewModel`], built by [`Translocator::view`].
//!
//! The shell is GPUI today (`crates/translocator`), with the effects carried
//! out by `tl-runtime`; tests drive the same core with a real runtime and no
//! window at all.

pub mod bookmarks;
pub mod capabilities;
pub mod conn;
pub mod dialog;
pub mod editor;
pub mod ids;
pub mod jobs;
pub mod keys;
pub mod model;
pub mod ops;
pub mod pane;
pub mod properties;
pub mod quick_look;
pub mod rename;
pub mod settings;
pub mod sync;
pub mod view;

use crux_core::macros::effect;
use crux_core::render::{RenderOperation, render};
use crux_core::{App, Command};

pub use crate::capabilities::*;
pub use crate::ids::*;
pub use crate::model::Model;
pub use crate::pane::Side;
pub use crate::view::ViewModel;

pub use tl_model;

#[effect]
pub enum Effect {
    Render(RenderOperation),
    Fs(FsOp),
    Job(JobOp),
    Prompt(PromptOp),
    Store(StoreOp),
    Watch(WatchOp),
    Health(HealthOp),
    Platform(PlatformOp),
    Plugin(PluginOp),
    Sleep(SleepOp),
}

/// A command returned from `update`.
pub type Cmd = Command<Effect, Event>;

/// Something the user did or the shell finished.
///
/// Events stay in process (the shell is Rust and drives [`crux_core::Core`]
/// directly), so unlike the effect operations they are not serialized.
#[derive(Debug)]
pub enum Event {
    // ----- lifecycle -----
    /// The shell is up. `seed` makes the ids of new bookmarks unique.
    Start { seed: u64 },
    /// The app is about to quit: write what is pending.
    Quitting,

    // ----- input -----
    /// A command from the keyboard, a menu or a button, acting on the
    /// active pane (or on the whole window).
    Command(model::Command),
    /// Mouse and pane-local input on one side.
    Pane(Side, pane::PaneInput),
    /// A pane got the keyboard.
    Focus(Side),
    /// An answer or edit in a modal dialog.
    Dialog(DialogId, dialog::DialogInput),
    /// The command line under the panes.
    CommandLine(model::CommandLineInput),
    /// The jobs list.
    Jobs(jobs::JobsInput),
    /// The settings window, the theme toggle, the key map.
    Settings(model::SettingsEdit),
    /// The bookmark editor and manager.
    Bookmarks(bookmarks::BookmarksInput),
    /// The multi-rename dialog.
    Rename(DialogId, rename::RenameInput),
    /// The synchronize dialog.
    Sync(DialogId, sync::SyncInput),
    /// The properties dialog.
    Properties(DialogId, properties::PropertiesInput),
    /// A viewer or editor window.
    Editor(EditorId, editor::EditorInput),
    /// Files dropped from outside the app.
    DropExternal { side: Side, paths: Vec<tl_model::VPath> },

    // ----- answers from the shell -----
    Shell(Box<model::Reply>),
}

#[derive(Default)]
pub struct Translocator;

impl App for Translocator {
    type Event = Event;
    type Model = Model;
    type ViewModel = ViewModel;
    type Effect = Effect;

    fn update(&self, event: Event, model: &mut Model) -> Cmd {
        let mut out = Out::default();
        model::update(event, model, &mut out);
        out.push(quick_look::follow(model));
        out.push(render());
        Command::all(out.cmds)
    }

    fn view(&self, model: &Model) -> ViewModel {
        view::view(model)
    }
}

/// The commands an update collects; they all run concurrently.
#[derive(Default)]
pub struct Out {
    cmds: Vec<Cmd>,
}

impl Out {
    pub fn push(&mut self, cmd: Cmd) {
        self.cmds.push(cmd);
    }

    /// A toast in the main window.
    pub fn toast(&mut self, kind: ToastKind, message: impl Into<String>) {
        self.push(Command::notify_shell(PlatformOp::Toast { kind, message: message.into() }).into());
    }

    pub fn error(&mut self, message: impl Into<String>) {
        self.toast(ToastKind::Error, message);
    }

    pub fn info(&mut self, message: impl Into<String>) {
        self.toast(ToastKind::Info, message);
    }

    pub fn platform(&mut self, op: PlatformOp) {
        self.push(Command::notify_shell(op).into());
    }

    /// Asks a file system something; the answer comes back as `reply(output)`.
    pub fn fs(&mut self, op: FsOp, reply: impl FnOnce(FsOutput) -> model::Reply + Send + 'static) {
        self.push(Command::request_from_shell(op).then_send(move |out| Event::Shell(Box::new(reply(out)))));
    }

    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }
}

#[cfg(test)]
mod tests;
