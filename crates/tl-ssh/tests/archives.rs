//! Archives on a server through the SSH provider: listings from the
//! server's `tar` / `unzip`, extracting and adding through the transfer
//! engine, changes repacked on the server.
//!
//! Runs against a fake `ssh` that executes the commands locally. Everything
//! is checked through the provider, so it also runs against any real
//! server — it need not share this machine's files:
//! `TL_SSH_ARCHIVE_TEST_HOST` (with `_PORT`, `_KEY`, `_EXTRA`), or the
//! `TL_SSH_TEST_*` server of the other end-to-end tests.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use tl_core::archive::{self, ArchiveFormat};
use tl_core::prompt::DenyPrompter;
use tl_core::transfer::{self, ConflictChoice, FixedResolver, TransferMode, TransferSpec};
use tl_core::{Bookmark, CancelToken, OpCtx, PluginEnv, Progress, VPath, Vfs};

/// A stand-in for OpenSSH that runs commands locally.
const FAKE_SSH: &str = r#"#!/bin/sh
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
"#;

struct Server {
    _tmp: tempfile::TempDir,
    vfs: Arc<tl_ssh::SshVfs>,
}

fn connect(bookmark: &Bookmark, runtime: &Path) -> Arc<tl_ssh::SshVfs> {
    let env = PluginEnv {
        prompter: Arc::new(DenyPrompter),
        askpass: None,
        runtime_dir: runtime.to_path_buf(),
    };
    tl_ssh::connect(bookmark, &env, &CancelToken::new()).expect("connect")
}

fn servers() -> Vec<Server> {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let ssh = root.join("fake-ssh");
    fs::write(&ssh, FAKE_SSH).unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    let mut bookmark = Bookmark::new("fake", "ssh");
    bookmark.set("host", "fakehost");
    bookmark.set("ssh_bin", ssh.display().to_string());
    let vfs = connect(&bookmark, &root);
    let mut out = vec![Server { _tmp: tmp, vfs }];

    let prefix = ["TL_SSH_ARCHIVE_TEST_", "TL_SSH_TEST_"]
        .into_iter()
        .find(|p| std::env::var(format!("{p}HOST")).is_ok());
    if let Some(prefix) = prefix {
        let tmp = tempfile::tempdir().unwrap();
        let mut bookmark = Bookmark::new("real", "ssh");
        for (key, var) in [("host", "HOST"), ("port", "PORT"), ("identity", "KEY"), ("extra", "EXTRA")] {
            if let Ok(value) = std::env::var(format!("{prefix}{var}")) {
                bookmark.set(key, value);
            }
        }
        let vfs = connect(&bookmark, &tmp.path().canonicalize().unwrap());
        out.push(Server { _tmp: tmp, vfs });
    }
    out
}

fn sh(vfs: &tl_ssh::SshVfs, cwd: &VPath, command: &str) -> String {
    let out = vfs.exec(cwd, command, &OpCtx::new()).unwrap();
    assert_eq!(out.status, 0, "{command}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn names(vfs: &dyn Vfs, dir: &VPath) -> Vec<String> {
    let mut names: Vec<String> = vfs.list(dir, &OpCtx::new()).unwrap().iter().map(|e| e.name_lossy()).collect();
    names.sort();
    names
}

fn copy(src: Arc<dyn Vfs>, src_dir: &VPath, names: &[&str], dst: Arc<dyn Vfs>, dst_dir: &VPath, choice: ConflictChoice) {
    let spec = TransferSpec {
        mode: TransferMode::Copy,
        src,
        src_dir: src_dir.clone(),
        names: names.iter().map(|n| n.as_bytes().to_vec()).collect(),
        src_elevated: false,
        dst,
        dst_dir: dst_dir.clone(),
        dst_elevated: false,
        target_name: None,
    };
    transfer::run(&spec, &FixedResolver(choice), &CancelToken::new(), &Progress::new()).unwrap();
}

const STAR: &str = "st*ar.txt";
/// Non-ASCII without decomposable letters (macOS tools may hand back NFD).
const UNI: &str = "€_日本.txt";

fn check_archive(server: &Server, base: &VPath, name: &str) {
    let ssh: Arc<dyn Vfs> = server.vfs.clone();
    let ctx = OpCtx::new();
    let file = base.child(name.as_bytes());
    let av: Arc<dyn Vfs> = archive::open(ssh.clone(), file.clone(), &ctx).unwrap();
    let proj = file.child(b"proj");
    assert_eq!(names(&*av, &file), ["proj"], "{name}");
    assert_eq!(names(&*av, &proj), ["README", "link", "src", STAR, UNI], "{name}");
    let read = |vfs: &Arc<dyn Vfs>, path: VPath| String::from_utf8(vfs.read_file(&path, u64::MAX, &ctx).unwrap()).unwrap();
    assert_eq!(read(&av, proj.child(b"README")), "readme");
    assert_eq!(read(&av, proj.child(STAR.as_bytes())), "star");
    assert_eq!(read(&av, proj.child(UNI.as_bytes())), "uni");
    assert_eq!(av.read_file(&proj.child(b"README"), 2, &ctx).unwrap(), b"re");
    let readme = av.stat(&proj.child(b"README"), &ctx).unwrap().unwrap();
    assert_eq!(readme.size, 6);
    assert!(readme.mtime.unwrap() > 1_600_000_000, "{name}: {readme:?}");

    // Extract to another folder on the server.
    let out = base.child(format!("out-{name}").as_bytes());
    ssh.mkdir(&out, &ctx).unwrap();
    copy(av.clone(), &proj, &["src", STAR, UNI, "link"], ssh.clone(), &out, ConflictChoice::Cancel);
    assert_eq!(read(&ssh, out.child(b"src").child(b"main.rs")), "fn main() {}");
    assert_eq!(read(&ssh, out.child(STAR.as_bytes())), "star");
    assert_eq!(read(&ssh, out.child(UNI.as_bytes())), "uni");
    let link = ssh.stat(&out.child(b"link"), &ctx).unwrap().unwrap();
    assert_eq!(link.link_target.as_deref(), Some(&b"src/main.rs"[..]), "{name}");

    // Extract to this Mac.
    let local_tmp = tempfile::tempdir().unwrap();
    let local_dir = local_tmp.path().canonicalize().unwrap();
    copy(av.clone(), &proj, &["README"], tl_local::local_vfs(), &VPath::from_local(&local_dir), ConflictChoice::Cancel);
    assert_eq!(fs::read_to_string(local_dir.join("README")).unwrap(), "readme");

    // Add from this Mac, including awkward names.
    let incoming = local_dir.join("incoming");
    fs::create_dir_all(incoming.join("newdir")).unwrap();
    fs::write(incoming.join("new.txt"), "new").unwrap();
    fs::write(incoming.join("newdir/inner.txt"), "inner").unwrap();
    fs::write(incoming.join("we ird [1].txt"), "weird").unwrap();
    fs::write(incoming.join("README"), "changed").unwrap();
    let local: Arc<dyn Vfs> = tl_local::local_vfs();
    let incoming = VPath::from_local(&incoming);
    copy(local.clone(), &incoming, &["new.txt", "newdir", "we ird [1].txt"], av.clone(), &proj, ConflictChoice::Cancel);
    assert_eq!(
        names(&*av, &proj),
        ["README", "link", "new.txt", "newdir", "src", STAR, "we ird [1].txt", UNI],
        "{name}"
    );
    assert_eq!(read(&av, proj.child(b"newdir").child(b"inner.txt")), "inner");
    assert_eq!(read(&av, proj.child(b"we ird [1].txt")), "weird");
    copy(local, &incoming, &["README"], av.clone(), &proj, ConflictChoice::Overwrite);
    assert_eq!(read(&av, proj.child(b"README")), "changed");

    // Folders, saved files and deletions inside.
    av.mkdir(&proj.child(b"made"), &ctx).unwrap();
    av.write_file(&proj.child(b"made").child(b"saved.txt"), b"saved", &ctx).unwrap();
    assert_eq!(read(&av, proj.child(b"made").child(b"saved.txt")), "saved");
    av.delete(&[proj.child(b"src"), proj.child(STAR.as_bytes()), proj.child(b"we ird [1].txt")], &ctx, &Progress::new())
        .unwrap();
    assert_eq!(names(&*av, &proj), ["README", "link", "made", "new.txt", "newdir", UNI], "{name}");

    // The server's own tools still read it, and nothing is left behind.
    let listing = if name.ends_with(".zip") {
        sh(&server.vfs, base, &format!("unzip -l {}", tl_core::shell::quote_str(name)))
    } else {
        sh(&server.vfs, base, &format!("tar -tf {}", tl_core::shell::quote_str(name)))
    };
    assert!(listing.contains("proj/made/saved.txt") && !listing.contains("main.rs"), "{name}: {listing}");
    let leftovers = sh(&server.vfs, base, "ls -a | grep -c 'tl-' || true");
    assert_eq!(leftovers, "0", "{name}");
}

/// A server with `unzip` but no `zip`: zips can be browsed and extracted,
/// changing them is refused with a clear reason.
fn check_zip_without_zip(server: &Server, base: &VPath) {
    use std::io::Write as _;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
    zip.start_file(format!("proj/{UNI}"), opts).unwrap();
    zip.write_all(b"uni").unwrap();
    zip.start_file("proj/we ird [1].txt", opts).unwrap();
    zip.write_all(b"weird").unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    let ssh: Arc<dyn Vfs> = server.vfs.clone();
    let ctx = OpCtx::new();
    let file = base.child(b"only-unzip.zip");
    ssh.write_file(&file, &bytes, &ctx).unwrap();
    let av: Arc<dyn Vfs> = archive::open(ssh.clone(), file.clone(), &ctx).unwrap();
    let proj = file.child(b"proj");
    assert_eq!(names(&*av, &proj), ["we ird [1].txt", UNI]);
    assert_eq!(av.read_file(&proj.child(UNI.as_bytes()), u64::MAX, &ctx).unwrap(), b"uni");
    let out = base.child(b"out-only-unzip");
    ssh.mkdir(&out, &ctx).unwrap();
    copy(av.clone(), &proj, &["we ird [1].txt"], ssh.clone(), &out, ConflictChoice::Cancel);
    assert_eq!(ssh.read_file(&out.child(b"we ird [1].txt"), u64::MAX, &ctx).unwrap(), b"weird");
    let err = av.mkdir(&proj.child(b"new"), &ctx).unwrap_err();
    assert!(matches!(err, tl_core::VfsError::Unsupported(ref m) if m.contains("zip")), "{err:?}");
}

/// Packing into new archives: an empty archive is written, then filled.
fn check_packing(server: &Server, base: &VPath, zip: bool) {
    let ssh: Arc<dyn Vfs> = server.vfs.clone();
    let ctx = OpCtx::new();
    let mut formats = vec![(ArchiveFormat::Tar, "new.tar"), (ArchiveFormat::TarGz, "new.tar.gz")];
    if zip {
        formats.push((ArchiveFormat::Zip, "new.zip"));
    }
    for (format, name) in formats {
        let file = base.child(name.as_bytes());
        ssh.write_file(&file, &format.empty_archive(), &ctx).unwrap();
        let av: Arc<dyn Vfs> = archive::open(ssh.clone(), file.clone(), &ctx).unwrap();
        assert!(names(&*av, &file).is_empty(), "{name}");
        copy(ssh.clone(), &base.child(b"data"), &["proj"], av.clone(), &file, ConflictChoice::Cancel);
        assert_eq!(names(&*av, &file.child(b"proj")), ["README", "link", "src", STAR, UNI], "{name}");
    }
}

#[test]
fn archives_on_the_server() {
    for server in servers() {
        let vfs = &server.vfs;
        let base = VPath::parse(&sh(vfs, &VPath::root(), "mktemp -d \"${TMPDIR:-/tmp}/tlarc.XXXXXX\""));
        sh(
            vfs,
            &base,
            &format!(
                "mkdir -p data/proj/src && printf 'fn main() {{}}' > data/proj/src/main.rs && printf readme > data/proj/README \
                 && printf star > 'data/proj/{STAR}' && printf uni > 'data/proj/{UNI}' && ln -s src/main.rs data/proj/link \
                 && cd data && tar -cf ../a.tar ./proj && tar -czf ../b.tar.gz proj"
            ),
        );
        let mut archives = vec!["a.tar", "b.tar.gz"];
        let tools = sh(vfs, &base, "for c in zip unzip; do command -v $c >/dev/null && printf '%s ' $c; done; true");
        match tools.as_str() {
            "zip unzip" => {
                sh(vfs, &base, "cd data && zip -qry ../c.zip proj");
                archives.push("c.zip");
            }
            "unzip" => check_zip_without_zip(&server, &base),
            _ => {}
        }
        for name in archives {
            check_archive(&server, &base, name);
        }
        check_packing(&server, &base, tools == "zip unzip");
        sh(vfs, &VPath::root(), &format!("rm -rf {}", tl_core::shell::quote_str(&base.display())));
    }
}

#[test]
fn broken_archives_are_reported() {
    for server in servers() {
        let vfs = &server.vfs;
        let base = VPath::parse(&sh(vfs, &VPath::root(), "mktemp -d \"${TMPDIR:-/tmp}/tlarc.XXXXXX\""));
        sh(vfs, &base, "echo garbage > bad.tar.gz && echo garbage > bad.zip");
        for name in ["bad.tar.gz", "bad.zip", "missing.tar"] {
            assert!(archive::open(vfs.clone(), base.child(name.as_bytes()), &OpCtx::new()).is_err(), "{name}");
        }
        sh(vfs, &VPath::root(), &format!("rm -rf {}", tl_core::shell::quote_str(&base.display())));
    }
}
