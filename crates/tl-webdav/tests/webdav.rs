//! End-to-end tests against a real WebDAV server.
//!
//! `scripts/webdav-test-server.sh` starts one in Docker and prints the
//! environment to export. Without `TL_WEBDAV_TEST_URL` every test returns
//! immediately, so a plain `cargo test` still works on a machine with no
//! server. CI starts one, so there the whole suite runs.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use tl_core::prompt::DenyPrompter;
use tl_core::transfer::{self, ConflictChoice, FixedResolver, TransferMode, TransferSpec};
use tl_core::{
    Bookmark, CancelToken, Entry, OpCtx, PluginEnv, Progress, VPath, Vfs, VfsError, VfsResult,
};
use tl_webdav::DavVfs;

const WEIRD: &str = "it's \"odd\" + ünï [1].txt";

struct Server {
    vfs: Arc<DavVfs>,
    root: String,
    _tmp: tempfile::TempDir,
}

impl Server {
    /// `/tl-test-…/<rest>`
    fn path(&self, rest: &str) -> VPath {
        let base = VPath::root().child(self.root.as_bytes());
        if rest.is_empty() { base } else { base.join(rest) }
    }

    fn vfs(&self) -> Arc<dyn Vfs> {
        self.vfs.clone()
    }

    fn put(&self, rest: &str, text: &str) {
        let path = self.path(rest);
        if let Some(parent) = path.parent() {
            self.mkdirs(&parent);
        }
        self.vfs
            .write_file(&path, text.as_bytes(), &OpCtx::new())
            .unwrap_or_else(|e| panic!("cannot write {rest}: {e}"));
    }

    /// WebDAV has no implicit folders, so a test fixture has to build them.
    fn mkdirs(&self, path: &VPath) {
        let mut here = VPath::root();
        for part in path.as_bytes().split(|b| *b == b'/').filter(|p| !p.is_empty()) {
            here = here.child(part);
            if self.vfs.stat(&here, &OpCtx::new()).unwrap().is_none() {
                self.vfs
                    .mkdir(&here, &OpCtx::new())
                    .unwrap_or_else(|e| panic!("cannot create {here}: {e}"));
            }
        }
    }

    fn read(&self, rest: &str) -> String {
        let bytes = self
            .vfs
            .read_file(&self.path(rest), u64::MAX, &OpCtx::new())
            .unwrap_or_else(|e| panic!("cannot read {rest}: {e}"));
        String::from_utf8(bytes).unwrap()
    }

    fn exists(&self, rest: &str) -> bool {
        self.vfs
            .stat(&self.path(rest), &OpCtx::new())
            .unwrap()
            .is_some()
    }

    fn list(&self, rest: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .vfs
            .list(&self.path(rest), &OpCtx::new())
            .unwrap_or_else(|e| panic!("cannot list {rest}: {e}"))
            .iter()
            .map(Entry::name_lossy)
            .collect();
        names.sort();
        names
    }

    fn walked(&self, rest: &str) -> Vec<String> {
        let mut rels: Vec<String> = self
            .vfs
            .walk(&self.path(rest), &OpCtx::new())
            .unwrap_or_else(|e| panic!("cannot walk {rest}: {e}"))
            .iter()
            .map(|w| String::from_utf8_lossy(&w.rel).into_owned())
            .collect();
        rels.sort();
        rels
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Leaving the tree behind would make a rerun fail.
        let _ = self
            .vfs
            .delete(&[self.path("")], &OpCtx::new(), &Progress::new());
    }
}

/// A connection plus an empty folder of its own, or `None` when no test
/// server is configured.
fn server() -> Option<Server> {
    let url = std::env::var("TL_WEBDAV_TEST_URL")
        .ok()
        .filter(|u| !u.is_empty())?;
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let root = format!(
        "tl-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let tmp = tempfile::tempdir().unwrap();

    let mut bookmark = Bookmark::new("dav", "webdav");
    bookmark.set("url", url);
    if let Ok(user) = std::env::var("TL_WEBDAV_TEST_USER") {
        bookmark.set("user", user);
        bookmark.set(
            "password",
            std::env::var("TL_WEBDAV_TEST_PASSWORD").unwrap_or_default(),
        );
    }
    let env = PluginEnv {
        prompter: Arc::new(DenyPrompter),
        askpass: None,
        runtime_dir: tmp.path().to_path_buf(),
    };
    let vfs = tl_webdav::connect(&bookmark, &env, &CancelToken::new())
        .expect("cannot reach the WebDAV test server");
    vfs.mkdir(&VPath::root().child(root.as_bytes()), &OpCtx::new())
        .expect("cannot create the test folder");
    Some(Server {
        vfs,
        root,
        _tmp: tmp,
    })
}

fn spec(
    mode: TransferMode,
    src: Arc<dyn Vfs>,
    src_dir: VPath,
    names: &[&str],
    dst: Arc<dyn Vfs>,
    dst_dir: VPath,
) -> TransferSpec {
    TransferSpec {
        mode,
        src,
        src_dir,
        names: names.iter().map(|n| n.as_bytes().to_vec()).collect(),
        src_elevated: false,
        dst,
        dst_dir,
        dst_elevated: false,
        target_name: None,
    }
}

fn run(spec: &TransferSpec, choice: ConflictChoice) -> VfsResult<transfer::TransferReport> {
    transfer::run(
        spec,
        &FixedResolver(choice),
        &CancelToken::new(),
        &Progress::new(),
    )
}

#[test]
fn lists_files_and_collections() {
    let Some(s) = server() else { return };

    s.put("notes.txt", "hello");
    s.put(&format!("docs/{WEIRD}"), "weird");
    s.put("docs/deep/inner.txt", "inner");
    s.vfs.mkdir(&s.path("empty-folder"), &OpCtx::new()).unwrap();

    assert_eq!(s.list(""), ["docs", "empty-folder", "notes.txt"]);
    // A name with spaces, quotes and non-ASCII has to survive the round
    // trip through the href the server sends back percent-encoded.
    assert_eq!(s.list("docs"), ["deep", WEIRD]);
    assert_eq!(s.list("empty-folder"), Vec::<String>::new());

    let file = s
        .vfs
        .stat(&s.path("notes.txt"), &OpCtx::new())
        .unwrap()
        .unwrap();
    assert!(!file.is_dir());
    assert_eq!(file.size, 5);
    assert!(file.mtime.unwrap() > 1_600_000_000, "{:?}", file.mtime);

    let folder = s.vfs.stat(&s.path("docs"), &OpCtx::new()).unwrap().unwrap();
    assert!(folder.is_dir());
    assert!(
        s.vfs
            .stat(&s.path("nothing"), &OpCtx::new())
            .unwrap()
            .is_none(),
        "a missing path is None, not an error"
    );

    assert_eq!(
        s.walked(""),
        [
            "docs",
            "docs/deep",
            "docs/deep/inner.txt",
            &format!("docs/{WEIRD}"),
            "empty-folder",
            "notes.txt",
        ]
    );

    let totals = s.vfs.scan(&[s.path("")], &OpCtx::new()).unwrap();
    assert_eq!(totals.files, 3);
    assert_eq!(totals.dirs, 4, "the walked root counts too");
    assert_eq!(totals.bytes, 5 + 5 + 5);

    assert_eq!(s.read(&format!("docs/{WEIRD}")), "weird");
    // The viewer asks for a prefix, which goes out as a Range request;
    // the answer is cut to the limit either way, because a server may pay
    // the header no attention and send the whole file.
    let head = s
        .vfs
        .read_file(&s.path("notes.txt"), 3, &OpCtx::new())
        .unwrap();
    assert_eq!(head, b"hel");
    let all = s
        .vfs
        .read_file(&s.path("notes.txt"), 500, &OpCtx::new())
        .unwrap();
    assert_eq!(all, b"hello", "a limit past the end is not an error");
}

#[test]
fn walks_folder_by_folder_when_a_deep_propfind_is_refused() {
    let Some(s) = server() else { return };
    s.put("tree/a.txt", "a");
    s.put("tree/sub/b.txt", "b");
    s.put("tree/sub/deeper/c.txt", "c");

    let deep = s.walked("tree");
    // Apache refuses Depth: infinity by default, so the fallback has to
    // produce exactly the same tree.
    s.vfs.walk_folder_by_folder();
    assert_eq!(s.walked("tree"), deep);
    assert_eq!(
        deep,
        ["a.txt", "sub", "sub/b.txt", "sub/deeper", "sub/deeper/c.txt"]
    );
}

#[test]
fn renames_files_and_folders() {
    let Some(s) = server() else { return };
    s.put("one.txt", "one");
    s.put("tree/a.txt", "a");
    s.put("tree/sub/b.txt", "b");

    s.vfs
        .rename(&s.path("one.txt"), &s.path("renamed.txt"), &OpCtx::new())
        .unwrap();
    assert!(!s.exists("one.txt"));
    assert_eq!(s.read("renamed.txt"), "one");

    // A collection moves with everything under it, in one request.
    s.vfs
        .rename(&s.path("tree"), &s.path("moved"), &OpCtx::new())
        .unwrap();
    assert!(!s.exists("tree"));
    assert_eq!(s.read("moved/a.txt"), "a");
    assert_eq!(s.read("moved/sub/b.txt"), "b");

    // Renaming onto something that exists must not quietly replace it,
    // even though MOVE with Overwrite: T would.
    s.put("taken.txt", "keep");
    let err = s
        .vfs
        .rename(&s.path("renamed.txt"), &s.path("taken.txt"), &OpCtx::new())
        .unwrap_err();
    assert!(matches!(err, VfsError::AlreadyExists(_)), "{err:?}");
    assert_eq!(s.read("taken.txt"), "keep");

    // So must creating something that is already there.
    let err = s.vfs.mkdir(&s.path("moved"), &OpCtx::new()).unwrap_err();
    assert!(matches!(err, VfsError::AlreadyExists(_)), "{err:?}");
    let err = s
        .vfs
        .create_file(&s.path("taken.txt"), &OpCtx::new())
        .unwrap_err();
    assert!(matches!(err, VfsError::AlreadyExists(_)), "{err:?}");
    assert_eq!(s.read("taken.txt"), "keep");
}

#[test]
fn copies_and_moves_on_the_server() {
    let Some(s) = server() else { return };
    s.put("src/keep.txt", "new");
    s.put("src/only-src.txt", "only");
    s.put("src/deep/x.txt", "x");
    s.put("dst/keep.txt", "old");

    // Same connection on both sides: COPY, and no data through this
    // machine.
    let copy = spec(
        TransferMode::Copy,
        s.vfs(),
        s.path(""),
        &["src"],
        s.vfs(),
        s.path("dst"),
    );
    let report = run(&copy, ConflictChoice::Cancel).unwrap();
    assert!(
        report.server_side,
        "a copy within one server must not stream"
    );
    assert_eq!(s.read("dst/src/keep.txt"), "new");
    assert_eq!(s.read("dst/src/deep/x.txt"), "x");
    assert_eq!(s.read("src/keep.txt"), "new", "a copy leaves the source");

    // Merge, keeping what the destination already has.
    let merge = spec(
        TransferMode::Copy,
        s.vfs(),
        s.path("src"),
        &["keep.txt"],
        s.vfs(),
        s.path("dst"),
    );
    run(&merge, ConflictChoice::SkipExisting).unwrap();
    assert_eq!(s.read("dst/keep.txt"), "old");
    run(&merge, ConflictChoice::Overwrite).unwrap();
    assert_eq!(s.read("dst/keep.txt"), "new");

    // Keep both renames while copying.
    run(&merge, ConflictChoice::KeepBoth).unwrap();
    assert_eq!(s.read("dst/keep (2).txt"), "new");

    // A move takes the source with it.
    let move_it = spec(
        TransferMode::Move,
        s.vfs(),
        s.path("src"),
        &["only-src.txt"],
        s.vfs(),
        s.path("dst"),
    );
    let report = run(&move_it, ConflictChoice::Cancel).unwrap();
    assert!(report.server_side, "a move within one server must not stream");
    assert_eq!(s.read("dst/only-src.txt"), "only");
    assert!(!s.exists("src/only-src.txt"));
}

#[test]
fn merging_a_folder_move_keeps_what_it_skipped() {
    let Some(s) = server() else { return };
    s.put("src/tree/moved.txt", "fresh");
    s.put("src/tree/kept.txt", "fresh");
    s.put("dst/tree/kept.txt", "already here");

    let move_it = spec(
        TransferMode::Move,
        s.vfs(),
        s.path("src"),
        &["tree"],
        s.vfs(),
        s.path("dst"),
    );
    run(&move_it, ConflictChoice::SkipExisting).unwrap();

    assert_eq!(s.read("dst/tree/moved.txt"), "fresh");
    assert_eq!(s.read("dst/tree/kept.txt"), "already here");
    // The file that was skipped is still the source's, so the source tree
    // has to survive the move.
    assert_eq!(s.read("src/tree/kept.txt"), "fresh");
    assert!(!s.exists("src/tree/moved.txt"));
}

fn local_tree(root: &Path) {
    fs::create_dir_all(root.join("proj/src")).unwrap();
    fs::write(root.join("proj/src/main.rs"), "fn main() {}").unwrap();
    fs::write(root.join("proj/README"), "readme").unwrap();
    fs::write(root.join("proj").join(WEIRD), "weird").unwrap();
    fs::create_dir_all(root.join("proj/empty")).unwrap();
}

#[test]
fn streams_between_local_and_webdav() {
    let Some(s) = server() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let local_root = tmp.path().canonicalize().unwrap();
    local_tree(&local_root);
    let local = tl_local::local_vfs();

    // Upload: different locations, so the engine streams a tar.
    let up = spec(
        TransferMode::Copy,
        local.clone(),
        VPath::from_local(&local_root),
        &["proj"],
        s.vfs(),
        s.path(""),
    );
    let report = run(&up, ConflictChoice::Cancel).unwrap();
    assert!(!report.server_side, "local to WebDAV has to stream");
    assert_eq!(s.read("proj/src/main.rs"), "fn main() {}");
    assert_eq!(s.read(&format!("proj/{WEIRD}")), "weird");
    assert!(s.exists("proj/empty"), "an empty folder is a collection");

    // Download the same tree back and compare it with the original.
    let back: PathBuf = local_root.join("back");
    fs::create_dir(&back).unwrap();
    let down = spec(
        TransferMode::Copy,
        s.vfs(),
        s.path(""),
        &["proj"],
        local.clone(),
        VPath::from_local(&back),
    );
    run(&down, ConflictChoice::Cancel).unwrap();
    assert_eq!(
        fs::read_to_string(back.join("proj/src/main.rs")).unwrap(),
        "fn main() {}"
    );
    assert_eq!(
        fs::read_to_string(back.join("proj").join(WEIRD)).unwrap(),
        "weird"
    );
    assert!(back.join("proj/empty").is_dir());
}

#[test]
fn streams_a_file_bigger_than_one_buffer() {
    let Some(s) = server() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    fs::create_dir(root.join("big")).unwrap();
    // Over the 8 MiB mark, so the upload really goes across the pipe
    // rather than from a buffer, and a wrong length or a chunked PUT would
    // show up as corruption.
    let payload: Vec<u8> = (0..=255u8).cycle().take(12 * 1024 * 1024).collect();
    fs::write(root.join("big/blob.bin"), &payload).unwrap();
    let local = tl_local::local_vfs();

    let up = spec(
        TransferMode::Copy,
        local.clone(),
        VPath::from_local(&root),
        &["big"],
        s.vfs(),
        s.path(""),
    );
    run(&up, ConflictChoice::Cancel).unwrap();

    let stat = s
        .vfs
        .stat(&s.path("big/blob.bin"), &OpCtx::new())
        .unwrap()
        .unwrap();
    assert_eq!(stat.size, payload.len() as u64);

    let back = root.join("back");
    fs::create_dir(&back).unwrap();
    let down = spec(
        TransferMode::Copy,
        s.vfs(),
        s.path(""),
        &["big"],
        local,
        VPath::from_local(&back),
    );
    run(&down, ConflictChoice::Cancel).unwrap();
    assert_eq!(
        fs::read(back.join("big/blob.bin")).unwrap(),
        payload,
        "the file has to come back byte for byte"
    );
}

#[test]
fn refuses_what_webdav_cannot_hold() {
    let Some(s) = server() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    fs::create_dir(root.join("tree")).unwrap();
    fs::write(root.join("tree/real.txt"), "real").unwrap();
    std::os::unix::fs::symlink("real.txt", root.join("tree/link")).unwrap();

    let up = spec(
        TransferMode::Move,
        tl_local::local_vfs(),
        VPath::from_local(&root),
        &["tree"],
        s.vfs(),
        s.path(""),
    );
    let err = run(&up, ConflictChoice::Cancel).unwrap_err();
    assert!(matches!(err, VfsError::Unsupported(_)), "{err:?}");
    // The move failed, so nothing may have been deleted locally.
    assert!(root.join("tree/link").exists());
    assert!(root.join("tree/real.txt").exists());

    // Neither are the things a WebDAV server has no idea about.
    let err = s
        .vfs
        .chmod(&[s.path("")], 0o700, false, &OpCtx::new())
        .unwrap_err();
    assert!(matches!(err, VfsError::Unsupported(_)), "{err:?}");
    let err = s
        .vfs
        .symlink(b"real.txt", &s.path("link"), &OpCtx::new())
        .unwrap_err();
    assert!(matches!(err, VfsError::Unsupported(_)), "{err:?}");
}

#[test]
fn deletes_files_and_whole_trees() {
    let Some(s) = server() else { return };
    s.put("tree/a.txt", "a");
    s.put("tree/deep/b.txt", "b");
    s.put("loose.txt", "loose");

    s.vfs
        .delete(&[s.path("loose.txt")], &OpCtx::new(), &Progress::new())
        .unwrap();
    assert!(!s.exists("loose.txt"));

    // One DELETE takes the collection and everything under it.
    s.vfs
        .delete(&[s.path("tree")], &OpCtx::new(), &Progress::new())
        .unwrap();
    assert!(!s.exists("tree"));
    assert!(!s.exists("tree/deep/b.txt"));

    // Deleting what is already gone is the outcome we wanted.
    s.vfs
        .delete(&[s.path("tree")], &OpCtx::new(), &Progress::new())
        .unwrap();

    // The whole server is never a thing to delete by accident.
    let err = s
        .vfs
        .delete(&[VPath::root()], &OpCtx::new(), &Progress::new())
        .unwrap_err();
    assert!(matches!(err, VfsError::Other(_)), "{err:?}");
}

#[test]
fn reports_a_quota_only_when_the_server_has_one() {
    let Some(s) = server() else { return };
    // RFC 4331 is optional, so either answer is right; what must not
    // happen is a failure that stops a pane from drawing.
    if let Some(space) = s.vfs.free_space(&s.path(""), &OpCtx::new()).unwrap() {
        assert!(space.total >= space.available);
    }
}

#[test]
fn a_bad_password_fails_at_connect_time() {
    let Some(url) = std::env::var("TL_WEBDAV_TEST_URL").ok().filter(|u| !u.is_empty()) else {
        return;
    };
    if std::env::var("TL_WEBDAV_TEST_USER").is_err() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut bookmark = Bookmark::new("dav", "webdav");
    bookmark.set("url", url);
    bookmark.set("user", std::env::var("TL_WEBDAV_TEST_USER").unwrap());
    bookmark.set("password", "definitely-not-the-password");
    let env = PluginEnv {
        prompter: Arc::new(DenyPrompter),
        askpass: None,
        runtime_dir: tmp.path().to_path_buf(),
    };
    // This is also what proves the other tests are talking to a server
    // that really checks the credentials they pass.
    let Err(err) = tl_webdav::connect(&bookmark, &env, &CancelToken::new()) else {
        panic!("a wrong password must not connect");
    };
    assert!(
        matches!(err, VfsError::Auth(_)),
        "a wrong password must be a refusal, not a missing folder: {err:?}"
    );
}
