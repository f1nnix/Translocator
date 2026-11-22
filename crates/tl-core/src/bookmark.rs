//! Bookmarks and places persisted as JSON in the application support
//! directory. The data and its edits are [`tl_model::bookmark`]; this is
//! the file they live in.

use std::io;
use std::path::{Path, PathBuf};

pub use tl_model::bookmark::*;

use crate::VPath;

/// Bookmarks and places persisted as JSON in the application support
/// directory.
pub struct BookmarkStore {
    path: PathBuf,
    data: Bookmarks,
}

impl BookmarkStore {
    /// `~/Library/Application Support/Translocator/bookmarks.json` on macOS.
    pub fn default_path() -> PathBuf {
        app_data_dir().join("bookmarks.json")
    }

    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let data = read(&path);
        BookmarkStore { path, data }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Everything in the file.
    pub fn data(&self) -> &Bookmarks {
        &self.data
    }

    pub fn all(&self) -> &[Bookmark] {
        self.data.all()
    }

    pub fn get(&self, id: &str) -> Option<&Bookmark> {
        self.data.get(id)
    }

    /// Inserts or replaces (by id) and saves.
    pub fn upsert(&mut self, bookmark: Bookmark) -> io::Result<()> {
        self.data.upsert(bookmark);
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> io::Result<()> {
        self.data.remove(id);
        self.save()
    }

    /// Moves a bookmark one position up or down.
    pub fn reorder(&mut self, id: &str, delta: isize) -> io::Result<()> {
        if self.data.reorder(id, delta) {
            return self.save();
        }
        Ok(())
    }

    /// One host's places, in the order the user put them.
    pub fn places(&self, host: &str) -> Vec<Place> {
        self.data.places(host)
    }

    /// The n-th place of a host, which is what ⌘1…⌘9 jump to.
    pub fn place_at(&self, host: &str, index: usize) -> Option<&Place> {
        self.data.place_at(host, index)
    }

    /// The place for exactly this folder, if the folder is one.
    pub fn place_of(&self, host: &str, path: &VPath) -> Option<&Place> {
        self.data.place_of(host, path)
    }

    pub fn add_place(&mut self, place: Place) -> io::Result<()> {
        self.data.add_place(place);
        self.save()
    }

    pub fn remove_place(&mut self, id: &str) -> io::Result<()> {
        self.data.remove_place(id);
        self.save()
    }

    pub fn rename_place(&mut self, id: &str, name: impl Into<String>) -> io::Result<()> {
        if self.data.rename_place(id, name) {
            return self.save();
        }
        Ok(())
    }

    /// Moves a place one position along its own host's row.
    pub fn move_place(&mut self, id: &str, delta: isize) -> io::Result<()> {
        if self.data.move_place(id, delta) {
            return self.save();
        }
        Ok(())
    }

    pub fn save(&self) -> io::Result<()> {
        write(&self.path, &self.data)
    }
}

/// Reads a bookmarks file. A missing file is an empty one; a file that
/// cannot be parsed is copied aside rather than overwritten later, so a
/// hand edit with a comma too few can be repaired.
pub fn read(path: &Path) -> Bookmarks {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Bookmarks>(&bytes) {
            Ok(file) => file,
            Err(err) => {
                log::error!("cannot parse {}: {err}", path.display());
                let _ = std::fs::copy(path, path.with_extension("json.broken"));
                Bookmarks::default()
            }
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => Bookmarks::default(),
        Err(err) => {
            log::error!("cannot read {}: {err}", path.display());
            Bookmarks::default()
        }
    }
}

/// Atomic write (temp file + rename), readable only by the user.
pub fn write(path: &Path, data: &Bookmarks) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_vec_pretty(data).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)
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
