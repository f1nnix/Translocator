//! Bookmarks (saved connections) and places (saved folders inside one
//! connection), and every edit made to them. Reading and writing the file
//! is `tl_core::bookmark`; everything here is plain data.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::path::VPath;

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
    /// A bookmark with a fresh random id.
    #[cfg(feature = "uuid")]
    pub fn new(name: impl Into<String>, plugin: impl Into<String>) -> Self {
        Self::with_id(uuid::Uuid::new_v4().to_string(), name, plugin)
    }

    pub fn with_id(id: impl Into<String>, name: impl Into<String>, plugin: impl Into<String>) -> Self {
        Bookmark {
            id: id.into(),
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
    /// Which connection it belongs to: the provider's location key, so two
    /// bookmarks opening the same host share its places, and one host's row
    /// never shows another's folders.
    pub host: String,
    pub name: String,
    /// Absolute path on that host.
    pub path: VPath,
}

impl Place {
    /// A place with a fresh random id.
    #[cfg(feature = "uuid")]
    pub fn new(host: impl Into<String>, name: impl Into<String>, path: VPath) -> Self {
        Self::with_id(uuid::Uuid::new_v4().to_string(), host, name, path)
    }

    pub fn with_id(id: impl Into<String>, host: impl Into<String>, name: impl Into<String>, path: VPath) -> Self {
        Place {
            id: id.into(),
            host: host.into(),
            name: name.into(),
            path,
        }
    }
}

/// Everything in the bookmarks file, and the edits made to it. Each edit
/// says whether it changed anything, so the caller knows when to save.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bookmarks {
    #[serde(default)]
    pub bookmarks: Vec<Bookmark>,
    #[serde(default)]
    pub places: Vec<Place>,
}

impl Bookmarks {
    pub fn all(&self) -> &[Bookmark] {
        &self.bookmarks
    }

    pub fn get(&self, id: &str) -> Option<&Bookmark> {
        self.bookmarks.iter().find(|b| b.id == id)
    }

    /// Inserts or replaces (by id).
    pub fn upsert(&mut self, bookmark: Bookmark) -> bool {
        match self.bookmarks.iter_mut().find(|b| b.id == bookmark.id) {
            Some(existing) if *existing == bookmark => return false,
            Some(existing) => *existing = bookmark,
            None => self.bookmarks.push(bookmark),
        }
        true
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.bookmarks.len();
        self.bookmarks.retain(|b| b.id != id);
        self.bookmarks.len() != before
    }

    /// Moves a bookmark one position up or down.
    pub fn reorder(&mut self, id: &str, delta: isize) -> bool {
        if let Some(idx) = self.bookmarks.iter().position(|b| b.id == id) {
            let target = idx as isize + delta;
            if target >= 0 && (target as usize) < self.bookmarks.len() {
                self.bookmarks.swap(idx, target as usize);
                return true;
            }
        }
        false
    }

    /// One host's places, in the order the user put them.
    pub fn places(&self, host: &str) -> Vec<Place> {
        self.places.iter().filter(|p| p.host == host).cloned().collect()
    }

    /// Whether a host has any place at all, without collecting them.
    pub fn has_places(&self, host: &str) -> bool {
        self.places.iter().any(|p| p.host == host)
    }

    /// The n-th place of a host, which is what ⌘1…⌘9 jump to.
    pub fn place_at(&self, host: &str, index: usize) -> Option<&Place> {
        self.places.iter().filter(|p| p.host == host).nth(index)
    }

    /// The place for exactly this folder, if the folder is one.
    pub fn place_of(&self, host: &str, path: &VPath) -> Option<&Place> {
        self.places.iter().find(|p| p.host == host && &p.path == path)
    }

    pub fn add_place(&mut self, place: Place) -> bool {
        self.places.push(place);
        true
    }

    pub fn remove_place(&mut self, id: &str) -> bool {
        let before = self.places.len();
        self.places.retain(|p| p.id != id);
        self.places.len() != before
    }

    pub fn rename_place(&mut self, id: &str, name: impl Into<String>) -> bool {
        match self.places.iter_mut().find(|p| p.id == id) {
            Some(place) => {
                place.name = name.into();
                true
            }
            None => false,
        }
    }

    /// Moves a place one position along its own host's row, stepping over
    /// the places of other hosts in between.
    pub fn move_place(&mut self, id: &str, delta: isize) -> bool {
        let Some(place) = self.places.iter().find(|p| p.id == id) else {
            return false;
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
            return false;
        };
        let target = at as isize + delta;
        if target < 0 || target as usize >= mine.len() {
            return false;
        }
        self.places.swap(mine[at], mine[target as usize]);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(id: &str, host: &str, name: &str, at: &str) -> Place {
        Place::with_id(id, host, name, VPath::parse(at))
    }

    #[test]
    fn places_belong_to_one_host_each() {
        let web = "deploy@web-1:22";
        let db = "root@db-1:22";
        let mut store = Bookmarks::default();
        store.add_place(place("1", web, "logs", "/var/log"));
        store.add_place(place("2", db, "data", "/var/lib/postgresql"));
        store.add_place(place("3", web, "config", "/etc/nginx"));
        let names: Vec<String> = store.places(web).iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, ["logs", "config"]);
        assert!(store.has_places(db));
        assert!(!store.has_places("nobody@nowhere:22"));
        assert_eq!(store.place_at(web, 1).unwrap().name, "config");
        assert!(store.place_at(web, 2).is_none());
        assert!(store.place_of(web, &VPath::parse("/etc/nginx")).is_some());
        assert!(store.place_of(db, &VPath::parse("/etc/nginx")).is_none());
    }

    #[test]
    fn moving_a_place_steps_over_other_hosts() {
        let mut store = Bookmarks::default();
        store.add_place(place("1", "web", "logs", "/var/log"));
        store.add_place(place("2", "db", "data", "/data"));
        store.add_place(place("3", "web", "config", "/etc"));
        assert!(store.rename_place("1", "Logs"));
        assert!(store.move_place("1", 1));
        let names: Vec<String> = store.places("web").iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, ["config", "Logs"]);
        assert!(!store.move_place("1", 1), "past the end nothing moves");
        assert!(store.remove_place("1"));
        assert_eq!(store.places("web").len(), 1);
        assert_eq!(store.places("db").len(), 1);
    }

    #[test]
    fn an_unchanged_bookmark_is_not_an_edit() {
        let mut store = Bookmarks::default();
        let mut b = Bookmark::with_id("a", "web", "ssh");
        b.set("host", " web-1 ");
        assert!(store.upsert(b.clone()));
        assert!(!store.upsert(b.clone()));
        assert_eq!(store.get("a").unwrap().get("host"), "web-1");
        assert!(!store.reorder("a", 1), "a lone bookmark has nowhere to go");
    }

    #[test]
    fn a_place_keeps_a_path_that_is_not_utf8() {
        let mut store = Bookmarks::default();
        let odd = VPath::new(b"/srv/\xff\xfe/data");
        store.add_place(Place::with_id("1", "h", "odd", odd.clone()));
        store.add_place(Place::with_id("2", "h", "plain", VPath::parse("/srv/ünï")));
        let json = serde_json::to_string(&store).unwrap();
        assert!(json.contains("\"/srv/ünï\""), "{json}");
        assert!(json.contains("255"), "{json}");
        let back: Bookmarks = serde_json::from_str(&json).unwrap();
        assert_eq!(back.places("h")[0].path, odd, "the bytes have to survive");
    }
}
