//! End-to-end tests against a real S3 service.
//!
//! `scripts/s3-test-server.sh` starts MinIO in Docker and prints the
//! environment to export. Without `TL_S3_TEST_ENDPOINT` every test returns
//! immediately, so a plain `cargo test` still works on a machine with no
//! object store. CI starts MinIO as a service container, so there the whole
//! suite runs.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use tl_core::prompt::DenyPrompter;
use tl_core::transfer::{self, ConflictChoice, FixedResolver, TransferMode, TransferSpec};
use tl_core::{
    Bookmark, CancelToken, Entry, OpCtx, PluginEnv, Progress, VPath, Vfs, VfsError, VfsResult,
};
use tl_s3::S3Vfs;

const WEIRD: &str = "it's \"odd\" + ünï [1].txt";

struct Server {
    vfs: Arc<S3Vfs>,
    bucket: String,
    _tmp: tempfile::TempDir,
}

impl Server {
    /// `/bucket/<rest>`
    fn path(&self, rest: &str) -> VPath {
        let base = VPath::root().child(self.bucket.as_bytes());
        if rest.is_empty() { base } else { base.join(rest) }
    }

    fn vfs(&self) -> Arc<dyn Vfs> {
        self.vfs.clone()
    }

    fn put(&self, rest: &str, text: &str) {
        self.vfs
            .write_file(&self.path(rest), text.as_bytes(), &OpCtx::new())
            .unwrap_or_else(|e| panic!("cannot write {rest}: {e}"));
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
}

impl Drop for Server {
    fn drop(&mut self) {
        // Leaving buckets behind would make a rerun fail.
        let _ = self.vfs.delete(
            &[VPath::root().child(self.bucket.as_bytes())],
            &OpCtx::new(),
            &Progress::new(),
        );
    }
}

/// A connection plus an empty bucket of its own, or `None` when no test
/// service is configured.
fn server() -> Option<Server> {
    let endpoint = std::env::var("TL_S3_TEST_ENDPOINT").ok().filter(|e| !e.is_empty())?;
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let bucket = format!(
        "tl-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let tmp = tempfile::tempdir().unwrap();

    let mut bookmark = Bookmark::new("minio", "s3");
    bookmark.set("endpoint", endpoint);
    bookmark.set("region", std::env::var("TL_S3_TEST_REGION").unwrap_or_else(|_| "us-east-1".into()));
    if let Ok(key) = std::env::var("TL_S3_TEST_KEY") {
        bookmark.set("access_key", key);
        bookmark.set("secret_key", std::env::var("TL_S3_TEST_SECRET").unwrap_or_default());
    }
    let env = PluginEnv {
        prompter: Arc::new(DenyPrompter),
        askpass: None,
        runtime_dir: tmp.path().to_path_buf(),
    };
    let vfs = tl_s3::connect(&bookmark, &env, &CancelToken::new())
        .expect("cannot reach the S3 test service");
    vfs.mkdir(&VPath::root().child(bucket.as_bytes()), &OpCtx::new())
        .expect("cannot create the test bucket");
    Some(Server {
        vfs,
        bucket,
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
fn buckets_objects_and_folders() {
    let Some(s) = server() else { return };

    // The root of a connection is the bucket list.
    let buckets = s.vfs.list(&VPath::root(), &OpCtx::new()).unwrap();
    assert!(
        buckets.iter().any(|b| b.name_lossy() == s.bucket),
        "the new bucket should show at the root"
    );
    assert!(buckets.iter().all(|b| b.is_dir()));

    s.put("notes.txt", "hello");
    s.put(&format!("docs/{WEIRD}"), "weird");
    s.put("docs/deep/inner.txt", "inner");
    s.vfs
        .mkdir(&s.path("empty-folder"), &OpCtx::new())
        .unwrap();

    assert_eq!(s.list(""), ["docs", "empty-folder", "notes.txt"]);
    assert_eq!(s.list("docs"), ["deep", WEIRD]);
    // A folder with nothing in it still has to be visible.
    assert_eq!(s.list("empty-folder"), Vec::<String>::new());
    assert!(s.exists("empty-folder"));

    let file = s.vfs.stat(&s.path("notes.txt"), &OpCtx::new()).unwrap().unwrap();
    assert!(!file.is_dir() && file.size == 5);
    assert!(file.mtime.unwrap() > 1_600_000_000);
    let folder = s.vfs.stat(&s.path("docs"), &OpCtx::new()).unwrap().unwrap();
    assert!(folder.is_dir());
    assert!(s.vfs.stat(&s.path("nothing"), &OpCtx::new()).unwrap().is_none());

    // walk reports the folders that only exist as parts of a key.
    let walked: Vec<String> = s
        .vfs
        .walk(&s.path(""), &OpCtx::new())
        .unwrap()
        .iter()
        .map(|w| String::from_utf8_lossy(&w.rel).into_owned())
        .collect();
    assert!(walked.contains(&"docs".to_string()), "{walked:?}");
    assert!(walked.contains(&"docs/deep".to_string()), "{walked:?}");
    assert!(walked.contains(&"docs/deep/inner.txt".to_string()), "{walked:?}");

    let totals = s.vfs.scan(&[s.path("")], &OpCtx::new()).unwrap();
    assert_eq!(totals.files, 3);
    assert_eq!(totals.bytes, 5 + 5 + 5);

    assert_eq!(s.read(&format!("docs/{WEIRD}")), "weird");
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

    // A folder is a prefix, so every key below it moves.
    s.vfs
        .rename(&s.path("tree"), &s.path("moved"), &OpCtx::new())
        .unwrap();
    assert!(!s.exists("tree"));
    assert_eq!(s.read("moved/a.txt"), "a");
    assert_eq!(s.read("moved/sub/b.txt"), "b");

    // Renaming onto something that exists must not quietly replace it.
    s.put("taken.txt", "keep");
    let err = s
        .vfs
        .rename(&s.path("renamed.txt"), &s.path("taken.txt"), &OpCtx::new())
        .unwrap_err();
    assert!(matches!(err, VfsError::AlreadyExists(_)), "{err:?}");
    assert_eq!(s.read("taken.txt"), "keep");
}

#[test]
fn copies_and_moves_inside_the_account() {
    let Some(s) = server() else { return };
    s.put("src/keep.txt", "new");
    s.put("src/only-src.txt", "only");
    s.put("src/deep/x.txt", "x");
    s.put("dst/keep.txt", "old");

    // Same connection on both sides: the engine must stay server-side.
    let copy = spec(
        TransferMode::Copy,
        s.vfs(),
        s.path(""),
        &["src"],
        s.vfs(),
        s.path("dst"),
    );
    let report = run(&copy, ConflictChoice::Cancel).unwrap();
    assert!(report.server_side, "a copy within one account must not stream");
    assert_eq!(s.read("dst/src/keep.txt"), "new");
    assert_eq!(s.read("dst/src/deep/x.txt"), "x");

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
    run(&move_it, ConflictChoice::Cancel).unwrap();
    assert_eq!(s.read("dst/only-src.txt"), "only");
    assert!(!s.exists("src/only-src.txt"));
}

fn local_tree(root: &Path) {
    fs::create_dir_all(root.join("proj/src")).unwrap();
    fs::write(root.join("proj/src/main.rs"), "fn main() {}").unwrap();
    fs::write(root.join("proj/README"), "readme").unwrap();
    fs::write(root.join("proj").join(WEIRD), "weird").unwrap();
    fs::create_dir_all(root.join("proj/empty")).unwrap();
}

#[test]
fn streams_between_local_and_s3() {
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
    assert!(!report.server_side, "local to S3 has to stream");
    assert_eq!(s.read("proj/src/main.rs"), "fn main() {}");
    assert_eq!(s.read(&format!("proj/{WEIRD}")), "weird");
    // An empty folder survives as a marker object.
    assert!(s.exists("proj/empty"));

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
    assert_eq!(fs::read_to_string(back.join("proj/src/main.rs")).unwrap(), "fn main() {}");
    assert_eq!(fs::read_to_string(back.join("proj").join(WEIRD)).unwrap(), "weird");
    assert!(back.join("proj/empty").is_dir());
}

#[test]
fn uploads_a_large_file_in_parts() {
    let Some(s) = server() else { return };
    // The test server is told to switch to multipart well below the usual
    // 64 MB, so a 12 MB file really does go up in several parts.
    if std::env::var("TL_S3_MULTIPART_THRESHOLD").is_err() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    fs::create_dir(root.join("big")).unwrap();
    let payload: Vec<u8> = (0..=255u8).cycle().take(12 * 1024 * 1024).collect();
    fs::write(root.join("big/blob.bin"), &payload).unwrap();

    let up = spec(
        TransferMode::Copy,
        tl_local::local_vfs(),
        VPath::from_local(&root),
        &["big"],
        s.vfs(),
        s.path(""),
    );
    run(&up, ConflictChoice::Cancel).unwrap();

    let stat = s.vfs.stat(&s.path("big/blob.bin"), &OpCtx::new()).unwrap().unwrap();
    assert_eq!(stat.size, payload.len() as u64);
    let got = s
        .vfs
        .read_file(&s.path("big/blob.bin"), u64::MAX, &OpCtx::new())
        .unwrap();
    assert_eq!(got, payload, "the reassembled object must match byte for byte");
}

#[test]
fn refuses_what_s3_cannot_hold() {
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
}

#[test]
fn deletes_objects_folders_and_buckets() {
    let Some(s) = server() else { return };
    s.put("tree/a.txt", "a");
    s.put("tree/deep/b.txt", "b");
    s.put("loose.txt", "loose");

    s.vfs
        .delete(&[s.path("loose.txt")], &OpCtx::new(), &Progress::new())
        .unwrap();
    assert!(!s.exists("loose.txt"));

    // Deleting a folder takes every key beneath it.
    s.vfs
        .delete(&[s.path("tree")], &OpCtx::new(), &Progress::new())
        .unwrap();
    assert!(!s.exists("tree"));
    assert!(!s.exists("tree/deep/b.txt"));
    assert_eq!(s.list(""), Vec::<String>::new());

    // And a bucket can go the same way.
    let bucket = VPath::root().child(s.bucket.as_bytes());
    s.vfs.delete(&[bucket], &OpCtx::new(), &Progress::new()).unwrap();
    let left = s.vfs.list(&VPath::root(), &OpCtx::new()).unwrap();
    assert!(left.iter().all(|b| b.name_lossy() != s.bucket));
}
