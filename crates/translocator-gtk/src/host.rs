//! The core's [`Host`]: what the runtime asks of the shell, carried to the
//! GLib main loop.
//!
//! Every method is called on a Rust thread of the runtime. It puts an
//! event on an unbounded channel and returns at once; the main loop takes
//! the events off in [`spawn_receiver`]. Bursts of `changed()` collapse:
//! only the first one after a refresh is queued, and the flag is cleared
//! just before the main loop takes its snapshot, so there is one snapshot
//! per main-loop turn however many changes came in.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gtk::glib;
use tl_ffi::host::Host;
use tl_ffi::types::ToastKind;

/// What the core asks for, on the main loop.
pub enum HostEvent {
    Changed,
    Toast(ToastKind, String),
    Clipboard(String),
    Open(String),
    Reveal(String),
    OpenUrl(String),
    Quit,
}

pub struct GtkHost {
    tx: async_channel::Sender<HostEvent>,
    /// A `Changed` is queued and not yet taken.
    pending: Arc<AtomicBool>,
}

/// The host and the receiving end, which [`spawn_receiver`] drains.
pub struct Receiver {
    rx: async_channel::Receiver<HostEvent>,
    pending: Arc<AtomicBool>,
}

pub fn new() -> (Arc<GtkHost>, Receiver) {
    let (tx, rx) = async_channel::unbounded();
    let pending = Arc::new(AtomicBool::new(false));
    (
        Arc::new(GtkHost {
            tx,
            pending: pending.clone(),
        }),
        Receiver { rx, pending },
    )
}

impl GtkHost {
    fn deliver(&self, event: HostEvent) {
        // Unbounded: only fails once the main loop is gone.
        let _ = self.tx.try_send(event);
    }
}

impl Host for GtkHost {
    fn changed(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            self.deliver(HostEvent::Changed);
        }
    }

    fn toast(&self, kind: ToastKind, message: String) {
        self.deliver(HostEvent::Toast(kind, message));
    }

    fn write_clipboard(&self, text: String) {
        self.deliver(HostEvent::Clipboard(text));
    }

    fn open_path(&self, path: String) {
        self.deliver(HostEvent::Open(path));
    }

    fn reveal_path(&self, path: String) {
        self.deliver(HostEvent::Reveal(path));
    }

    fn open_url(&self, url: String) {
        self.deliver(HostEvent::OpenUrl(url));
    }

    fn quit(&self) {
        self.deliver(HostEvent::Quit);
    }
}

/// Runs `handle` on the main loop for every event, until the host is gone.
/// A `Changed` clears the pending flag first, so changes that come while
/// the snapshot is being taken queue another refresh.
pub fn spawn_receiver(receiver: Receiver, handle: impl Fn(HostEvent) + 'static) {
    glib::spawn_future_local(async move {
        while let Ok(event) = receiver.rx.recv().await {
            if matches!(event, HostEvent::Changed) {
                receiver.pending.store(false, Ordering::Release);
            }
            handle(event);
        }
    });
}
