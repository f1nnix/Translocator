//! Recursive copy / merge / move / delete with progress and cancellation.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use tl_core::{CancelToken, EntryKind, MergePolicy, Progress, ScanTotals, VfsError, VfsResult, WalkEntry};

const CHUNK: usize = 1 << 20;

pub struct Ctx<'a> {
    pub cancel: &'a CancelToken,
    pub progress: &'a Progress,
}

fn err(e: io::Error, path: &Path) -> VfsError {
    VfsError::io(e, path.display())
}

pub fn read_dir_names(dir: &Path) -> VfsResult<Vec<OsString>> {
    let mut names = fs::read_dir(dir)
        .map_err(|e| err(e, dir))?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<io::Result<Vec<_>>>()
        .map_err(|e| err(e, dir))?;
    names.sort();
    Ok(names)
}

fn set_times(path: &Path, meta: &fs::Metadata) {
    let mtime = filetime::FileTime::from_last_modification_time(meta);
    let atime = filetime::FileTime::from_last_access_time(meta);
    if let Err(e) = filetime::set_file_times(path, atime, mtime) {
        log::debug!("cannot set times on {}: {e}", path.display());
    }
}

/// Copies `src` to the not-yet-existing `dst`, preserving mode and times.
/// Symlinks are copied as symlinks.
pub fn copy_tree(src: &Path, dst: &Path, ctx: &Ctx) -> VfsResult<()> {
    ctx.cancel.check()?;
    let meta = fs::symlink_metadata(src).map_err(|e| err(e, src))?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        let target = fs::read_link(src).map_err(|e| err(e, src))?;
        std::os::unix::fs::symlink(&target, dst).map_err(|e| err(e, dst))?;
        let mtime = filetime::FileTime::from_last_modification_time(&meta);
        let _ = filetime::set_symlink_file_times(dst, mtime, mtime);
        ctx.progress.add_items(1);
    } else if ft.is_dir() {
        match fs::create_dir(dst) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dst.is_dir() => {}
            Err(e) => return Err(err(e, dst)),
        }
        for name in read_dir_names(src)? {
            copy_tree(&src.join(&name), &dst.join(&name), ctx)?;
        }
        // Applied after the children so read-only folders can be filled.
        fs::set_permissions(dst, fs::Permissions::from_mode(meta.mode() & 0o7777))
            .map_err(|e| err(e, dst))?;
        set_times(dst, &meta);
    } else if ft.is_file() {
        ctx.progress.set_current(src.display().to_string());
        copy_file(src, dst, &meta, ctx)?;
        ctx.progress.add_items(1);
    } else {
        log::warn!("skipping special file {}", src.display());
    }
    Ok(())
}

fn copy_file(src: &Path, dst: &Path, meta: &fs::Metadata, ctx: &Ctx) -> VfsResult<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        // APFS clones are instant and keep metadata; other volumes fall back.
        let s = std::ffi::CString::new(src.as_os_str().as_bytes());
        let d = std::ffi::CString::new(dst.as_os_str().as_bytes());
        if let (Ok(s), Ok(d)) = (s, d)
            && unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), 0) } == 0 {
                ctx.progress.add_bytes(meta.len());
                return Ok(());
            }
    }
    let mut input = File::open(src).map_err(|e| err(e, src))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst)
        .map_err(|e| err(e, dst))?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if ctx.cancel.is_cancelled() {
            drop(output);
            let _ = fs::remove_file(dst);
            return Err(VfsError::Cancelled);
        }
        let n = input.read(&mut buf).map_err(|e| err(e, src))?;
        if n == 0 {
            break;
        }
        output.write_all(&buf[..n]).map_err(|e| err(e, dst))?;
        ctx.progress.add_bytes(n as u64);
    }
    output
        .set_permissions(fs::Permissions::from_mode(meta.mode() & 0o7777))
        .map_err(|e| err(e, dst))?;
    drop(output);
    set_times(dst, meta);
    Ok(())
}

fn tree_bytes(path: &Path) -> u64 {
    let mut totals = ScanTotals::default();
    let _ = scan_into(path, &mut totals, &CancelToken::new());
    totals.bytes
}

/// Merges directory `src` into existing directory `dst`.
pub fn merge_tree(src: &Path, dst: &Path, policy: MergePolicy, ctx: &Ctx) -> VfsResult<()> {
    for name in read_dir_names(src)? {
        ctx.cancel.check()?;
        let s = src.join(&name);
        let d = dst.join(&name);
        let smeta = fs::symlink_metadata(&s).map_err(|e| err(e, &s))?;
        match fs::symlink_metadata(&d) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => copy_tree(&s, &d, ctx)?,
            Err(e) => return Err(err(e, &d)),
            Ok(dmeta) if smeta.file_type().is_dir() && dmeta.file_type().is_dir() => {
                merge_tree(&s, &d, policy, ctx)?
            }
            Ok(dmeta) => {
                if replaces(policy, &smeta, &dmeta) {
                    remove_tree(&d, &Ctx {
                        cancel: ctx.cancel,
                        progress: &Progress::new(),
                    })?;
                    copy_tree(&s, &d, ctx)?;
                } else {
                    ctx.progress.add_bytes(tree_bytes(&s));
                }
            }
        }
    }
    Ok(())
}

fn replaces(policy: MergePolicy, src: &fs::Metadata, dst: &fs::Metadata) -> bool {
    match policy {
        MergePolicy::Overwrite => true,
        MergePolicy::SkipExisting => false,
        MergePolicy::OverwriteOlder => src.mtime() > dst.mtime(),
    }
}

/// Renames, falling back to copy + delete across volumes.
pub fn move_tree(src: &Path, dst: &Path, ctx: &Ctx) -> VfsResult<()> {
    match fs::rename(src, dst) {
        Ok(()) => {
            ctx.progress.add_items(1);
            Ok(())
        }
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            copy_tree(src, dst, ctx)?;
            remove_tree(src, &Ctx {
                cancel: ctx.cancel,
                progress: &Progress::new(),
            })
        }
        Err(e) => Err(err(e, src)),
    }
}

/// Moves the contents of directory `src` into existing directory `dst`;
/// items kept back by the policy stay in `src`.
pub fn merge_move(src: &Path, dst: &Path, policy: MergePolicy, ctx: &Ctx) -> VfsResult<()> {
    for name in read_dir_names(src)? {
        ctx.cancel.check()?;
        let s = src.join(&name);
        let d = dst.join(&name);
        let smeta = fs::symlink_metadata(&s).map_err(|e| err(e, &s))?;
        match fs::symlink_metadata(&d) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => move_tree(&s, &d, ctx)?,
            Err(e) => return Err(err(e, &d)),
            Ok(dmeta) if smeta.file_type().is_dir() && dmeta.file_type().is_dir() => {
                merge_move(&s, &d, policy, ctx)?
            }
            Ok(dmeta) => {
                if replaces(policy, &smeta, &dmeta) {
                    remove_tree(&d, &Ctx {
                        cancel: ctx.cancel,
                        progress: &Progress::new(),
                    })?;
                    move_tree(&s, &d, ctx)?;
                }
            }
        }
    }
    // Only succeeds when nothing was kept back.
    let _ = fs::remove_dir(src);
    Ok(())
}

pub fn remove_tree(path: &Path, ctx: &Ctx) -> VfsResult<()> {
    ctx.cancel.check()?;
    let meta = fs::symlink_metadata(path).map_err(|e| err(e, path))?;
    if meta.file_type().is_dir() {
        for name in read_dir_names(path)? {
            remove_tree(&path.join(name), ctx)?;
        }
        fs::remove_dir(path).map_err(|e| err(e, path))?;
    } else {
        fs::remove_file(path).map_err(|e| err(e, path))?;
    }
    ctx.progress.add_items(1);
    Ok(())
}

pub fn scan_into(path: &Path, totals: &mut ScanTotals, cancel: &CancelToken) -> VfsResult<()> {
    cancel.check()?;
    let meta = fs::symlink_metadata(path).map_err(|e| err(e, path))?;
    if meta.file_type().is_dir() {
        totals.dirs += 1;
        for name in read_dir_names(path)? {
            scan_into(&path.join(name), totals, cancel)?;
        }
    } else {
        totals.files += 1;
        if meta.file_type().is_file() {
            totals.bytes += meta.len();
        }
    }
    Ok(())
}

pub fn walk_into(
    root: &Path,
    rel: &mut Vec<u8>,
    out: &mut Vec<WalkEntry>,
    cancel: &CancelToken,
) -> VfsResult<()> {
    use std::os::unix::ffi::OsStrExt;
    let dir = root.join(std::ffi::OsStr::from_bytes(rel));
    for name in read_dir_names(&dir)? {
        cancel.check()?;
        let path = dir.join(&name);
        let meta = fs::symlink_metadata(&path).map_err(|e| err(e, &path))?;
        let len = rel.len();
        if !rel.is_empty() {
            rel.push(b'/');
        }
        rel.extend_from_slice(name.as_bytes());
        let kind = EntryKind::from_mode(meta.mode());
        out.push(WalkEntry {
            rel: rel.clone(),
            kind,
            size: meta.len(),
            mtime: Some(meta.mtime()),
        });
        if kind == EntryKind::Dir {
            walk_into(root, rel, out, cancel)?;
        }
        rel.truncate(len);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_merge_move() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        fs::create_dir_all(a.join("sub")).unwrap();
        fs::write(a.join("one.txt"), "1").unwrap();
        fs::write(a.join("sub/two.txt"), "22").unwrap();
        std::os::unix::fs::symlink("one.txt", a.join("link")).unwrap();
        let cancel = CancelToken::new();
        let progress = Progress::new();
        let ctx = Ctx {
            cancel: &cancel,
            progress: &progress,
        };

        let b = tmp.path().join("b");
        copy_tree(&a, &b, &ctx).unwrap();
        assert_eq!(fs::read_to_string(b.join("sub/two.txt")).unwrap(), "22");
        assert_eq!(fs::read_link(b.join("link")).unwrap(), Path::new("one.txt"));

        fs::write(a.join("one.txt"), "changed").unwrap();
        fs::write(a.join("new.txt"), "n").unwrap();
        merge_tree(&a, &b, MergePolicy::SkipExisting, &ctx).unwrap();
        assert_eq!(fs::read_to_string(b.join("one.txt")).unwrap(), "1");
        assert_eq!(fs::read_to_string(b.join("new.txt")).unwrap(), "n");
        merge_tree(&a, &b, MergePolicy::Overwrite, &ctx).unwrap();
        assert_eq!(fs::read_to_string(b.join("one.txt")).unwrap(), "changed");

        // Moving with "skip existing" keeps conflicting files in the source.
        fs::write(a.join("only-src.txt"), "x").unwrap();
        merge_move(&a, &b, MergePolicy::SkipExisting, &ctx).unwrap();
        assert!(b.join("only-src.txt").exists());
        assert!(a.join("one.txt").exists());
        assert!(!a.join("only-src.txt").exists());

        let mut totals = ScanTotals::default();
        scan_into(&b, &mut totals, &cancel).unwrap();
        assert_eq!(totals.dirs, 2);

        let mut walked = Vec::new();
        walk_into(&b, &mut Vec::new(), &mut walked, &cancel).unwrap();
        assert!(walked.iter().any(|w| w.rel == b"sub/two.txt"));

        remove_tree(&b, &ctx).unwrap();
        assert!(!b.exists());
    }
}
