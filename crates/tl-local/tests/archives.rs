//! Local archives as folders: browsing, extracting and adding through the
//! transfer engine, for tar, tar.gz and zip.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use tl_core::archive::{self, ArchiveFormat};
use tl_core::transfer::{self, ConflictChoice, FixedResolver, TransferMode, TransferSpec};
use tl_core::{CancelToken, OpCtx, Progress, VPath, Vfs};

fn vp(path: &Path) -> VPath {
    VPath::from_local(path)
}

/// `proj/` with `src/main.rs`, `README` and a link, plus `top.txt`.
fn make_archive(path: &Path, format: ArchiveFormat) {
    let file = fs::File::create(path).unwrap();
    match format {
        ArchiveFormat::Zip => {
            let mut zip = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
            zip.add_directory("proj/", opts).unwrap();
            zip.start_file("proj/src/main.rs", opts).unwrap();
            zip.write_all(b"fn main() {}").unwrap();
            zip.start_file("proj/README", opts).unwrap();
            zip.write_all(b"readme").unwrap();
            zip.add_symlink("proj/link", "src/main.rs", opts).unwrap();
            zip.start_file("top.txt", opts).unwrap();
            zip.write_all(b"top").unwrap();
            zip.finish().unwrap();
        }
        _ => {
            let out: Box<dyn Write> = if format == ArchiveFormat::TarGz {
                Box::new(flate2::write::GzEncoder::new(file, flate2::Compression::default()))
            } else {
                Box::new(file)
            };
            // Plain tar uses `./` names, tar.gz bare ones; both are common.
            let prefix = if format == ArchiveFormat::Tar { "./" } else { "" };
            let mut tar = tar::Builder::new(out);
            let mut dir = tar::Header::new_gnu();
            dir.set_entry_type(tar::EntryType::Directory);
            dir.set_mode(0o755);
            dir.set_size(0);
            tar.append_data(&mut dir, format!("{prefix}proj/"), std::io::empty()).unwrap();
            for (name, data) in [("proj/src/main.rs", "fn main() {}"), ("proj/README", "readme"), ("top.txt", "top")] {
                let mut h = tar::Header::new_gnu();
                h.set_mode(0o644);
                h.set_mtime(1_600_000_000);
                h.set_size(data.len() as u64);
                tar.append_data(&mut h, format!("{prefix}{name}"), data.as_bytes()).unwrap();
            }
            let mut link = tar::Header::new_gnu();
            link.set_entry_type(tar::EntryType::Symlink);
            link.set_size(0);
            tar.append_link(&mut link, format!("{prefix}proj/link"), "src/main.rs").unwrap();
            tar.into_inner().unwrap().flush().unwrap();
        }
    }
}

fn names(vfs: &dyn Vfs, dir: &VPath) -> Vec<String> {
    let mut names: Vec<String> = vfs.list(dir, &OpCtx::new()).unwrap().iter().map(|e| e.name_lossy()).collect();
    names.sort();
    names
}

fn copy(src: Arc<dyn Vfs>, src_dir: VPath, names: &[&str], dst: Arc<dyn Vfs>, dst_dir: VPath, mode: TransferMode, choice: ConflictChoice) {
    let spec = TransferSpec {
        mode,
        src,
        src_dir,
        names: names.iter().map(|n| n.as_bytes().to_vec()).collect(),
        src_elevated: false,
        dst,
        dst_dir,
        dst_elevated: false,
        target_name: None,
    };
    let report = transfer::run(&spec, &FixedResolver(choice), &CancelToken::new(), &Progress::new()).unwrap();
    assert!(!report.server_side);
}

/// What the system's own tools see in the archive.
fn system_listing(path: &Path, format: ArchiveFormat) -> String {
    let out = match format {
        ArchiveFormat::Zip => std::process::Command::new("unzip").arg("-l").arg(path).output(),
        _ => std::process::Command::new("tar").arg("-tf").arg(path).output(),
    }
    .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn roundtrip(format: ArchiveFormat, file_name: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let path = root.join(file_name);
    make_archive(&path, format);
    let local = tl_local::local_vfs();
    let ctx = OpCtx::new();

    let vfs = archive::open(local.clone(), vp(&path), &ctx).unwrap();
    let av: Arc<dyn Vfs> = vfs.clone();
    let top = vp(&path);
    let proj = top.child(b"proj");
    assert_eq!(names(&*av, &top), ["proj", "top.txt"], "{format:?}");
    assert_eq!(names(&*av, &proj), ["README", "link", "src"]);
    assert_eq!(av.read_file(&proj.child(b"README"), u64::MAX, &ctx).unwrap(), b"readme");
    assert_eq!(av.read_file(&proj.child(b"README"), 3, &ctx).unwrap(), b"rea");
    let link = av.stat(&proj.child(b"link"), &ctx).unwrap().unwrap();
    assert_eq!(link.link_target.as_deref(), Some(&b"src/main.rs"[..]));
    assert!(av.walk(&top, &ctx).unwrap().iter().any(|w| w.rel == b"proj/src/main.rs"));
    assert_eq!(av.scan(std::slice::from_ref(&proj), &ctx).unwrap().bytes, 18);
    assert!(av.list(&proj.child(b"README"), &ctx).is_err());

    // Extract.
    let out = root.join("out");
    fs::create_dir(&out).unwrap();
    copy(av.clone(), proj.clone(), &["src", "README", "link"], local.clone(), vp(&out), TransferMode::Copy, ConflictChoice::Cancel);
    assert_eq!(fs::read_to_string(out.join("src/main.rs")).unwrap(), "fn main() {}");
    assert_eq!(fs::read_to_string(out.join("README")).unwrap(), "readme");
    assert_eq!(fs::read_link(out.join("link")).unwrap(), Path::new("src/main.rs"));

    // Add a file and a folder.
    let incoming = root.join("incoming");
    fs::create_dir_all(incoming.join("newdir")).unwrap();
    fs::write(incoming.join("new.txt"), "new").unwrap();
    fs::write(incoming.join("newdir/inner.txt"), "inner").unwrap();
    fs::write(incoming.join("README"), "changed").unwrap();
    copy(local.clone(), vp(&incoming), &["new.txt", "newdir"], av.clone(), proj.clone(), TransferMode::Copy, ConflictChoice::Cancel);
    assert_eq!(names(&*av, &proj), ["README", "link", "new.txt", "newdir", "src"]);
    assert_eq!(av.read_file(&proj.child(b"newdir").child(b"inner.txt"), u64::MAX, &ctx).unwrap(), b"inner");
    assert_eq!(av.read_file(&proj.child(b"src").child(b"main.rs"), u64::MAX, &ctx).unwrap(), b"fn main() {}");

    // Replace an existing member.
    copy(local.clone(), vp(&incoming), &["README"], av.clone(), proj.clone(), TransferMode::Copy, ConflictChoice::Overwrite);
    assert_eq!(av.read_file(&proj.child(b"README"), u64::MAX, &ctx).unwrap(), b"changed");
    let listing = system_listing(&path, format);
    assert_eq!(listing.matches("proj/README").count(), 1, "{listing}");
    assert!(listing.contains("proj/newdir/inner.txt"), "{listing}");

    // Folders and files made inside, and deletions.
    av.mkdir(&proj.child(b"made"), &ctx).unwrap();
    assert!(av.mkdir(&proj.child(b"made"), &ctx).is_err());
    av.write_file(&proj.child(b"made").child(b"saved.txt"), b"saved", &ctx).unwrap();
    assert_eq!(av.read_file(&proj.child(b"made").child(b"saved.txt"), u64::MAX, &ctx).unwrap(), b"saved");
    av.delete(&[proj.child(b"src"), proj.child(b"new.txt")], &ctx, &Progress::new()).unwrap();
    assert_eq!(names(&*av, &proj), ["README", "link", "made", "newdir"]);
    assert!(av.rename(&proj.child(b"README"), &proj.child(b"x"), &ctx).is_err());

    // Moving out removes the member from the archive.
    copy(av.clone(), top.clone(), &["top.txt"], local.clone(), vp(&out), TransferMode::Move, ConflictChoice::Cancel);
    assert_eq!(fs::read_to_string(out.join("top.txt")).unwrap(), "top");
    assert_eq!(names(&*av, &top), ["proj"]);

    // Still a valid archive for the system tools, and no temp files left.
    let listing = system_listing(&path, format);
    assert!(listing.contains("proj/made/saved.txt") && !listing.contains("top.txt"), "{listing}");
    let leftovers: Vec<_> = fs::read_dir(&root).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains(".tl-")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn tar_archives() {
    roundtrip(ArchiveFormat::Tar, "site.tar");
}

#[test]
fn tar_gz_archives() {
    roundtrip(ArchiveFormat::TarGz, "site.tar.gz");
}

#[test]
fn zip_archives() {
    roundtrip(ArchiveFormat::Zip, "site.zip");
}

/// Packing starts from an empty archive that every tool accepts.
#[test]
fn empty_archives_take_files() {
    for (format, name) in [(ArchiveFormat::Tar, "new.tar"), (ArchiveFormat::TarGz, "new.tar.gz"), (ArchiveFormat::Zip, "new.zip")] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let path = root.join(name);
        fs::write(&path, format.empty_archive()).unwrap();
        if format != ArchiveFormat::Zip {
            // (`unzip -l` exits 1 for "zipfile is empty".)
            system_listing(&path, format);
        }
        let local = tl_local::local_vfs();
        let av: Arc<dyn Vfs> = archive::open(local.clone(), vp(&path), &OpCtx::new()).unwrap();
        assert!(names(&*av, &vp(&path)).is_empty());
        fs::create_dir(root.join("docs")).unwrap();
        fs::write(root.join("docs/a.txt"), "a").unwrap();
        copy(local, vp(&root), &["docs"], av.clone(), vp(&path), TransferMode::Copy, ConflictChoice::Cancel);
        assert_eq!(names(&*av, &vp(&path)), ["docs"], "{name}");
        assert!(system_listing(&path, format).contains("docs/a.txt"));
    }
}

#[test]
fn broken_archives_fail_to_open() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("bad.zip");
    fs::write(&path, "not a zip").unwrap();
    assert!(archive::open(tl_local::local_vfs(), vp(&path), &OpCtx::new()).is_err());
    let txt = tmp.path().join("notes.txt");
    fs::write(&txt, "x").unwrap();
    assert!(archive::open(tl_local::local_vfs(), vp(&txt), &OpCtx::new()).is_err());
}
