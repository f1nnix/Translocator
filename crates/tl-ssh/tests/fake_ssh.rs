//! End-to-end tests of the SSH provider against a fake `ssh` that runs the
//! remote commands locally. Everything above the transport — command
//! building, quoting, the marker protocol, listings, server-side copies and
//! tar streaming — runs for real. Each test runs with the system's BSD tools
//! and, when Homebrew's GNU tools are installed, with GNU coreutils over BSD
//! find (the `stat -c` listings BusyBox servers get) and with GNU coreutils
//! plus findutils. A fake `sudo` (password [`FAKE_SUDO_PASSWORD`]) stands in
//! for the real one; it checks the protocol but does not change the user.

use std::collections::VecDeque;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tl_core::transfer::{self, ConflictChoice, FixedResolver, TransferMode, TransferSpec};
use tl_core::{Bookmark, CancelToken, OpCtx, PluginEnv, Progress, Prompter, VPath, Vfs, VfsError};
use tl_ssh::Dialect;

const GNU_COREUTILS: &str = "/opt/homebrew/opt/coreutils/libexec/gnubin";
const GNU_FINDUTILS: &str = "/opt/homebrew/opt/findutils/libexec/gnubin";

const FAKE_SUDO_PASSWORD: &str = "letmein";

/// sudo's password handling as seen through `-n`, `-S` and stdin; runs the
/// command as the same user.
const FAKE_SUDO: &str = r#"#!/bin/sh
nopass=0 stdin=0
while [ $# -gt 0 ]; do
  case "$1" in
    -n) nopass=1 ;;
    -S) stdin=1 ;;
    -k) ;;
    -p|-u) shift ;;
    --) shift; break ;;
    -*) echo "sudo: unknown option $1" >&2; exit 1 ;;
    *) break ;;
  esac
  shift
done
if [ $nopass = 1 ]; then echo "sudo: a password is required" >&2; exit 1; fi
if [ $stdin = 0 ]; then echo "sudo: a terminal is required to read the password" >&2; exit 1; fi
tries=0
while [ $tries -lt 3 ]; do
  IFS= read -r pw || { echo "sudo: no password was provided" >&2; break; }
  if [ "$pw" = "$TL_FAKE_SUDO_PASSWORD" ]; then exec "$@"; fi
  tries=$((tries + 1))
  echo "Sorry, try again." >&2
done
echo "sudo: $tries incorrect password attempts" >&2
exit 1
"#;

/// A stand-in for OpenSSH covering the options the provider uses.
fn fake_ssh(dir: &Path, path_prefix: &str) -> PathBuf {
    let bin = dir.join("fakebin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("sudo"), FAKE_SUDO).unwrap();
    fs::set_permissions(bin.join("sudo"), fs::Permissions::from_mode(0o755)).unwrap();
    let bin = bin.display();
    let script = format!(
        r#"#!/bin/sh
PATH="{bin}:{path_prefix}/usr/bin:/bin:/usr/sbin:/sbin"; export PATH
TL_FAKE_SUDO_PASSWORD={FAKE_SUDO_PASSWORD}; export TL_FAKE_SUDO_PASSWORD
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

/// Answers password prompts from a queue and records them; with nothing
/// queued it cancels, like a user closing the dialog.
#[derive(Default)]
struct Answers {
    queue: Mutex<VecDeque<String>>,
    asked: Mutex<Vec<String>>,
}

impl Answers {
    fn queue(&self, answers: &[&str]) {
        self.queue.lock().unwrap().extend(answers.iter().map(|a| a.to_string()));
    }

    /// The prompts shown since the last call.
    fn asked(&self) -> Vec<String> {
        std::mem::take(&mut *self.asked.lock().unwrap())
    }
}

impl Prompter for Answers {
    fn secret(&self, _title: &str, message: &str) -> Option<String> {
        self.asked.lock().unwrap().push(message.to_string());
        self.queue.lock().unwrap().pop_front()
    }

    fn confirm(&self, _title: &str, _message: &str) -> bool {
        false
    }
}

struct Setup {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    vfs: Arc<tl_ssh::SshVfs>,
    prompter: Arc<Answers>,
    /// The password sudo asks for, when known.
    sudo_password: Option<String>,
    /// What the probe must detect, when known.
    dialect: Option<Dialect>,
    /// A real sshd rather than the fake transport.
    real: bool,
}

fn env_with(root: &Path) -> (PluginEnv, Arc<Answers>) {
    let prompter = Arc::new(Answers::default());
    let env = PluginEnv {
        prompter: prompter.clone(),
        askpass: None,
        runtime_dir: root.to_path_buf(),
    };
    (env, prompter)
}

/// A real OpenSSH server to test against, from the environment:
/// `TL_SSH_TEST_HOST`, `TL_SSH_TEST_PORT`, `TL_SSH_TEST_KEY`,
/// `TL_SSH_TEST_EXTRA`, plus `TL_SSH_TEST_SUDO_PASSWORD` (the user's sudo
/// asks for this password) and `TL_SSH_TEST_DIALECT` (`gnu`, `statc` or
/// `statf`: what the probe must detect). The server must see this machine's
/// file system at the same paths (an sshd on localhost, or a container with
/// `$TMPDIR` mounted at the same path), because the tests check results on
/// disk. `scripts/ssh-test-servers.sh` sets up Linux containers like that.
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
    let dialect = std::env::var("TL_SSH_TEST_DIALECT").ok().map(|d| match d.as_str() {
        "gnu" => Dialect::Gnu,
        "statc" => Dialect::StatC,
        "statf" => Dialect::StatF,
        _ => panic!("TL_SSH_TEST_DIALECT must be gnu, statc or statf"),
    });
    let (env, prompter) = env_with(&root);
    let vfs = tl_ssh::connect(&bookmark, &env, &CancelToken::new()).expect("connect to TL_SSH_TEST_HOST");
    fs::create_dir_all(root.join("data")).unwrap();
    Some(Setup {
        _tmp: tmp,
        root,
        vfs,
        prompter,
        sudo_password: std::env::var("TL_SSH_TEST_SUDO_PASSWORD").ok(),
        dialect,
        real: true,
    })
}

fn setups() -> Vec<Setup> {
    // No PATH prefix means the system's own tools, which differ by host:
    // BSD on macOS, GNU on Linux. The Homebrew variants below exist only on
    // macOS, so a Linux run covers the GNU dialect and leaves the other two
    // to the unit tests in listing.rs and to scripts/ssh-test-servers.sh.
    let system = if cfg!(target_os = "macos") {
        Dialect::StatF
    } else {
        Dialect::Gnu
    };
    let mut variants = vec![(String::new(), system)];
    if Path::new(GNU_COREUTILS).exists() {
        variants.push((format!("{GNU_COREUTILS}:"), Dialect::StatC));
        if Path::new(GNU_FINDUTILS).exists() {
            variants.push((format!("{GNU_FINDUTILS}:{GNU_COREUTILS}:"), Dialect::Gnu));
        }
    }
    let mut out: Vec<Setup> = variants
        .into_iter()
        .map(|(prefix, dialect)| {
            let tmp = tempfile::tempdir().unwrap();
            // Canonical path: /var → /private/var on macOS.
            let root = tmp.path().canonicalize().unwrap();
            let ssh = fake_ssh(&root, &prefix);
            let mut bookmark = Bookmark::new("fake", "ssh");
            bookmark.set("host", "fakehost");
            bookmark.set("ssh_bin", ssh.display().to_string());
            let (env, prompter) = env_with(&root);
            let vfs = tl_ssh::connect(&bookmark, &env, &CancelToken::new()).unwrap();
            fs::create_dir_all(root.join("data")).unwrap();
            Setup {
                _tmp: tmp,
                root,
                vfs,
                prompter,
                sudo_password: Some(FAKE_SUDO_PASSWORD.to_string()),
                dialect: Some(dialect),
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
        if let Some(dialect) = s.dialect {
            assert_eq!(info.dialect, dialect, "{info:?}");
        }
        if s.real {
            assert!(!s.vfs.home().is_root(), "{info:?}");
        } else {
            assert_eq!(s.vfs.location_key(), "ssh://tester@fakehost:22");
            assert_eq!(s.vfs.home(), VPath::parse(&std::env::var("HOME").unwrap()));
        }
    }
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

#[test]
fn listing_and_basic_ops() {
    for s in setups() {
        let data = s.root.join("data");
        fs::create_dir(data.join("sub")).unwrap();
        fs::write(data.join(WEIRD), "weird").unwrap();
        fs::write(data.join(".hidden"), "h").unwrap();
        fs::write(data.join("-"), "dash").unwrap();
        std::os::unix::fs::symlink("sub", data.join("to-sub")).unwrap();
        std::os::unix::fs::symlink("missing", data.join("dangling")).unwrap();
        let ctx = OpCtx::new();
        let dir = vp(&data);

        let mut entries = s.vfs.list(&dir, &ctx).unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<String> = entries.iter().map(|e| e.name_lossy()).collect();
        assert_eq!(names, ["-", ".hidden", "dangling", WEIRD, "sub", "to-sub"], "{:?}", s.vfs.info().dialect);
        let weird = entries.iter().find(|e| e.name_lossy() == WEIRD).unwrap();
        assert_eq!(weird.size, 5);
        assert!(weird.mtime.unwrap() > 1_600_000_000);
        let dash = entries.iter().find(|e| e.name == b"-").unwrap();
        assert!(dash.kind == tl_core::EntryKind::File && dash.size == 4, "{dash:?}");
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
        assert_eq!(mode(&data.join("renamed.txt")), 0o600);

        s.vfs.delete(&[dir.child(b"made"), dir.child(b"renamed.txt")], &ctx, &Progress::new()).unwrap();
        assert!(!data.join("made").exists() && !data.join("renamed.txt").exists());

        s.vfs.symlink(b"sub", &dir.child(b"new-link"), &ctx).unwrap();
        assert_eq!(fs::read_link(data.join("new-link")).unwrap(), Path::new("sub"));
        // Recursive: files get the mode, folders also get x wherever r is
        // set, and symlinks are not followed out of the tree.
        fs::create_dir(data.join("sub/inner")).unwrap();
        fs::write(data.join("sub/inner/f.txt"), "f").unwrap();
        fs::write(data.join("outside.txt"), "o").unwrap();
        fs::set_permissions(data.join("outside.txt"), fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink("../../outside.txt", data.join("sub/inner/out")).unwrap();
        s.vfs.chmod(&[dir.child(b"sub")], 0o640, true, &ctx).unwrap();
        assert_eq!(mode(&data.join("sub/inner/f.txt")), 0o640);
        assert_eq!(mode(&data.join("sub/inner")), 0o750);
        assert_eq!(mode(&data.join("sub")), 0o750);
        assert_eq!(mode(&data.join("outside.txt")), 0o644);

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
        // Modes and times must survive whatever the server's umask is.
        let bin = local.join("proj/src/deep/x.bin");
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o664)).unwrap();
        fs::set_permissions(local.join("proj/src/deep"), fs::Permissions::from_mode(0o777)).unwrap();
        let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        fs::File::options().write(true).open(&bin).unwrap().set_modified(mtime).unwrap();
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
        let remote_bin = remote_dir.join("proj/src/deep/x.bin");
        assert_eq!(fs::read(&remote_bin).unwrap(), vec![7u8; 100_000]);
        assert_eq!(mode(&remote_bin), 0o664);
        assert_eq!(mode(&remote_dir.join("proj/src/deep")), 0o777);
        assert_eq!(fs::metadata(&remote_bin).unwrap().modified().unwrap(), mtime);
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

        // And back, keeping the local README: the server removes exactly
        // what was transferred.
        let back = TransferSpec {
            mode: TransferMode::Move,
            src: s.vfs.clone(),
            src_dir: vp(&remote_dir),
            names: vec![b"proj".to_vec()],
            src_elevated: false,
            dst: tl_local::local_vfs(),
            dst_dir: vp(&local),
            dst_elevated: false,
            target_name: None,
        };
        transfer::run(&back, &FixedResolver(ConflictChoice::SkipExisting), &CancelToken::new(), &Progress::new()).unwrap();
        assert_eq!(fs::read_to_string(local.join("proj/README")).unwrap(), "readme");
        assert_eq!(fs::read(local.join("proj/src/deep/x.bin")).unwrap(), vec![7u8; 100_000]);
        assert_eq!(fs::read_link(local.join("proj/link")).unwrap(), Path::new("src/main.rs"));
        assert!(remote_dir.join("proj/README").exists(), "skipped file must stay");
        assert!(!remote_dir.join("proj/src").exists());
        assert!(fs::symlink_metadata(remote_dir.join("proj/link")).is_err());
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

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

/// sudo that wants a password: the `sudo -n` probe, the password dialog
/// (wrong answers, cancel, retry) and elevated operations, which send the
/// password through stdin ahead of their own data. On a real server they
/// work in a folder only root can enter.
#[test]
fn sudo_with_password() {
    for s in setups() {
        let Some(password) = s.sudo_password.clone() else {
            continue;
        };
        let ctx = OpCtx::new();
        let root = OpCtx::new().elevated(true);
        assert!(s.vfs.capabilities().elevation);

        // Three wrong passwords are refused; a closed dialog cancels.
        s.prompter.queue(&["wrong-1", "wrong-2", "wrong-3"]);
        let err = s.vfs.prepare_elevation(&ctx).unwrap_err();
        assert!(matches!(err, VfsError::Auth(_)), "{err:?}");
        assert_eq!(s.prompter.asked().len(), 3);
        let err = s.vfs.prepare_elevation(&ctx).unwrap_err();
        assert!(err.is_cancelled(), "{err:?}");
        assert_eq!(s.prompter.asked().len(), 1);

        // A typo, then the right password.
        s.prompter.queue(&["wrong", &password]);
        s.vfs.prepare_elevation(&ctx).unwrap();
        let asked = s.prompter.asked();
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert!(asked[1].starts_with("Sorry, try again."), "{asked:?}");

        let base = if s.real { VPath::parse("/tmp") } else { vp(&s.root.join("data")) };
        let out = s.vfs.exec(&base, "id -u; mktemp -d \"$PWD/tl-sudo.XXXXXX\"", &root).unwrap();
        assert_eq!(out.status, 0, "{}", text(&out.stderr));
        let out = text(&out.stdout);
        let (uid, dir) = out.split_once('\n').unwrap();
        let dir = VPath::parse(dir);
        if s.real {
            // Without sudo every way in is "Permission denied", which is what
            // makes the app offer a retry with sudo.
            assert_eq!(uid, "0");
            let errors = [
                s.vfs.list(&dir, &ctx).unwrap_err(),
                s.vfs.mkdir(&dir.child(b"x"), &ctx).unwrap_err(),
                s.vfs.write_file(&dir.child(b"x"), b"data", &ctx).unwrap_err(),
                s.vfs.import_tar(&dir, &ctx).err().unwrap(),
                s.vfs.delete(std::slice::from_ref(&dir), &ctx, &Progress::new()).unwrap_err(),
            ];
            for err in errors {
                assert!(err.is_permission_denied(), "{err:?}");
            }
        }

        // Cancelling an elevated job stops it on the server.
        let job = OpCtx::new().elevated(true);
        let cancel = job.cancel.clone();
        let started = std::time::Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            cancel.cancel();
        });
        let err = s.vfs.exec(&base, "sleep 31; echo done", &job).unwrap_err();
        assert!(err.is_cancelled(), "{err:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        if s.real {
            let ps = "for f in /proc/[0-9]*/cmdline; do tr '\\0' ' ' < \"$f\"; echo; done 2>/dev/null | grep -c '^sleep 31'";
            let out = s.vfs.exec(&base, ps, &ctx).unwrap();
            assert_eq!(text(&out.stdout), "0", "sleep is still running");
        }

        // The password must not end up in the file.
        let payload: Vec<u8> = b"first line\n".iter().copied().chain((0..=255u8).cycle().take(200_000)).collect();
        let file = dir.child(b"secret.txt");
        s.vfs.write_file(&file, &payload, &root).unwrap();
        assert_eq!(s.vfs.read_file(&file, u64::MAX, &root).unwrap(), payload);
        let entries = s.vfs.list(&dir, &root).unwrap();
        let entry = entries.iter().find(|e| e.name == b"secret.txt").expect("listed");
        assert_eq!(entry.size, payload.len() as u64);
        if s.real {
            assert_eq!(entry.user.as_deref(), Some("root"));
        }

        // Upload into the folder (import_tar)...
        let local_root = tempfile::tempdir().unwrap();
        let local = local_root.path().canonicalize().unwrap();
        tree(&local);
        let up = TransferSpec {
            mode: TransferMode::Copy,
            src: tl_local::local_vfs(),
            src_dir: vp(&local),
            names: vec![b"proj".to_vec()],
            src_elevated: false,
            dst: s.vfs.clone(),
            dst_dir: dir.clone(),
            dst_elevated: true,
            target_name: None,
        };
        transfer::run(&up, &FixedResolver(ConflictChoice::Cancel), &CancelToken::new(), &Progress::new()).unwrap();
        let bin = s.vfs.read_file(&dir.join("proj/src/deep/x.bin"), u64::MAX, &root).unwrap();
        assert_eq!(bin, vec![7u8; 100_000]);
        let link = s.vfs.stat(&dir.join("proj/link"), &root).unwrap().unwrap();
        assert_eq!(link.link_target.as_deref(), Some(&b"src/main.rs"[..]));

        // ...and download everything back (export_tar).
        let down = local.join("down");
        fs::create_dir(&down).unwrap();
        let spec = TransferSpec {
            mode: TransferMode::Copy,
            src: s.vfs.clone(),
            src_dir: dir.clone(),
            names: vec![b"proj".to_vec(), b"secret.txt".to_vec()],
            src_elevated: true,
            dst: tl_local::local_vfs(),
            dst_dir: vp(&down),
            dst_elevated: false,
            target_name: None,
        };
        transfer::run(&spec, &FixedResolver(ConflictChoice::Cancel), &CancelToken::new(), &Progress::new()).unwrap();
        assert_eq!(fs::read(down.join("secret.txt")).unwrap(), payload);
        assert_eq!(fs::read(down.join("proj/src/deep/x.bin")).unwrap(), vec![7u8; 100_000]);
        assert_eq!(fs::read_link(down.join("proj/link")).unwrap(), Path::new("src/main.rs"));

        // An elevated copy into a pane that is not elevated hands the copy
        // to the login user.
        let out = s.vfs.exec(&base, "mktemp -d \"$PWD/tl-user.XXXXXX\"", &ctx).unwrap();
        let mine = VPath::parse(&text(&out.stdout));
        let copy = TransferSpec {
            mode: TransferMode::Copy,
            src: s.vfs.clone(),
            src_dir: dir.clone(),
            names: vec![b"secret.txt".to_vec()],
            src_elevated: true,
            dst: s.vfs.clone(),
            dst_dir: mine.clone(),
            dst_elevated: false,
            target_name: None,
        };
        let report = transfer::run(&copy, &FixedResolver(ConflictChoice::Cancel), &CancelToken::new(), &Progress::new()).unwrap();
        assert!(report.server_side);
        let copied = s.vfs.stat(&mine.child(b"secret.txt"), &ctx).unwrap().unwrap();
        assert_eq!(copied.uid, Some(s.vfs.info().uid));
        assert_eq!(s.vfs.read_file(&mine.child(b"secret.txt"), u64::MAX, &ctx).unwrap(), payload);

        s.vfs.chown(std::slice::from_ref(&file), Some(&s.vfs.info().user), None, false, &root).unwrap();
        assert_eq!(s.vfs.stat(&file, &root).unwrap().unwrap().uid, Some(s.vfs.info().uid));

        assert!(s.prompter.asked().is_empty(), "the password is asked for once");
        s.vfs.delete(&[dir.clone(), mine], &root, &Progress::new()).unwrap();
        assert!(s.vfs.stat(&dir, &root).unwrap().is_none());
    }
}
