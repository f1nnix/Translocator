//! What the core knows about a live file system, and how locations on one
//! are written for people.

use serde::{Deserialize, Serialize};
use tl_model::{Capabilities, VPath};

use crate::ids::{ConnId, LOCAL};

/// The plugin id of the local file system.
pub const LOCAL_PLUGIN: &str = "local";

/// A live file system, as described by the shell when it was opened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConnInfo {
    pub id: ConnId,
    /// Plugin that created it ("local", "ssh").
    pub plugin: String,
    /// Short label for the UI ("deploy@web-1").
    pub label: String,
    /// Two file systems with the same key see the same files, so copies
    /// between them run inside the provider.
    pub location_key: String,
    pub caps: Capabilities,
    pub home: VPath,
    /// The bookmark it was opened from.
    pub bookmark: Option<String>,
    /// For an archive opened as a folder: the file system holding it, and
    /// the archive file.
    pub host: Option<(ConnId, VPath)>,
    /// Server description ("Debian 12 · GNU tools").
    pub server_info: Option<String>,
    /// The user's home directory on this machine, for writing `~`.
    pub local_home: Option<String>,
    /// For the local file system: how messages name this machine ("this
    /// Mac", "this computer"). The shell knows which platform it is.
    #[serde(default)]
    pub machine: Option<String>,
}

impl ConnInfo {
    pub fn is_local(&self) -> bool {
        self.plugin == LOCAL_PLUGIN
    }

    pub fn is_archive(&self) -> bool {
        self.host.is_some()
    }
}

/// `~/Projects` for a path under `home`, else the path.
pub fn tilde(path: &VPath, home: Option<&str>) -> String {
    let text = path.display();
    if let Some(home) = home.filter(|h| !h.is_empty()) {
        if text == home {
            return "~".into();
        }
        if let Some(rest) = text.strip_prefix(&format!("{home}/")) {
            return format!("~/{rest}");
        }
    }
    text
}

/// The connections the core knows, by id.
#[derive(Clone, Debug, Default)]
pub struct Conns {
    list: Vec<ConnInfo>,
    /// Connections the health watch reported as gone.
    dropped: Vec<ConnId>,
}

impl Conns {
    pub fn get(&self, id: ConnId) -> Option<&ConnInfo> {
        self.list.iter().find(|c| c.id == id)
    }

    pub fn insert(&mut self, info: ConnInfo) {
        self.dropped.retain(|d| *d != info.id);
        match self.list.iter_mut().find(|c| c.id == info.id) {
            Some(slot) => *slot = info,
            None => self.list.push(info),
        }
    }

    pub fn remove(&mut self, id: ConnId) -> Option<ConnInfo> {
        let at = self.list.iter().position(|c| c.id == id)?;
        self.dropped.retain(|d| *d != id);
        Some(self.list.remove(at))
    }

    pub fn all(&self) -> &[ConnInfo] {
        &self.list
    }

    pub fn local(&self) -> Option<&ConnInfo> {
        self.get(LOCAL)
    }

    pub fn set_connected(&mut self, id: ConnId, connected: bool) {
        self.dropped.retain(|d| *d != id);
        if !connected {
            self.dropped.push(id);
        }
    }

    pub fn is_connected(&self, id: ConnId) -> bool {
        !self.dropped.contains(&id)
    }

    /// The live server connection of a bookmark.
    pub fn of_bookmark(&self, bookmark: &str) -> Option<&ConnInfo> {
        self.list
            .iter()
            .find(|c| c.host.is_none() && c.bookmark.as_deref() == Some(bookmark) && self.is_connected(c.id))
    }

    /// The real file system under `id`: itself, or for an archive the one
    /// holding the archive file.
    pub fn host_of(&self, id: ConnId) -> ConnId {
        match self.get(id).and_then(|c| c.host.as_ref()) {
            Some((host, _)) => self.host_of(*host),
            None => id,
        }
    }

    /// `~/Projects` locally, `deploy@web-1:/etc` on a server, the host's
    /// path inside an archive (`~/site.tar.gz/www`).
    pub fn display(&self, id: ConnId, path: &VPath) -> String {
        let host = self.host_of(id);
        match self.get(host) {
            Some(info) if info.is_local() => tilde(path, info.local_home.as_deref()),
            Some(info) => format!("{}:{}", info.label, path),
            None => path.display(),
        }
    }

    /// `/local/path` or `user@host:/remote/path`, for the clipboard.
    pub fn full_path(&self, id: ConnId, path: &VPath) -> String {
        let host = self.host_of(id);
        match self.get(host) {
            Some(info) if !info.is_local() => format!("{}:{}", info.label, path),
            _ => path.display(),
        }
    }

    /// "this Mac" or the server's label.
    pub fn place_name(&self, id: ConnId) -> String {
        let host = self.host_of(id);
        match self.get(host) {
            Some(info) if !info.is_local() => info.label.clone(),
            _ => self.machine(),
        }
    }

    /// How messages name the local machine: "this Mac", "this computer".
    pub fn machine(&self) -> String {
        self.local()
            .and_then(|c| c.machine.clone())
            .unwrap_or_else(|| THIS_MACHINE.into())
    }

    pub fn label(&self, id: ConnId) -> String {
        self.get(id).map(|c| c.label.clone()).unwrap_or_default()
    }

    pub fn caps(&self, id: ConnId) -> Capabilities {
        self.get(id).map(|c| c.caps).unwrap_or_default()
    }

    pub fn is_local(&self, id: ConnId) -> bool {
        self.get(id).is_some_and(ConnInfo::is_local)
    }

    pub fn location_key(&self, id: ConnId) -> String {
        self.get(id).map(|c| c.location_key.clone()).unwrap_or_default()
    }
}

/// How the local machine is named when the shell does not say.
pub const THIS_MACHINE: &str = "this computer";
