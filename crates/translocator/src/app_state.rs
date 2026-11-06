//! Process-wide state: plugins, bookmarks, live connections, settings.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::*;
use serde::{Deserialize, Serialize};
use tl_core::askpass::AskpassServer;
use tl_core::bookmark::app_data_dir;
use tl_core::{BookmarkStore, PluginEnv, PluginRegistry, VPath, Vfs};

use crate::bridge::{self, UiBridge};

pub struct AppState {
    pub registry: PluginRegistry,
    pub bookmarks: BookmarkStore,
    /// Open connections, by bookmark id.
    pub connections: Vec<Connection>,
    pub env: PluginEnv,
    pub bridge: UiBridge,
    pub settings: Settings,
    pub clipboard: Option<FileClip>,
    pub main_window: Option<AnyWindowHandle>,
    _askpass: Option<AskpassServer>,
}

impl Global for AppState {}

pub struct Connection {
    pub bookmark_id: String,
    pub vfs: Arc<dyn Vfs>,
}

/// Files put on the internal clipboard with ⌘C / ⌘X.
#[derive(Clone)]
pub struct FileClip {
    pub vfs: Arc<dyn Vfs>,
    pub dir: VPath,
    pub names: Vec<Vec<u8>>,
    pub elevated: bool,
    pub cut: bool,
}

/// Persisted between runs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default)]
    pub left_path: Option<String>,
    #[serde(default)]
    pub right_path: Option<String>,
}

impl Settings {
    fn path() -> PathBuf {
        app_data_dir().join("settings.json")
    }

    fn load() -> Self {
        std::fs::read(Self::path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

/// Removes runtime directories of instances that are no longer running.
fn remove_stale_runtime_dirs() {
    let Ok(read) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for item in read.flatten() {
        let name = item.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.strip_prefix("tl-")).and_then(|p| p.parse::<i32>().ok()) else {
            continue;
        };
        let alive = unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !alive {
            let _ = std::fs::remove_dir_all(item.path());
        }
    }
}

/// Private per-process directory for control sockets and temp files.
fn runtime_dir() -> PathBuf {
    remove_stale_runtime_dirs();
    let dir = std::env::temp_dir().join(format!("tl-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir
}

pub fn init(cx: &mut App) {
    let mut registry = PluginRegistry::new();
    registry.register(Arc::new(tl_ssh::SshPlugin));
    registry.register(Arc::new(tl_s3::S3Plugin));
    registry.register(Arc::new(tl_webdav::WebDavPlugin));
    registry.register(Arc::new(tl_local::LocalPlugin));

    let bridge = bridge::start(cx);
    let runtime = runtime_dir();
    let askpass = std::env::current_exe().ok().and_then(|exe| {
        match AskpassServer::start(runtime.join("askpass.sock"), exe, Arc::new(bridge.clone())) {
            Ok(server) => Some(server),
            Err(err) => {
                log::error!("askpass server unavailable: {err}");
                None
            }
        }
    });
    let env = PluginEnv {
        prompter: Arc::new(bridge.clone()),
        askpass: askpass.as_ref().map(AskpassServer::handle),
        runtime_dir: runtime,
    };
    cx.set_global(AppState {
        registry,
        bookmarks: BookmarkStore::load(BookmarkStore::default_path()),
        connections: Vec::new(),
        env,
        bridge,
        settings: Settings::load(),
        clipboard: None,
        main_window: None,
        _askpass: askpass,
    });

    cx.on_app_quit(|cx| {
        let state = cx.global::<AppState>();
        state.settings.save();
        for conn in &state.connections {
            conn.vfs.disconnect();
        }
        let runtime = state.env.runtime_dir.clone();
        async move {
            let _ = std::fs::remove_dir_all(runtime);
        }
    })
    .detach();
}

/// State for UI tests: no askpass server, bookmarks and settings kept in
/// `dir` (the user's own files are never read or written).
#[cfg(test)]
pub fn init_for_tests(cx: &mut App, dir: &std::path::Path, settings: Settings) {
    let mut registry = PluginRegistry::new();
    registry.register(Arc::new(tl_ssh::SshPlugin));
    registry.register(Arc::new(tl_s3::S3Plugin));
    registry.register(Arc::new(tl_webdav::WebDavPlugin));
    registry.register(Arc::new(tl_local::LocalPlugin));
    let bridge = bridge::start(cx);
    let env = PluginEnv {
        prompter: Arc::new(bridge.clone()),
        askpass: None,
        runtime_dir: dir.to_path_buf(),
    };
    cx.set_global(AppState {
        registry,
        bookmarks: BookmarkStore::load(dir.join("bookmarks.json")),
        connections: Vec::new(),
        env,
        bridge,
        settings,
        clipboard: None,
        main_window: None,
        _askpass: None,
    });
}

impl AppState {
    pub fn connection(&self, bookmark_id: &str) -> Option<Arc<dyn Vfs>> {
        self.connections
            .iter()
            .find(|c| c.bookmark_id == bookmark_id && c.vfs.is_connected())
            .map(|c| c.vfs.clone())
    }

    /// Which bookmark a live connection belongs to.
    pub fn bookmark_of(&self, vfs: &Arc<dyn Vfs>) -> Option<String> {
        self.connections
            .iter()
            .find(|c| Arc::ptr_eq(&c.vfs, vfs))
            .map(|c| c.bookmark_id.clone())
    }
}
