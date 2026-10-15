use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Plugin-defined settings of a bookmark. Values are strings; toggles are
/// stored as `"true"`/`"false"`.
pub type BookmarkConfig = BTreeMap<String, String>;

/// A saved location: which plugin to use and how to configure it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: String,
    pub name: String,
    /// Id of the provider plugin, e.g. `"ssh"`.
    pub plugin: String,
    #[serde(default)]
    pub config: BookmarkConfig,
}

impl Bookmark {
    pub fn new(name: impl Into<String>, plugin: impl Into<String>) -> Self {
        Bookmark {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            plugin: plugin.into(),
            config: BookmarkConfig::new(),
        }
    }

    /// A config value, trimmed; empty string when unset.
    pub fn get(&self, key: &str) -> &str {
        self.config.get(key).map(|s| s.trim()).unwrap_or("")
    }

    pub fn flag(&self, key: &str) -> bool {
        matches!(self.get(key), "true" | "1" | "yes")
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.config.insert(key.into(), value.into());
    }
}

#[derive(Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    bookmarks: Vec<Bookmark>,
}

/// Bookmarks persisted as JSON in the application support directory.
pub struct BookmarkStore {
    path: PathBuf,
    bookmarks: Vec<Bookmark>,
}

impl BookmarkStore {
    /// `~/Library/Application Support/Translocator/bookmarks.json` on macOS.
    pub fn default_path() -> PathBuf {
        app_data_dir().join("bookmarks.json")
    }

    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let bookmarks = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<StoreFile>(&bytes) {
                Ok(file) => file.bookmarks,
                Err(err) => {
                    log::error!("cannot parse {}: {err}", path.display());
                    // Keep the unreadable file around instead of overwriting it.
                    let _ = std::fs::copy(&path, path.with_extension("json.broken"));
                    Vec::new()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                log::error!("cannot read {}: {err}", path.display());
                Vec::new()
            }
        };
        BookmarkStore { path, bookmarks }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn all(&self) -> &[Bookmark] {
        &self.bookmarks
    }

    pub fn get(&self, id: &str) -> Option<&Bookmark> {
        self.bookmarks.iter().find(|b| b.id == id)
    }

    /// Inserts or replaces (by id) and saves.
    pub fn upsert(&mut self, bookmark: Bookmark) -> io::Result<()> {
        match self.bookmarks.iter_mut().find(|b| b.id == bookmark.id) {
            Some(existing) => *existing = bookmark,
            None => self.bookmarks.push(bookmark),
        }
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> io::Result<()> {
        self.bookmarks.retain(|b| b.id != id);
        self.save()
    }

    /// Moves a bookmark one position up or down.
    pub fn reorder(&mut self, id: &str, delta: isize) -> io::Result<()> {
        if let Some(idx) = self.bookmarks.iter().position(|b| b.id == id) {
            let target = idx as isize + delta;
            if target >= 0 && (target as usize) < self.bookmarks.len() {
                self.bookmarks.swap(idx, target as usize);
                return self.save();
            }
        }
        Ok(())
    }

    /// Atomic write (temp file + rename), readable only by the user.
    pub fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = StoreFile {
            bookmarks: self.bookmarks.clone(),
        };
        let json = serde_json::to_vec_pretty(&file).map_err(io::Error::other)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &self.path)
    }
}

/// Per-user data directory of the app.
pub fn app_data_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("Translocator")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.json");
        let mut store = BookmarkStore::load(&path);
        assert!(store.all().is_empty());
        let mut b = Bookmark::new("web", "ssh");
        b.set("host", " web-1 ");
        b.set("sudo", "true");
        store.upsert(b.clone()).unwrap();
        let store = BookmarkStore::load(&path);
        assert_eq!(store.all(), &[b.clone()]);
        assert_eq!(store.get(&b.id).unwrap().get("host"), "web-1");
        assert!(store.get(&b.id).unwrap().flag("sudo"));
    }
}
