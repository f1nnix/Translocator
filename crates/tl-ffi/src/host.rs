//! The native shell's side of the runtime: [`Host`] is implemented in
//! Swift, and [`HostUi`] adapts it to `tl_runtime::Ui`.
//!
//! Every method is called on the runtime's thread (or a worker): the
//! implementation hops to its UI thread and returns at once.

use std::path::Path;
use std::sync::Arc;

use crate::types::ToastKind;

#[uniffi::export(with_foreign)]
pub trait Host: Send + Sync {
    /// The view changed: take a new `Core::snapshot` (coalesce: many calls
    /// may come in a burst).
    fn changed(&self);
    /// A transient notification in the main window.
    fn toast(&self, kind: ToastKind, message: String);
    fn write_clipboard(&self, text: String);
    /// Opens a local file or folder with its application.
    fn open_path(&self, path: String);
    /// Shows a local file in Finder.
    fn reveal_path(&self, path: String);
    fn open_url(&self, url: String);
    /// The core is done (it saved what it had): terminate the app.
    fn quit(&self);
}

pub(crate) struct HostUi(pub Arc<dyn Host>);

impl tl_runtime::Ui for HostUi {
    fn render(&self) {
        self.0.changed();
    }

    fn toast(&self, kind: tl_runtime::ToastKind, message: String) {
        self.0.toast(kind.into(), message);
    }

    fn write_clipboard(&self, text: String) {
        self.0.write_clipboard(text);
    }

    fn open(&self, path: &Path) {
        self.0.open_path(path.to_string_lossy().into_owned());
    }

    fn reveal(&self, path: &Path) {
        self.0.reveal_path(path.to_string_lossy().into_owned());
    }

    fn open_url(&self, url: &str) {
        self.0.open_url(url.to_string());
    }

    fn quit(&self) {
        self.0.quit();
    }
}
