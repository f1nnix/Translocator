//! The settings and bookmarks files. One thread does every read and write,
//! in the order the core asked for them, so a save never overtakes an
//! earlier one and `Flush` only has to wait for the queue to drain.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use crux_core::Request;
use tl_app::settings::Settings;
use tl_app::{StoreOp, StoreOutput};
use tl_core::bookmark;

use crate::exec::{Activity, Inbox};

enum StoreMsg {
    Op(Request<StoreOp>),
    Flush(mpsc::Sender<()>),
}

#[derive(Clone)]
pub(crate) struct Store {
    tx: mpsc::Sender<StoreMsg>,
    activity: Activity,
}

/// What a flush waits for at most: a disk that hangs must not hang quitting.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(10);

impl Store {
    pub fn start(data_dir: PathBuf, inbox: Inbox) -> Store {
        let (tx, rx) = mpsc::channel();
        let activity = inbox.activity().clone();
        let thread_activity = activity.clone();
        let spawned = std::thread::Builder::new()
            .name("tl-store".into())
            .spawn(move || store_thread(Files::new(data_dir), rx, inbox, thread_activity));
        if let Err(err) = spawned {
            log::error!("cannot start the settings thread: {err}");
        }
        Store { tx, activity }
    }

    pub fn submit(&self, request: Request<StoreOp>) {
        self.activity.inc();
        if let Err(mpsc::SendError(StoreMsg::Op(request))) = self.tx.send(StoreMsg::Op(request)) {
            self.activity.dec();
            log::error!("the settings thread is gone; dropping {:?}", request.operation);
        }
    }

    /// Blocks until everything submitted before has been written.
    pub fn flush(&self) {
        let (tx, rx) = mpsc::channel();
        if self.tx.send(StoreMsg::Flush(tx)).is_ok() && rx.recv_timeout(FLUSH_TIMEOUT).is_err() {
            log::error!("settings were not written within {FLUSH_TIMEOUT:?}");
        }
    }
}

struct Files {
    settings: PathBuf,
    bookmarks: PathBuf,
    /// What the settings file says, so a save that changes nothing writes
    /// nothing.
    written: Option<Settings>,
}

impl Files {
    fn new(data_dir: PathBuf) -> Self {
        Files {
            settings: data_dir.join("settings.json"),
            bookmarks: data_dir.join("bookmarks.json"),
            written: None,
        }
    }

    fn handle(&mut self, op: &StoreOp) -> StoreOutput {
        match op {
            StoreOp::Load => {
                let settings = load_settings(&self.settings);
                self.written = Some(settings.clone());
                StoreOutput::Loaded {
                    settings: Box::new(settings),
                    bookmarks: bookmark::read(&self.bookmarks),
                }
            }
            StoreOp::SaveSettings(settings) => {
                if self.written.as_ref() == Some(&**settings) {
                    return StoreOutput::Saved(Ok(()));
                }
                match save_settings(&self.settings, settings) {
                    Ok(()) => {
                        self.written = Some((**settings).clone());
                        StoreOutput::Saved(Ok(()))
                    }
                    Err(err) => {
                        log::error!("cannot save {}: {err}", self.settings.display());
                        StoreOutput::Saved(Err(err.to_string()))
                    }
                }
            }
            StoreOp::SaveBookmarks(data) => match bookmark::write(&self.bookmarks, data) {
                Ok(()) => StoreOutput::Saved(Ok(())),
                Err(err) => {
                    log::error!("cannot save {}: {err}", self.bookmarks.display());
                    StoreOutput::Saved(Err(err.to_string()))
                }
            },
            // Everything before it has been written by the time it is read.
            StoreOp::Flush => StoreOutput::Saved(Ok(())),
        }
    }
}

fn store_thread(mut files: Files, rx: mpsc::Receiver<StoreMsg>, inbox: Inbox, activity: Activity) {
    while let Ok(msg) = rx.recv() {
        match msg {
            StoreMsg::Op(mut request) => {
                let output = files.handle(&request.operation);
                if !matches!(request.handle, crux_core::RequestHandle::Never) {
                    inbox.task(move |rt| {
                        rt.resolve(&mut request, output);
                    });
                }
                activity.dec();
            }
            StoreMsg::Flush(done) => {
                let _ = done.send(());
            }
        }
    }
}

/// Reads the settings, falling back to the defaults for anything the file
/// does not say. A file that cannot be parsed is copied aside rather than
/// quietly replaced, so a hand edit with one comma too few can be fixed.
pub(crate) fn load_settings(path: &Path) -> Settings {
    let mut settings = match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Settings>(&bytes) {
            Ok(settings) => settings,
            Err(err) => {
                log::error!("cannot parse {}: {err}", path.display());
                let _ = std::fs::copy(path, path.with_extension("json.broken"));
                Settings::default()
            }
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => Settings::default(),
        Err(err) => {
            log::error!("cannot read {}: {err}", path.display());
            Settings::default()
        }
    };
    settings.migrate();
    settings
}

/// Atomic write (temp file + rename), readable only by the user.
pub(crate) fn save_settings(path: &Path, settings: &Settings) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(settings).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)
}
