//! Directory listings over the text protocol.
//!
//! GNU `find -printf` gives exact, NUL-separated records (names may contain
//! anything but `/` and NUL). Servers without it (BusyBox, BSD, macOS) fall
//! back to batched `stat` calls plus a symlink pass.

use tl_core::shell::Script;
use tl_core::{Entry, EntryKind, ScanTotals, VPath, WalkEntry};

use crate::scripts::require_dir;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dialect {
    /// GNU findutils: `find -printf`.
    #[default]
    Gnu,
    /// GNU or BusyBox `stat -c`.
    StatC,
    /// BSD/macOS `stat -f`.
    StatF,
}

impl Dialect {
    pub fn name(self) -> &'static str {
        match self {
            Dialect::Gnu => "GNU tools",
            Dialect::StatC => "BusyBox/stat -c",
            Dialect::StatF => "BSD tools",
        }
    }

    fn stat_cmd(self) -> &'static str {
        match self {
            Dialect::StatF => "stat -f '%p/%z/%m/%u/%g/%Su/%Sg/%N'",
            _ => "stat -c '%f/%s/%Y/%u/%g/%U/%G/%n'",
        }
    }

    fn stat_size_cmd(self) -> &'static str {
        match self {
            Dialect::StatF => "stat -f %z",
            _ => "stat -c %s",
        }
    }
}

const GNU_FMT: &str = r"%y\0%Y\0%m\0%s\0%T@\0%U\0%G\0%u\0%g\0%f\0%l\0";
const GNU_FIELDS: usize = 11;

/// Lists the entries of `dir` (not recursive).
pub fn list_script(dialect: Dialect, dir: &VPath) -> Script {
    let mut s = Script::new();
    require_dir(&mut s, dir.as_bytes());
    match dialect {
        Dialect::Gnu => {
            s.raw("find . -mindepth 1 -maxdepth 1 -printf '")
                .raw(GNU_FMT)
                .raw("' 2>/dev/null\nexit 0\n");
        }
        _ => {
            // `./`: GNU stat takes a file named `-` for standard input.
            s.raw("printf './%s\\0' * .[!.]* ..?* | xargs -0 ")
                .raw(dialect.stat_cmd())
                .raw(" -- 2>/dev/null\n")
                .raw(SYMLINK_PASS)
                .raw("exit 0\n");
        }
    }
    s
}

/// Reports target and target kind of every symlink in the current
/// directory, after a `\036` separator.
const SYMLINK_PASS: &str = r#"printf '\036'
for f in * .[!.]* ..?*; do
  if [ -L "$f" ]; then
    t=$(readlink -- "$f")
    if [ -d "$f" ]; then k=d; elif [ -e "$f" ]; then k=f; else k=n; fi
    printf '%s\0%s\0%s\0' "$f" "$t" "$k"
  fi
done
"#;

pub fn parse_list(dialect: Dialect, out: &[u8]) -> Vec<Entry> {
    match dialect {
        Dialect::Gnu => parse_gnu(out),
        _ => parse_stat_listing(dialect, out),
    }
}

fn parse_gnu(out: &[u8]) -> Vec<Entry> {
    let mut fields: Vec<&[u8]> = out.split(|b| *b == 0).collect();
    // Trailing terminator yields one empty element.
    if fields.last().is_some_and(|f| f.is_empty()) {
        fields.pop();
    }
    fields
        .as_chunks::<GNU_FIELDS>().0.iter()
        .filter_map(|f| {
            let kind = EntryKind::from_find_letter(f[0])?;
            let mut entry = Entry::new(f[9].to_vec(), kind);
            entry.mode = parse_num(f[2], 8).unwrap_or(0) as u32;
            entry.size = parse_num(f[3], 10).unwrap_or(0);
            entry.mtime = parse_epoch(f[4]);
            entry.uid = parse_num(f[5], 10).map(|v| v as u32);
            entry.gid = parse_num(f[6], 10).map(|v| v as u32);
            entry.user = Some(String::from_utf8_lossy(f[7]).into_owned());
            entry.group = Some(String::from_utf8_lossy(f[8]).into_owned());
            if kind == EntryKind::Symlink {
                entry.link_target = Some(f[10].to_vec());
                entry.target_kind = EntryKind::from_find_letter(f[1]);
            }
            Some(entry)
        })
        .collect()
}

fn parse_num(field: &[u8], radix: u32) -> Option<u64> {
    u64::from_str_radix(std::str::from_utf8(field).ok()?.trim(), radix).ok()
}

/// `1697371234.123456789` → 1697371234.
fn parse_epoch(field: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(field).ok()?.trim();
    let whole = s.split('.').next()?;
    whole.parse().ok()
}

/// One `stat` line: mode/size/mtime/uid/gid/user/group/name.
fn parse_stat_line(dialect: Dialect, line: &[u8]) -> Option<Entry> {
    let mut parts = line.splitn(8, |b| *b == b'/');
    let mode_field = parts.next()?;
    let mode = match dialect {
        Dialect::StatF => parse_num(mode_field, 8)?,
        _ => parse_num(mode_field, 16)?,
    } as u32;
    let size = parse_num(parts.next()?, 10).unwrap_or(0);
    let mtime = parse_epoch(parts.next()?);
    let uid = parse_num(parts.next()?, 10).map(|v| v as u32);
    let gid = parse_num(parts.next()?, 10).map(|v| v as u32);
    let user = String::from_utf8_lossy(parts.next()?).into_owned();
    let group = String::from_utf8_lossy(parts.next()?).into_owned();
    let name = parts.next()?;
    let name = name.strip_prefix(b"./").unwrap_or(name);
    if name.is_empty() {
        return None;
    }
    let mut entry = Entry::new(name.to_vec(), EntryKind::from_mode(mode));
    entry.mode = mode & 0o7777;
    entry.size = size;
    entry.mtime = mtime;
    entry.uid = uid;
    entry.gid = gid;
    entry.user = Some(user);
    entry.group = Some(group);
    Some(entry)
}

fn parse_stat_listing(dialect: Dialect, out: &[u8]) -> Vec<Entry> {
    let (stats, links) = match out.iter().position(|b| *b == 0x1e) {
        Some(i) => (&out[..i], &out[i + 1..]),
        None => (out, &out[out.len()..]),
    };
    let mut entries: Vec<Entry> = stats
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .filter_map(|l| parse_stat_line(dialect, l))
        .collect();
    let mut fields: Vec<&[u8]> = links.split(|b| *b == 0).collect();
    if fields.last().is_some_and(|f| f.is_empty()) {
        fields.pop();
    }
    for triple in fields.as_chunks::<3>().0 {
        if let Some(entry) = entries.iter_mut().find(|e| e.name == triple[0]) {
            entry.link_target = Some(triple[1].to_vec());
            entry.target_kind = match triple[2] {
                b"d" => Some(EntryKind::Dir),
                b"f" => Some(EntryKind::File),
                _ => None,
            };
        }
    }
    entries
}

/// `lstat` of one path; exits 3 when it does not exist.
pub fn stat_script(dialect: Dialect, path: &VPath) -> Script {
    let mut s = Script::new();
    s.raw("p=").arg(path.as_bytes()).raw("\n");
    s.raw("if [ ! -e \"$p\" ] && [ ! -L \"$p\" ]; then exit 3; fi\n");
    match dialect {
        Dialect::Gnu => {
            s.raw("find \"$p\" -maxdepth 0 -printf '").raw(GNU_FMT).raw("'\n");
        }
        _ => {
            s.raw(dialect.stat_cmd()).raw(" -- \"$p\"\n");
            s.raw(concat!(
                "printf '\\036'\n",
                "if [ -L \"$p\" ]; then t=$(readlink -- \"$p\")\n",
                "  if [ -d \"$p\" ]; then k=d; elif [ -e \"$p\" ]; then k=f; else k=n; fi\n",
                "  printf '%s\\0%s\\0%s\\0' \"$p\" \"$t\" \"$k\"\n",
                "fi\n",
            ));
        }
    }
    s
}

pub fn parse_stat(dialect: Dialect, path: &VPath, out: &[u8]) -> Option<Entry> {
    let mut entry = match dialect {
        Dialect::Gnu => parse_gnu(out).into_iter().next()?,
        _ => {
            // Names in this output are full paths; match the symlink pass
            // against the same full path.
            let mut entries = parse_stat_listing(dialect, out);
            if entries.is_empty() {
                return None;
            }
            entries.swap_remove(0)
        }
    };
    entry.name = path.file_name().unwrap_or(b"/").to_vec();
    Some(entry)
}

/// Recursive listing below `root`, relative paths, parents first.
pub fn walk_script(dialect: Dialect, root: &VPath) -> Script {
    let mut s = Script::new();
    require_dir(&mut s, root.as_bytes());
    match dialect {
        Dialect::Gnu => {
            s.raw(r"find . -mindepth 1 -printf '%y\0%s\0%T@\0%P\0' 2>/dev/null")
                .raw("\nexit 0\n");
        }
        _ => {
            s.raw("find . -mindepth 1 -exec ")
                .raw(dialect.stat_cmd())
                .raw(" -- {} + 2>/dev/null\nexit 0\n");
        }
    }
    s
}

pub fn parse_walk(dialect: Dialect, out: &[u8]) -> Vec<WalkEntry> {
    match dialect {
        Dialect::Gnu => {
            let mut fields: Vec<&[u8]> = out.split(|b| *b == 0).collect();
            if fields.last().is_some_and(|f| f.is_empty()) {
                fields.pop();
            }
            fields
                .as_chunks::<4>().0.iter()
                .filter_map(|f| {
                    Some(WalkEntry {
                        kind: EntryKind::from_find_letter(f[0])?,
                        size: parse_num(f[1], 10).unwrap_or(0),
                        mtime: parse_epoch(f[2]),
                        rel: f[3].to_vec(),
                    })
                })
                .collect()
        }
        _ => out
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .filter_map(|l| parse_stat_line(dialect, l))
            .map(|e| WalkEntry {
                rel: e.name,
                kind: e.kind,
                size: e.size,
                mtime: e.mtime,
            })
            .collect(),
    }
}

/// Recursive totals of `paths`: prints `bytes files dirs`.
pub fn scan_script(dialect: Dialect, paths: &[VPath]) -> Script {
    let mut s = Script::new();
    match dialect {
        Dialect::Gnu => {
            s.raw("find");
            for p in paths {
                s.sp_arg(p.as_bytes());
            }
            s.raw(
                " -printf '%y %s\\n' 2>/dev/null | awk '$1==\"d\"{d++;next}{f++;if($1==\"f\")b+=$2}END{printf \"%.0f %d %d\\n\",b,f,d}'\n",
            );
        }
        _ => {
            s.raw("d=$(find");
            for p in paths {
                s.sp_arg(p.as_bytes());
            }
            s.raw(" -type d 2>/dev/null | wc -l)\nfind");
            for p in paths {
                s.sp_arg(p.as_bytes());
            }
            s.raw(" ! -type d -exec ")
                .raw(dialect.stat_size_cmd())
                .raw(" -- {} + 2>/dev/null | awk -v d=\"$d\" '{b+=$1;f++}END{printf \"%.0f %d %d\\n\",b,f,d+0}'\n");
        }
    }
    s
}

pub fn parse_scan(out: &[u8]) -> ScanTotals {
    let text = String::from_utf8_lossy(out);
    let mut nums = text.split_whitespace().map(|n| n.parse::<u64>().unwrap_or(0));
    ScanTotals {
        bytes: nums.next().unwrap_or(0),
        files: nums.next().unwrap_or(0),
        dirs: nums.next().unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnu_records() {
        let out = b"d\x00d\x00755\x004096\x001697371234.5\x000\x000\x00root\x00root\x00etc\x00\x00\
l\x00d\x00777\x007\x001697371234.0\x001000\x001000\x00me\x00me\x00link\x00/etc\x00\
l\x00N\x00777\x003\x001697371234.0\x001000\x001000\x00me\x00me\x00dead\x00nowhere\x00\
f\x00f\x004755\x00100\x001\x00 0\x000\x00root\x00root\x00we ird\nname\x00\x00";
        let entries = parse_gnu(out);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].name, b"etc");
        assert!(entries[0].is_dir());
        assert_eq!(entries[0].mode, 0o755);
        assert_eq!(entries[0].mtime, Some(1697371234));
        assert!(entries[1].is_dir());
        assert_eq!(entries[1].link_target.as_deref(), Some(&b"/etc"[..]));
        assert_eq!(entries[2].target_kind, None);
        assert_eq!(entries[3].name, b"we ird\nname");
        assert_eq!(entries[3].mode, 0o4755);
    }

    #[test]
    fn stat_c_records() {
        let out = b"41ed/4096/1697371234/0/0/root/root/etc\n81a4/12/1697371235/1000/1000/me/me/notes.txt\na1ff/4/1697371236/1000/1000/me/me/lnk\n\x1elnk\0etc\0d\0";
        let entries = parse_stat_listing(Dialect::StatC, out);
        assert_eq!(entries.len(), 3);
        assert!(entries[0].is_dir());
        assert_eq!(entries[1].size, 12);
        assert_eq!(entries[1].mode, 0o644);
        assert!(entries[2].is_dir());
        assert_eq!(entries[2].link_target.as_deref(), Some(&b"etc"[..]));
    }

    #[test]
    fn stat_f_records() {
        let out = b"40755/4096/1697371234/0/0/root/wheel/etc\n100644/12/1697371235/501/20/me/staff/a.txt\n\x1e";
        let entries = parse_stat_listing(Dialect::StatF, out);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_dir());
        assert_eq!(entries[1].group.as_deref(), Some("staff"));
    }

    #[test]
    fn scan_output() {
        assert_eq!(
            parse_scan(b"1234 5 2\n"),
            ScanTotals { bytes: 1234, files: 5, dirs: 2 }
        );
    }
}
