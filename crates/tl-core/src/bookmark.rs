use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::VPath;

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

/// A folder saved inside one connection: what the quick-switch row under a
/// pane's location line is made of.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub id: String,
    /// Which connection it belongs to: the provider's
    /// [`location_key`](crate::Vfs::location_key), so two bookmarks opening
    /// the same host share its places, and one host's row never shows
    /// another's folders.
    pub host: String,
    pub name: String,
    /// Absolute path on that host.
    #[serde(with = "raw_path")]
    pub path: VPath,
}

impl Place {
    pub fn new(host: impl Into<String>, name: impl Into<String>, path: VPath) -> Self {
        Place {
            id: uuid::Uuid::new_v4().to_string(),
            host: host.into(),
            name: name.into(),
            path,
        }
    }
}

/// A path is bytes, because a remote name need not be UTF-8. One that is
/// valid UTF-8 is written as a plain string, so the file stays readable by
/// hand; anything else becomes a list of byte values, because writing it
/// lossily would leave a place pointing somewhere else.
mod raw_path {
    use std::fmt;

    use serde::de::{self, SeqAccess, Visitor};
    use serde::{Deserializer, Serializer};

    use crate::VPath;

    pub fn serialize<S: Serializer>(path: &VPath, s: S) -> Result<S::Ok, S::Error> {
        match std::str::from_utf8(path.as_bytes()) {
            Ok(text) => s.serialize_str(text),
            Err(_) => s.collect_seq(path.as_bytes().iter()),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<VPath, D::Error> {
        struct Raw;
        impl<'de> Visitor<'de> for Raw {
            type Value = VPath;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a path, as a string or a list of byte values")
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<VPath, E> {
                Ok(VPath::parse(text))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<VPath, A::Error> {
                let mut raw = Vec::new();
                while let Some(byte) = seq.next_element::<u8>()? {
                    raw.push(byte);
                }
                Ok(VPath::new(raw))
            }
        }
        d.deserialize_any(Raw)
    }
}

#[derive(Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    bookmarks: Vec<Bookmark>,
    #[serde(default)]
    places: Vec<Place>,
}

/// Bookmarks and places persisted as JSON in the application support
/// directory.
pub struct BookmarkStore {
    path: PathBuf,
    bookmarks: Vec<Bookmark>,
    places: Vec<Place>,
}

impl BookmarkStore {
    /// `~/Library/Application Support/Translocator/bookmarks.json` on macOS.
    pub fn default_path() -> PathBuf {
        app_data_dir().join("bookmarks.json")
    }

    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let (bookmarks, places) = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<StoreFile>(&bytes) {
                Ok(file) => (file.bookmarks, file.places),
                Err(err) => {
                    log::error!("cannot parse {}: {err}", path.display());
                    // Keep the unreadable file around instead of overwriting it.
                    let _ = std::fs::copy(&path, path.with_extension("json.broken"));
                    Default::default()
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Default::default(),
            Err(err) => {
                log::error!("cannot read {}: {err}", path.display());
                Default::default()
            }
        };
        BookmarkStore {
            path,
            bookmarks,
            places,
        }
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

    // ----- places ---------------------------------------------------------

    /// One host's places, in the order the user put them.
    pub fn places(&self, host: &str) -> Vec<Place> {
        self.places.iter().filter(|p| p.host == host).cloned().collect()
    }

    /// The n-th place of a host, which is what ⌘1…⌘9 jump to.
    pub fn place_at(&self, host: &str, index: usize) -> Option<&Place> {
        self.places.iter().filter(|p| p.host == host).nth(index)
    }

    /// The place for exactly this folder, if the folder is one.
    pub fn place_of(&self, host: &str, path: &VPath) -> Option<&Place> {
        self.places.iter().find(|p| p.host == host && &p.path == path)
    }

    pub fn add_place(&mut self, place: Place) -> io::Result<()> {
        self.places.push(place);
        self.save()
    }

    pub fn remove_place(&mut self, id: &str) -> io::Result<()> {
        self.places.retain(|p| p.id != id);
        self.save()
    }

    pub fn rename_place(&mut self, id: &str, name: impl Into<String>) -> io::Result<()> {
        if let Some(place) = self.places.iter_mut().find(|p| p.id == id) {
            place.name = name.into();
            return self.save();
        }
        Ok(())
    }

    /// Moves a place one position along its own host's row, stepping over
    /// the places of other hosts in between.
    pub fn move_place(&mut self, id: &str, delta: isize) -> io::Result<()> {
        let Some(place) = self.places.iter().find(|p| p.id == id) else {
            return Ok(());
        };
        let host = place.host.clone();
        let mine: Vec<usize> = self
            .places
            .iter()
            .enumerate()
            .filter(|(_, p)| p.host == host)
            .map(|(i, _)| i)
            .collect();
        let Some(at) = mine.iter().position(|i| self.places[*i].id == id) else {
            return Ok(());
        };
        let target = at as isize + delta;
        if target < 0 || target as usize >= mine.len() {
            return Ok(());
        }
        self.places.swap(mine[at], mine[target as usize]);
        self.save()
    }

    /// Atomic write (temp file + rename), readable only by the user.
    pub fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = StoreFile {
            bookmarks: self.bookmarks.clone(),
            places: self.places.clone(),
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

    fn store() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.json");
        (dir, path)
    }

    #[test]
    fn places_belong_to_one_host_each() {
        let (_dir, path) = store();
        let mut store = BookmarkStore::load(&path);
        let web = "deploy@web-1:22";
        let db = "root@db-1:22";
        for (host, name, at) in [
            (web, "logs", "/var/log"),
            (db, "data", "/var/lib/postgresql"),
            (web, "config", "/etc/nginx"),
        ] {
            store.add_place(Place::new(host, name, VPath::parse(at))).unwrap();
        }

        // A row shows its own host's folders, in the order they went in.
        let store = BookmarkStore::load(&path);
        let names: Vec<String> = store.places(web).iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, ["logs", "config"]);
        assert_eq!(store.places(db).len(), 1);
        assert!(store.places("nobody@nowhere:22").is_empty());

        // ⌘1 and ⌘2 count within the host, not across the file.
        assert_eq!(store.place_at(web, 1).unwrap().name, "config");
        assert!(store.place_at(web, 2).is_none());
        assert_eq!(store.place_at(db, 0).unwrap().name, "data");

        // The row marks the folder the pane is in.
        assert!(store.place_of(web, &VPath::parse("/etc/nginx")).is_some());
        assert!(store.place_of(db, &VPath::parse("/etc/nginx")).is_none());
        assert!(store.place_of(web, &VPath::parse("/etc")).is_none());
    }

    #[test]
    fn places_are_renamed_reordered_and_removed() {
        let (_dir, path) = store();
        let mut store = BookmarkStore::load(&path);
        let web = "deploy@web-1:22";
        let other = "root@db-1:22";
        // A place of another host sits between them in the file, so moving
        // one along its own row has to step over it.
        store.add_place(Place::new(web, "logs", VPath::parse("/var/log"))).unwrap();
        store.add_place(Place::new(other, "data", VPath::parse("/data"))).unwrap();
        store.add_place(Place::new(web, "config", VPath::parse("/etc"))).unwrap();
        let logs = store.places(web)[0].id.clone();

        store.rename_place(&logs, "Logs").unwrap();
        store.move_place(&logs, 1).unwrap();
        let store = BookmarkStore::load(&path);
        let names: Vec<String> = store.places(web).iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, ["config", "Logs"]);
        assert_eq!(store.places(other).len(), 1, "the other host is untouched");

        // Past either end nothing moves, and nothing is lost.
        let mut store = BookmarkStore::load(&path);
        store.move_place(&logs, 1).unwrap();
        assert_eq!(store.places(web).len(), 2);
        assert_eq!(store.places(web)[1].name, "Logs");

        store.remove_place(&logs).unwrap();
        let store = BookmarkStore::load(&path);
        assert_eq!(store.places(web).len(), 1);
        assert_eq!(store.places(other).len(), 1);
    }

    #[test]
    fn a_place_keeps_a_path_that_is_not_utf8() {
        let (_dir, path) = store();
        let mut store = BookmarkStore::load(&path);
        let odd = VPath::new(b"/srv/\xff\xfe/data");
        store.add_place(Place::new("h", "odd", odd.clone())).unwrap();
        store.add_place(Place::new("h", "plain", VPath::parse("/srv/ünï"))).unwrap();

        let store = BookmarkStore::load(&path);
        assert_eq!(store.places("h")[0].path, odd, "the bytes have to survive");
        assert_eq!(store.places("h")[1].path, VPath::parse("/srv/ünï"));
        // A path that is text stays readable in the file; the other does not
        // pretend to be.
        let json = std::fs::read_to_string(&path).unwrap();
        assert!(json.contains("\"/srv/ünï\""), "{json}");
        assert!(json.contains("255"), "{json}");
    }
}
