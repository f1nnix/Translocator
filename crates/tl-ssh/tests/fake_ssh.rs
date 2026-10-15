//! End-to-end tests of the SSH provider against a fake `ssh` that runs the
//! remote commands locally. Everything above the transport — command
//! building, quoting, the marker protocol, listings, server-side copies and
//! tar streaming — runs for real. Each test runs twice: with the system's
//! BSD tools and, when Homebrew's GNU tools are installed, with GNU tools.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tl_core::prompt::DenyPrompter;
use tl_core::transfer::{self, ConflictChoice, FixedResolver, TransferMode, TransferSpec};
use tl_core::{Bookmark, CancelToken, OpCtx, PluginEnv, Progress, VPath, Vfs};
use tl_ssh::Dialect;

const GNU_PATHS: &[&str] = &[
    "/opt/homebrew/opt/findutils/libexec/gnubin",
    "/opt/homebrew/opt/coreutils/libexec/gnubin",
];

/// A stand-in for OpenSSH covering the options the provider uses.
fn fake_ssh(dir: &Path, path_prefix: &str) -> PathBuf {
    let script = format!(
        r#"#!/bin/sh
PATH="{path_prefix}/usr/bin:/bin:/usr/sbin:/sbin"; export PATH
ctl=""; op=""; master=0; resolve=0
while [ $# -gt 0 ]; do
  case "$1" in
    -G) resolve=1 ;;
    -M) master=1 ;;
    -N|-T|-t) ;;
    -S) ctl=$2; shift ;;
    -O) op=$2; shift ;;
    -o|-l|-p|-i|-J) shift ;;
    -*) ;;
    *) break ;;
  esac
  shift
done
host=$1; shift
if [ $resolve = 1 ]; then printf 'user tester\nhostname %s\nport 22\nproxyjump none\n' "$host"; exit 0; fi
if [ -n "$op" ]; then
  case $op in check) [ -e "$ctl" ]; exit $? ;; exit) rm -f "$ctl"; exit 0 ;; esac
fi
if [ $master = 1 ]; then : > "$ctl"; exec sleep 100000; fi
SHELL=/bin/sh; export SHELL
exec /bin/sh -c "$*"
"#
    );
    let path = dir.join("fake-ssh");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct Setup {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    vfs: Arc<tl_ssh::SshVfs>,
    /// A real sshd rather than the fake transport.
    real: bool,
}

/// A real OpenSSH server to test against, from the environment:
/// `TL_SSH_TEST_HOST`, `TL_SSH_TEST_PORT`, `TL_SSH_TEST_KEY`,
/// `TL_SSH_TEST_EXTRA`. The server must see this machine's file system
/// (e.g. an sshd on localhost), because the tests check results on disk.
fn real_server() -> Option<Setup> {
    let host = std::env::var("TL_SSH_TEST_HOST").ok()?;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let mut bookmark = Bookmark::new("real", "ssh");
    bookmark.set("host", host);
    for (key, var) in [("port", "TL_SSH_TEST_PORT"), ("identity", "TL_SSH_TEST_KEY"), ("extra", "TL_SSH_TEST_EXTRA")] {
        if let Ok(value) = std::env::var(var) {
            bookmark.set(key, value);
        }
    }
    let env = PluginEnv {
        prompter: Arc::new(DenyPrompter),
        askpass: None,
        runtime_dir: root.clone(),
    };
    let vfs = tl_ssh::connect(&bookmark, &env, &CancelToken::new()).expect("connect to TL_SSH_TEST_HOST");
    fs::create_dir_all(root.join("data")).unwrap();
    Some(Setup { _tmp: tmp, root, vfs, real: true })
}

fn setups() -> Vec<Setup> {
    let mut variants = vec![String::new()];
    if GNU_PATHS.iter().all(|p| Path::new(p).exists()) {
        variants.push(GNU_PATHS.iter().map(|p| format!("{p}:")).collect());
    }
    let mut out: Vec<Setup> = variants
        .into_iter()
        .map(|prefix| {
            let tmp = tempfile::tempdir().unwrap();
            // Canonical path: /var → /private/var on macOS.
            let root = tmp.path().canonicalize().unwrap();
            let ssh = fake_ssh(&root, &prefix);
            let mut bookmark = Bookmark::new("fake", "ssh");
            bookmark.set("host", "fakehost");
            bookmark.set("ssh_bin", ssh.display().to_string());
            let env = PluginEnv {
                prompter: Arc::new(DenyPrompter),
                askpass: None,
                runtime_dir: root.clone(),
            };
            let vfs = tl_ssh::connect(&bookmark, &env, &CancelToken::new()).unwrap();
            fs::create_dir_all(root.join("data")).unwrap();
            Setup {
                _tmp: tmp,
                root,
                vfs,
                real: false,
            }
        })
        .collect();
    out.extend(real_server());
    out
}

fn vp(path: &Path) -> VPath {
    VPath::from_local(path)
}

const WEIRD: &str = "it's \"odd\" $HOME `x` \\ ünï.txt";

#[test]
fn connects_and_detects_dialect() {
    for s in setups() {
        let info = s.vfs.info();
        assert!(!info.user.is_empty());
        if !s.real {
            assert_eq!(s.vfs.location_key(), "ssh://tester@fakehost:22");
        }
        assert!(matches!(info.dialect, Dialect::Gnu | Dialect::StatF), "{info:?}");
        assert_eq!(s.vfs.home(), VPath::parse(&std::env::var("HOME").unwrap()));
    }
}

#[test]
fn listing_and_basic_ops() {
    for s in setups() {
        let data = s.root.join("data");
        fs::create_dir(data.join("sub")).unwrap();
        fs::write(data.join(WEIRD), "weird").unwrap();
        fs::write(data.join(".hidden"), "h").unwrap();
        std::os::unix::fs::symlink("sub", data.join("to-sub")).unwrap();
        std::os::unix::fs::symlink("missing", data.join("dangling")).unwrap();
        let ctx = OpCtx::new();
        let dir = vp(&data);

        let mut entries = s.vfs.list(&dir, &ctx).unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<String> = entries.iter().map(|e| e.name_lossy()).collect();
        assert_eq!(names, [".hidden", "dangling", WEIRD, "sub", "to-sub"], "{:?}", s.vfs.info().dialect);
        let weird = entries.iter().find(|e| e.name_lossy() == WEIRD).unwrap();
        assert_eq!(weird.size, 5);
        assert!(weird.mtime.unwrap() > 1_600_000_000);
        let link = entries.iter().find(|e| e.name == b"to-sub").unwrap();
        assert!(link.is_symlink() && link.is_dir());
        assert_eq!(link.link_target.as_deref(), Some(&b"sub"[..]));
        let dangling = entries.iter().find(|e| e.name == b"dangling").unwrap();
        assert!(dangling.is_symlink() && !dangling.is_dir());

        let st = s.vfs.stat(&dir.child(WEIRD.as_bytes()), &ctx).unwrap().unwrap();
        assert_eq!(st.name_lossy(), WEIRD);
        assert!(s.vfs.stat(&dir.child(b"nope"), &ctx).unwrap().is_none());

        let err = s.vfs.list(&dir.child(b"nope"), &ctx).unwrap_err();
        assert!(matches!(err, tl_core::VfsError::NotFound(_)), "{err:?}");

        s.vfs.mkdir(&dir.child(b"made"), &ctx).unwrap();
        assert!(data.join("made").is_dir());
        assert!(s.vfs.mkdir(&dir.child(b"made"), &ctx).is_err());
        s.vfs.create_file(&dir.child(b"new.txt"), &ctx).unwrap();
        assert!(s.vfs.create_file(&dir.child(b"new.txt"), &ctx).is_err());
        s.vfs.rename(&dir.child(b"new.txt"), &dir.child(b"renamed.txt"), &ctx).unwrap();
        let err = s.vfs.rename(&dir.child(b"renamed.txt"), &dir.child(b"made"), &ctx).unwrap_err();
        assert!(matches!(err, tl_core::VfsError::AlreadyExists(_)), "{err:?}");

        s.vfs.chmod(&[dir.child(b"renamed.txt")], 0o600, false, &ctx).unwrap();
        assert_eq!(fs::metadata(data.join("renamed.txt")).unwrap().permissions().mode() & 0o777, 0o600);

        s.vfs.delete(&[dir.child(b"made"), dir.child(b"renamed.txt")], &ctx, &Progress::new()).unwrap();
        assert!(!data.join("made").exists() && !data.join("renamed.txt").exists());

        let totals = s.vfs.scan(&[dir.clone()], &ctx).unwrap();
        assert!(totals.files >= 4 && totals.dirs >= 2, "{totals:?}");
        let walked = s.vfs.walk(&dir, &ctx).unwrap();
        assert!(walked.iter().any(|w| w.rel == b"sub"));

        assert!(s.vfs.free_space(&dir, &ctx).unwrap().unwrap().total > 0);
        let out = s.vfs.exec(&dir, "pwd; echo err >&2; exit 4", &ctx).unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), data.display().to_string());
        assert_eq!(out.status, 4);
    }
}

#[test]
fn file_roundtrip_keeps_inode() {
    for s in setups() {
        use std::os::unix::fs::MetadataExt;
        let data = s.root.join("data");
        let file = data.join(WEIRD);
        fs::write(&file, "old").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        let inode = fs::metadata(&file).unwrap().ino();
        let ctx = OpCtx::new();
        let payload: Vec<u8> = (0..=255u8).cycle().take(300_000).collect();
        s.vfs.write_file(&vp(&file), &payload, &ctx).unwrap();
        assert_eq!(fs::read(&file).unwrap(), payload);
        let meta = fs::metadata(&file).unwrap();
        assert_eq!(meta.ino(), inode, "rewritten in place");
        assert_eq!(meta.permissions().mode() & 0o777, 0o640);
        assert_eq!(s.vfs.read_file(&vp(&file), 10, &ctx).unwrap(), &payload[..10]);
        assert_eq!(s.vfs.read_file(&vp(&file), u64::MAX, &ctx).unwrap(), payload);
    }
}

fn tree(root: &Path) {
    fs::create_dir_all(root.join("proj/src/deep")).unwrap();
    fs::write(root.join("proj/src/main.rs"), "fn main() {}").unwrap();
    fs::write(root.join("proj/src/deep/x.bin"), vec![7u8; 100_000]).unwrap();
    fs::write(root.join("proj/README"), "readme").unwrap();
    std::os::unix::fs::symlink("src/main.rs", root.join("proj/link")).unwrap();
}

#[test]
fn server_side_copy_and_move() {
    for s in setups() {
        let a = s.root.join("data/a");
        let b = s.root.join("data/b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(b.join("proj/src")).unwrap();
        tree(&a);
        fs::write(b.join("proj/README"), "existing").unwrap();

        let spec = |mode, names: &[&str]| TransferSpec {
            mode,
            src: s.vfs.clone(),
            src_dir: vp(&a),
            names: names.iter().map(|n| n.as_bytes().to_vec()).collect(),
            src_elevated: false,
            dst: s.vfs.clone(),
            dst_dir: vp(&b),
            dst_elevated: false,
            target_name: None,
        };
        let progress = Progress::new();
        // Merge, keeping the existing README.
        let report = transfer::run(
            &spec(TransferMode::Copy, &["proj"]),
            &FixedResolver(ConflictChoice::SkipExisting),
            &CancelToken::new(),
            &progress,
        )
        .unwrap();
        assert!(report.server_side);
        assert_eq!(fs::read_to_string(b.join("proj/README")).unwrap(), "existing");
        assert_eq!(fs::read(b.join("proj/src/deep/x.bin")).unwrap().len(), 100_000);
        assert_eq!(fs::read_link(b.join("proj/link")).unwrap(), Path::new("src/main.rs"));

        // Overwrite merge.
        transfer::run(
            &spec(TransferMode::Copy, &["proj"]),
            &FixedResolver(ConflictChoice::Overwrite),
            &CancelToken::new(),
            &progress,
        )
        .unwrap();
        assert_eq!(fs::read_to_string(b.join("proj/README")).unwrap(), "readme");

        // Copying onto itself makes a sibling.
        let mut same = spec(TransferMode::Copy, &["proj"]);
        same.dst_dir = vp(&a);
        transfer::run(&same, &FixedResolver(ConflictChoice::Cancel), &CancelToken::new(), &progress).unwrap();
        assert!(a.join("proj (2)/src/main.rs").exists());

        // Moving into its own subfolder is refused.
        let mut inside = spec(TransferMode::Move, &["proj"]);
        inside.dst_dir = vp(&a.join("proj/src"));
        assert!(transfer::run(&inside, &FixedResolver(ConflictChoice::Cancel), &CancelToken::new(), &progress).is_err());

        // Plain move with a rename target.
        let mut mv = spec(TransferMode::Move, &["proj (2)"]);
        mv.target_name = Some(b"moved".to_vec());
        transfer::run(&mv, &FixedResolver(ConflictChoice::Cancel), &CancelToken::new(), &progress).unwrap();
        assert!(b.join("moved/src/deep/x.bin").exists());
        assert!(!a.join("proj (2)").exists());
    }
}

#[test]
fn streaming_between_local_and_ssh() {
    for s in setups() {
        let local_root = tempfile::tempdir().unwrap();
        let local = local_root.path().canonicalize().unwrap();
        tree(&local);
        let remote_dir = s.root.join("data");
        let local_vfs = tl_local::local_vfs();

        // Upload.
        let up = TransferSpec {
            mode: TransferMode::Copy,
            src: local_vfs.clone(),
            src_dir: vp(&local),
            names: vec![b"proj".to_vec()],
            src_elevated: false,
            dst: s.vfs.clone(),
            dst_dir: vp(&remote_dir),
            dst_elevated: false,
            target_name: None,
        };
        let progress = Progress::new();
        let report = transfer::run(&up, &FixedResolver(ConflictChoice::Cancel), &CancelToken::new(), &progress).unwrap();
        assert!(!report.server_side);
        assert_eq!(fs::read(remote_dir.join("proj/src/deep/x.bin")).unwrap(), vec![7u8; 100_000]);
        assert_eq!(fs::read_link(remote_dir.join("proj/link")).unwrap(), Path::new("src/main.rs"));
        let snap = progress.snapshot();
        assert_eq!(snap.bytes_done, snap.bytes_total);

        // Upload again as "keep both": renamed while streaming.
        transfer::run(&up, &FixedResolver(ConflictChoice::KeepBoth), &CancelToken::new(), &progress).unwrap();
        assert!(remote_dir.join("proj (2)/README").exists());

        // Overwrite one small file in place (keeps the remote inode).
        use std::os::unix::fs::MetadataExt;
        let inode = fs::metadata(remote_dir.join("proj/README")).unwrap().ino();
        fs::write(local.join("proj/README"), "changed locally").unwrap();
        let single = TransferSpec {
            src_dir: vp(&local.join("proj")),
            names: vec![b"README".to_vec()],
            dst_dir: vp(&remote_dir.join("proj")),
            ..up
        };
        transfer::run(&single, &FixedResolver(ConflictChoice::Overwrite), &CancelToken::new(), &progress).unwrap();
        assert_eq!(fs::read_to_string(remote_dir.join("proj/README")).unwrap(), "changed locally");
        assert_eq!(fs::metadata(remote_dir.join("proj/README")).unwrap().ino(), inode);

        // Download with a move: sources disappear from the server.
        let download_to = local.join("downloads");
        fs::create_dir(&download_to).unwrap();
        let down = TransferSpec {
            mode: TransferMode::Move,
            src: s.vfs.clone(),
            src_dir: vp(&remote_dir),
            names: vec![b"proj (2)".to_vec()],
            src_elevated: false,
            dst: local_vfs.clone(),
            dst_dir: vp(&download_to),
            dst_elevated: false,
            target_name: None,
        };
        transfer::run(&down, &FixedResolver(ConflictChoice::Cancel), &CancelToken::new(), &progress).unwrap();
        assert_eq!(fs::read(download_to.join("proj (2)/src/deep/x.bin")).unwrap().len(), 100_000);
        assert!(!remote_dir.join("proj (2)").exists());
    }
}

#[test]
fn streaming_merge_filters_and_partial_move() {
    for s in setups() {
        let local_root = tempfile::tempdir().unwrap();
        let local = local_root.path().canonicalize().unwrap();
        tree(&local);
        let remote_dir = s.root.join("data");
        fs::create_dir_all(remote_dir.join("proj")).unwrap();
        fs::write(remote_dir.join("proj/README"), "remote wins").unwrap();

        // Move local → remote, skipping existing files: README stays local.
        let spec = TransferSpec {
            mode: TransferMode::Move,
            src: tl_local::local_vfs(),
            src_dir: vp(&local),
            names: vec![b"proj".to_vec()],
            src_elevated: false,
            dst: s.vfs.clone(),
            dst_dir: vp(&remote_dir),
            dst_elevated: false,
            target_name: None,
        };
        transfer::run(&spec, &FixedResolver(ConflictChoice::SkipExisting), &CancelToken::new(), &Progress::new()).unwrap();
        assert_eq!(fs::read_to_string(remote_dir.join("proj/README")).unwrap(), "remote wins");
        assert!(remote_dir.join("proj/src/main.rs").exists());
        assert!(local.join("proj/README").exists(), "skipped file must stay");
        assert!(!local.join("proj/src").exists(), "moved folders are gone");
    }
}

#[test]
fn cancel_stops_server_side_job() {
    for s in setups() {
        let ctx = OpCtx::new();
        let cancel = ctx.cancel.clone();
        let started = std::time::Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            cancel.cancel();
        });
        let err = s.vfs.exec(&vp(&s.root), "sleep 20; echo done", &ctx).unwrap_err();
        assert!(err.is_cancelled(), "{err:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}

#[test]
fn server_side_find() {
    for s in setups() {
        tree(&s.root.join("data"));
        let mut hits = Vec::new();
        s.vfs
            .find(
                &vp(&s.root.join("data")),
                &tl_core::util::search_masks("*.RS; read"),
                &OpCtx::new(),
                &mut |path, kind| {
                    hits.push((path.file_name_lossy(), kind));
                    true
                },
            )
            .unwrap();
        hits.sort_by(|a, b| a.0.cmp(&b.0));
        let names: Vec<&str> = hits.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["README", "main.rs"], "{:?}", s.vfs.info().dialect);

        // A closure returning false stops the search.
        let mut count = 0;
        s.vfs
            .find(&vp(&s.root.join("data")), &["*".into()], &OpCtx::new(), &mut |_, _| {
                count += 1;
                false
            })
            .unwrap();
        assert_eq!(count, 1);
    }
}
