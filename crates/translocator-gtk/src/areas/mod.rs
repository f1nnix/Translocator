//! The areas still to be built, each a STUB with the interface the rest of
//! the shell already calls, and a doc comment saying what it must become.
//!
//! - dialogs ([`rename`], [`sync`], [`properties`], [`bookmarks`]): built by
//!   `crate::dialogs` for the top entry of `Snapshot.dialogs`, through
//!   `build(&snapshot, &Sender) -> Box<dyn DialogView>`; `DialogView::update`
//!   is called with every newer snapshot of the same dialog id. A dialog
//!   never closes itself: it sends input with the `Sender`, and the shell
//!   closes it when the core drops it (`can-close` off, Escape reported
//!   through `close-attempt`).
//! - windows ([`editor`]): one per entry of `Snapshot.editors`.
//! - [`quick_look`]: the preview in place of the pane opposite the active
//!   one, while `Snapshot.quick_look` is set.
//! - [`settings`]: the preferences window, presented when
//!   `Snapshot.settings_requests` moves.
//!
//! Each Swift counterpart under `macos/Sources/Translocator/Areas/` shows
//! how its `tl_ffi` area module maps onto native widgets.

pub mod bookmarks;
pub mod editor;
pub mod properties;
pub mod quick_look;
pub mod rename;
pub mod settings;
pub mod sync;

use adw::prelude::*;
use tl_ffi::input::DialogInput;

use crate::dialogs::{DialogView, Sender};

/// A placeholder dialog: the title, what will be here, and Close (which
/// sends Cancel, so whoever waits on the dialog hears).
pub struct Placeholder(adw::AlertDialog);

impl Placeholder {
    pub fn new(title: &str, detail: &str, sender: &Sender) -> Placeholder {
        let alert = adw::AlertDialog::new(Some(title), Some(detail));
        alert.set_can_close(false);
        alert.add_response("close", "Close");
        alert.set_close_response("close");
        alert.set_default_response(Some("close"));
        let s = sender.clone();
        alert.connect_response(None, move |_, _| s.send(DialogInput::Cancel));
        let s = sender.clone();
        alert.connect_close_attempt(move |_| s.send(DialogInput::Cancel));
        Placeholder(alert)
    }
}

impl DialogView for Placeholder {
    fn dialog(&self) -> adw::Dialog {
        self.0.clone().upcast()
    }
}
