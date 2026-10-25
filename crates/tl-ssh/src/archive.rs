//! Archives on servers, handled by the server's own `tar` and `unzip` /
//! `zip`, in keeping with the command protocol. Tar listings parse
//! `tar -tv`; zip listings read just the central directory (`tail -c`),
//! because `unzip -l` mangles non-ASCII names. Extracting unpacks the
//! chosen members into a temporary folder on the server and streams them
//! back as tar; changes unpack the archive there, apply the change, pack it
//! again and swap it in, keeping the archive's owner and permissions.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tl_core::archive::{ArchiveBackend, ArchiveFormat, ArchiveMember, member_join, normalize_member};
use tl_core::shell::Script;
use tl_core::{EntryKind, OpCtx, TarSink, TarSource, VPath, VfsError, VfsResult};

use crate::remote::{Remote, RunOpts, RunOutput};

/// Uses a UTF-8 locale when the server has one, so the tools print and
/// match names byte for byte.
const UTF8_LOCALE: &str = r#"for tl_l in C.UTF-8 C.utf8 en_US.UTF-8 en_US.utf8; do
  if [ "$(LC_ALL=$tl_l locale charmap 2>/dev/null)" = UTF-8 ]; then LC_ALL=$tl_l; export LC_ALL; break; fi
done
"#;

/// Which tar the server has: GNU tar matches member names literally with
/// `--no-wildcards`; bsdtar and BusyBox take patterns.
const TAR_FLAVOR: &str = r#"case "$(tar --version 2>/dev/null | head -n 1)" in
  *'GNU tar'*) tl_tar=gnu ;;
  *bsdtar*) tl_tar=bsd ;;
  *) tl_tar=other ;;
esac
"#;

/// A private temporary folder `$t`, removed on exit; `$n` (the new archive
/// next to the old one) goes too unless it was moved into place.
const TEMP_DIR: &str = r#"n=
t=$(mktemp -d "${TMPDIR:-/tmp}/tl.XXXXXX") || { echo TLERR:EACCES >&2; exit 2; }
trap 'rm -rf "$t"; if [ -n "$n" ]; then rm -f "$n"; fi' EXIT
trap 'exit 1' HUP INT TERM
"#;

/// Puts the archive built at `$n` in place of `$a`: renamed over it when it
/// sits next to it (owner and mode copied), else copied into it. Signals
/// are ignored meanwhile so a cancel cannot leave half an archive.
const REPLACE: &str = r#"if [ "$n" != "$t/new" ]; then
  tl_mode=$(stat -c %a "$a" 2>/dev/null || stat -f %Lp "$a" 2>/dev/null)
  tl_own=$(stat -c %u:%g "$a" 2>/dev/null || stat -f %u:%g "$a" 2>/dev/null)
  if [ -n "$tl_mode" ]; then chmod "$tl_mode" "$n" 2>/dev/null; fi
  if [ -n "$tl_own" ]; then chown "$tl_own" "$n" 2>/dev/null; fi
  trap '' HUP INT TERM
  mv -f "$n" "$a" || exit 1
  n=
else
  trap '' HUP INT TERM
  cat "$n" > "$a" || exit 1
fi
"#;

/// Member path → (name as stored, is a folder).
type StoredNames = HashMap<Vec<u8>, (Vec<u8>, bool)>;

pub struct RemoteArchive {
    remote: Arc<Remote>,
    path: VPath,
    format: ArchiveFormat,
    /// `tar` understands `-p`.
    tar_p: bool,
    /// From the last listing.
    stored: Mutex<StoredNames>,
}

/// Escapes pattern characters for bsdtar and BusyBox tar.
fn tar_escape(name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() + 4);
    for &b in name {
        if matches!(b, b'\\' | b'*' | b'?' | b'[') {
            out.push(b'\\');
        }
        out.push(b);
    }
    out
}

/// Escapes pattern characters for `unzip` and `zip` (bracket classes work
/// in Info-ZIP and BusyBox alike).
fn zip_escape(name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() + 4);
    for &b in name {
        match b {
            b'*' | b'?' | b'[' => {
                out.push(b'[');
                out.push(b);
                out.push(b']');
            }
            _ => out.push(b),
        }
    }
    out
}

impl RemoteArchive {
    pub fn new(remote: Arc<Remote>, path: VPath, format: ArchiveFormat, tar_p: bool) -> Self {
        RemoteArchive {
            remote,
            path,
            format,
            tar_p,
            stored: Mutex::new(HashMap::new()),
        }
    }

    fn is_zip(&self) -> bool {
        self.format == ArchiveFormat::Zip
    }

    /// `-x`/`-t`/`-c` plus the compression flag.
    fn tar_cmd(&self, op: &str) -> String {
        let mut cmd = format!("tar {op}");
        if self.format == ArchiveFormat::TarGz {
            cmd.push_str(" -z");
        }
        if op == "-x" && self.tar_p {
            cmd.push_str(" -p");
        }
        cmd
    }

    fn prologue(&self, s: &mut Script, writable: bool) {
        s.raw("a=").arg(self.path.as_bytes()).raw("\n");
        s.raw(concat!(
            "if [ ! -e \"$a\" ]; then echo TLERR:ENOENT >&2; exit 2; fi\n",
            "if [ -d \"$a\" ]; then echo TLERR:EISDIR >&2; exit 2; fi\n",
            "if [ ! -r \"$a\" ]; then echo TLERR:EACCES >&2; exit 2; fi\n",
        ));
        if writable {
            s.raw("if [ ! -w \"$a\" ]; then echo TLERR:EACCES >&2; exit 2; fi\n");
        }
        s.raw(UTF8_LOCALE);
        if !self.is_zip() {
            s.raw(TAR_FLAVOR);
        }
    }

    fn run(&self, s: &Script, ctx: &OpCtx) -> VfsResult<RunOutput> {
        let out = self.remote.run(
            s.as_bytes(),
            RunOpts {
                elevated: ctx.elevated,
                ..Default::default()
            },
            &ctx.cancel,
        )?;
        if out.success() { Ok(out) } else { Err(self.error(&out)) }
    }

    fn error(&self, out: &RunOutput) -> VfsError {
        let missing = |tool: &str| {
            out.stderr.lines().any(|l| l.contains(tool) && (l.contains("not found") || l.contains("No such file")))
        };
        match out.tlerr() {
            Some("NOZIP") => VfsError::Unsupported(format!(
                "changing zip archives on {}: the server has no `zip` command",
                self.remote.label
            )),
            _ if self.is_zip() && missing("unzip") => VfsError::Unsupported(format!(
                "zip archives on {}: the server has no `unzip` command",
                self.remote.label
            )),
            _ => out.error(&self.path.display()),
        }
    }

    /// Stored name of a member path (synthesized for implied folders).
    fn stored_name(&self, path: &[u8]) -> (Vec<u8>, bool) {
        self.stored.lock().get(path).cloned().unwrap_or_else(|| (path.to_vec(), false))
    }

    fn remember(&self, members: &[ArchiveMember]) {
        let mut stored = HashMap::new();
        for member in members {
            let Some(rel) = normalize_member(&member.raw) else { continue };
            let raw = member.raw.strip_suffix(b"/").unwrap_or(&member.raw);
            // What precedes the first component: `./`, `/` or nothing.
            let mut rest = raw;
            while let Some(r) = rest.strip_prefix(b"./").or_else(|| rest.strip_prefix(b"/")) {
                rest = r;
            }
            let prefix = &raw[..raw.len() - rest.len()];
            for (i, _) in rel.iter().enumerate().filter(|(_, b)| **b == b'/') {
                let mut implied = prefix.to_vec();
                implied.extend_from_slice(&rel[..i]);
                stored.entry(rel[..i].to_vec()).or_insert((implied, true));
            }
            stored.insert(rel, (raw.to_vec(), member.kind == EntryKind::Dir));
        }
        *self.stored.lock() = stored;
    }

    /// Sets `"$@"` to the tar member arguments for `paths`.
    fn tar_members(&self, s: &mut Script, paths: &[Vec<u8>]) {
        let names: Vec<Vec<u8>> = paths.iter().map(|p| self.stored_name(p).0).collect();
        s.raw("if [ \"$tl_tar\" = gnu ]; then set -- --no-wildcards --");
        for name in &names {
            s.sp_arg(name);
        }
        s.raw("; else set -- --");
        for name in &names {
            s.sp_arg(tar_escape(name));
        }
        s.raw("; fi\n");
    }

    /// `unzip`/`zip` patterns for `paths` (a folder with all it holds).
    fn zip_patterns(&self, s: &mut Script, paths: &[Vec<u8>]) {
        for path in paths {
            let (name, is_dir) = self.stored_name(path);
            let mut pattern = zip_escape(&name);
            if is_dir {
                pattern.extend_from_slice(b"/*");
            }
            s.sp_arg(pattern);
        }
    }

    /// Fails early when `zip` is missing (changing zip archives needs it).
    fn require_zip(&self, ctx: &OpCtx) -> VfsResult<()> {
        let mut s = Script::new();
        s.raw("command -v zip >/dev/null 2>&1 || { echo TLERR:NOZIP >&2; exit 2; }\n");
        self.run(&s, ctx).map(drop)
    }

    /// Script lines that unpack the whole archive into `$t/r` and choose
    /// where the new archive is built (`$n`).
    fn unpack_all(&self, s: &mut Script) {
        s.raw(TEMP_DIR);
        s.raw("n=$(mktemp \"$a.tl-XXXXXX\" 2>/dev/null) || n=\"$t/new\"\n");
        if self.is_zip() {
            // `zip` updates a copy of the archive in place.
            s.raw("cp \"$a\" \"$n\" || exit 1\nmkdir \"$t/r\" || exit 1\n");
        } else {
            s.raw("mkdir \"$t/r\" || exit 1\n")
                .raw(&self.tar_cmd("-x"))
                .raw(" -f \"$a\" -C \"$t/r\" >&2 || exit 1\n");
        }
    }

    /// Script lines that pack `$t/r` into `$n` and put it in place.
    fn repack(&self, s: &mut Script) {
        if self.is_zip() {
            s.raw("if [ -n \"$(ls -A \"$t/r\")\" ]; then (cd \"$t/r\" && zip -q -r -y \"$n\" .) >&2 || exit 1; fi\n");
        } else {
            s.raw("(cd \"$t/r\" && ")
                .raw(&self.tar_cmd("-c"))
                .raw(" -f \"$n\" .) >&2 || exit 1\n");
        }
        s.raw(REPLACE);
    }
}

impl RemoteArchive {
    /// Reads the zip's central directory: the tail of the file holds the
    /// end record (and, for most archives, the whole directory); a larger
    /// directory is fetched in a second command.
    fn list_zip(&self, ctx: &OpCtx) -> VfsResult<Vec<ArchiveMember>> {
        let mut s = Script::new();
        self.prologue(&mut s, false);
        s.raw(&format!("wc -c < \"$a\" | tr -d ' '\ntail -c {ZIP_TAIL} \"$a\"\n"));
        let out = self.run(&s, ctx)?;
        let broken = || VfsError::other(format!("{} is not a valid zip archive", self.path.file_name_lossy()));
        let newline = out.stdout.iter().position(|b| *b == b'\n').ok_or_else(broken)?;
        let size: u64 = std::str::from_utf8(&out.stdout[..newline])
            .ok()
            .and_then(|n| n.trim().parse().ok())
            .ok_or_else(broken)?;
        let tail = &out.stdout[newline + 1..];
        let tail_start = size.checked_sub(tail.len() as u64).ok_or_else(broken)?;
        let dir = find_central_directory(tail, tail_start).ok_or_else(broken)?;
        let fetched;
        let bytes = if dir.offset >= tail_start {
            let start = (dir.offset - tail_start) as usize;
            tail.get(start..start + dir.size as usize).ok_or_else(broken)?
        } else {
            let mut s = Script::new();
            self.prologue(&mut s, false);
            s.raw(&format!("tail -c +{} \"$a\" | head -c {}\n", dir.offset + 1, dir.size));
            fetched = self.run(&s, ctx)?.stdout;
            &fetched[..]
        };
        Ok(parse_central_directory(bytes))
    }
}

impl ArchiveBackend for RemoteArchive {
    fn list(&self, ctx: &OpCtx) -> VfsResult<Vec<ArchiveMember>> {
        let members = if self.is_zip() {
            self.list_zip(ctx)?
        } else {
            let mut s = Script::new();
            self.prologue(&mut s, false);
            let list = self.tar_cmd("-t -v");
            s.raw("TZ=UTC0; export TZ\nprintf '%s\\n' \"$tl_tar\"\n");
            s.raw(&format!(
                "if [ \"$tl_tar\" = gnu ]; then exec {list} --numeric-owner --full-time --quoting-style=escape -f \"$a\"; fi\nexec {list} -f \"$a\"\n"
            ));
            parse_tar_listing(&self.run(&s, ctx)?.stdout, now())
        };
        self.remember(&members);
        Ok(members)
    }

    fn export_tar(&self, base: &[u8], names: &[Vec<u8>], ctx: &OpCtx) -> VfsResult<Box<dyn TarSource>> {
        let paths: Vec<Vec<u8>> = names.iter().map(|n| member_join(base, n)).collect();
        let mut s = Script::new();
        self.prologue(&mut s, false);
        s.raw(TEMP_DIR);
        if self.is_zip() {
            s.raw("unzip -o -qq \"$a\"");
            self.zip_patterns(&mut s, &paths);
            s.raw(" -d \"$t\" >&2\ntl_rc=$?; if [ $tl_rc -gt 1 ]; then exit $tl_rc; fi\n");
        } else {
            self.tar_members(&mut s, &paths);
            // BusyBox extracts escaped names but then reports them "not
            // found"; the `tar -c` below fails if anything is missing.
            s.raw(&self.tar_cmd("-x"))
                .raw(" -f \"$a\" -C \"$t\" \"$@\" >&2 || [ \"$tl_tar\" = other ] || exit 1\n");
        }
        s.raw("cd \"$t\"");
        if !base.is_empty() {
            s.raw("/").arg(base);
        }
        s.raw(" || exit 1\ntar -c -f -");
        for name in names {
            let mut rel = b"./".to_vec();
            rel.extend_from_slice(name);
            s.sp_arg(&rel);
        }
        s.raw("\n");
        let source = self.remote.source(s.as_bytes(), ctx.elevated, &ctx.cancel, self.path.display())?;
        Ok(Box::new(source))
    }

    fn import_tar(&self, dir: &[u8], ctx: &OpCtx) -> VfsResult<Box<dyn TarSink>> {
        if self.is_zip() {
            self.require_zip(ctx)?;
        }
        let mut s = Script::new();
        self.prologue(&mut s, true);
        self.unpack_all(&mut s);
        s.raw("mkdir -p \"$t/r\"");
        if !dir.is_empty() {
            s.raw("/").arg(dir);
        }
        s.raw(" || exit 1\nprintf R\n");
        s.raw(if self.tar_p { "tar -x -p -o -f - -C \"$t/r\"" } else { "tar -x -o -f - -C \"$t/r\"" });
        if !dir.is_empty() {
            s.raw("/").arg(dir);
        }
        s.raw(" >&2 || exit 1\n");
        self.repack(&mut s);
        let sink = self.remote.sink(s.as_bytes(), ctx.elevated, &ctx.cancel, self.path.display())?;
        Ok(Box::new(sink))
    }

    fn delete(&self, paths: &[Vec<u8>], ctx: &OpCtx) -> VfsResult<()> {
        let mut s = Script::new();
        if self.is_zip() {
            self.require_zip(ctx)?;
            self.prologue(&mut s, true);
            s.raw(TEMP_DIR);
            s.raw("n=$(mktemp \"$a.tl-XXXXXX\" 2>/dev/null) || n=\"$t/new\"\ncp \"$a\" \"$n\" || exit 1\nzip -q -d \"$n\"");
            self.zip_patterns(&mut s, paths);
            // 12: nothing matched.
            s.raw(" >&2\ntl_rc=$?; if [ $tl_rc -ne 0 ] && [ $tl_rc -ne 12 ]; then exit $tl_rc; fi\n");
            s.raw(REPLACE);
        } else {
            self.prologue(&mut s, true);
            self.unpack_all(&mut s);
            s.raw("(cd \"$t/r\" && rm -rf --");
            for path in paths {
                let mut rel = b"./".to_vec();
                rel.extend_from_slice(path);
                s.sp_arg(&rel);
            }
            s.raw(") || exit 1\n");
            self.repack(&mut s);
        }
        self.run(&s, ctx).map(drop)
    }

    fn read_file(&self, path: &[u8], limit: u64, ctx: &OpCtx) -> VfsResult<Vec<u8>> {
        let mut s = Script::new();
        self.prologue(&mut s, false);
        let paths = [path.to_vec()];
        if self.is_zip() {
            s.raw("unzip -p \"$a\"");
            self.zip_patterns(&mut s, &paths);
        } else {
            self.tar_members(&mut s, &paths);
            s.raw("{ ").raw(&self.tar_cmd("-x")).raw(" -O -f \"$a\" \"$@\" || [ \"$tl_tar\" = other ]; }");
        }
        if limit != u64::MAX {
            s.raw(&format!(" | head -c {limit}"));
        }
        s.raw("\n");
        Ok(self.run(&s, ctx)?.stdout)
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The first `n` space-separated fields of `line`, and what follows the
/// single space after the last of them.
fn split_fields(line: &[u8], n: usize) -> Option<(Vec<&[u8]>, &[u8])> {
    let mut fields = Vec::with_capacity(n);
    let mut i = 0;
    while fields.len() < n {
        while line.get(i) == Some(&b' ') {
            i += 1;
        }
        let start = i;
        while i < line.len() && line[i] != b' ' {
            i += 1;
        }
        if start == i {
            return None;
        }
        fields.push(&line[start..i]);
    }
    if line.get(i) != Some(&b' ') {
        return None;
    }
    Some((fields, &line[i + 1..]))
}

/// Undoes the backslash escapes GNU tar and bsdtar put in names.
fn unescape(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'\\' && i + 1 < s.len() {
            let octal = |b: &u8| (b'0'..=b'7').contains(b);
            if i + 3 < s.len() && s[i + 1..i + 4].iter().all(octal) {
                let value = s[i + 1..i + 4].iter().fold(0u32, |v, d| v * 8 + u32::from(d - b'0'));
                out.push(value as u8);
                i += 4;
                continue;
            }
            let mapped = match s[i + 1] {
                b'\\' => Some(b'\\'),
                b'n' => Some(b'\n'),
                b't' => Some(b'\t'),
                b'r' => Some(b'\r'),
                b'a' => Some(7),
                b'b' => Some(8),
                b'f' => Some(12),
                b'v' => Some(11),
                _ => None,
            };
            if let Some(b) = mapped {
                out.push(b);
                i += 2;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// `drwxr-xr-x` → kind and permission bits.
fn parse_perm(perm: &[u8]) -> (EntryKind, u32) {
    let kind = match perm.first() {
        Some(b'd') => EntryKind::Dir,
        Some(b'l') => EntryKind::Symlink,
        Some(b'c') => EntryKind::CharDevice,
        Some(b'b') => EntryKind::BlockDevice,
        Some(b'p') => EntryKind::Fifo,
        Some(b's') => EntryKind::Socket,
        _ => EntryKind::File,
    };
    if perm.len() < 10 {
        return (kind, 0);
    }
    let p = &perm[1..10];
    let mut mode = 0;
    for (i, bit) in [(0, 0o400), (1, 0o200), (3, 0o40), (4, 0o20), (6, 0o4), (7, 0o2)] {
        if p[i] != b'-' {
            mode |= bit;
        }
    }
    mode |= match p[2] {
        b'x' => 0o100,
        b's' => 0o4100,
        b'S' => 0o4000,
        _ => 0,
    };
    mode |= match p[5] {
        b'x' => 0o10,
        b's' => 0o2010,
        b'S' => 0o2000,
        _ => 0,
    };
    mode |= match p[8] {
        b'x' => 0o1,
        b't' => 0o1001,
        b'T' => 0o1000,
        _ => 0,
    };
    (kind, mode)
}

/// Days since the Unix epoch of a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn year_of(epoch: i64) -> i64 {
    // Days → civil year (inverse of `days_from_civil`).
    let z = epoch.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    yoe + era * 400 + i64::from(m <= 2)
}

fn num(field: &[u8]) -> Option<i64> {
    std::str::from_utf8(field).ok()?.parse().ok()
}

fn epoch(y: i64, m: i64, d: i64, hh: i64, mm: i64, ss: i64) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 61 {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// `2024-01-02` + `03:04[:05]`.
fn iso_time(date: &[u8], time: &[u8]) -> Option<i64> {
    let d: Vec<i64> = date.split(|b| *b == b'-').map(num).collect::<Option<_>>()?;
    let t: Vec<i64> = time.split(|b| *b == b':').map(num).collect::<Option<_>>()?;
    match (d.as_slice(), t.as_slice()) {
        ([y, m, d], [hh, mm]) => epoch(*y, *m, *d, *hh, *mm, 0),
        ([y, m, d], [hh, mm, ss]) => epoch(*y, *m, *d, *hh, *mm, *ss),
        _ => None,
    }
}

const MONTHS: [&[u8]; 12] = [b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec"];

/// `ls`-style `Jan 2 03:04` (recent) or `Jan 2 2020`.
fn ls_time(month: &[u8], day: &[u8], time_or_year: &[u8], now: i64) -> Option<i64> {
    let m = MONTHS.iter().position(|x| *x == month)? as i64 + 1;
    let d = num(day)?;
    if let Some((hh, mm)) = time_or_year.iter().position(|b| *b == b':').map(|i| (&time_or_year[..i], &time_or_year[i + 1..])) {
        let (hh, mm) = (num(hh)?, num(mm)?);
        let year = year_of(now);
        let t = epoch(year, m, d, hh, mm, 0)?;
        // Recent dates are shown without a year; one far ahead is last year's.
        return if t > now + 183 * 86_400 { epoch(year - 1, m, d, hh, mm, 0) } else { Some(t) };
    }
    epoch(num(time_or_year)?, m, d, 0, 0, 0)
}

/// Splits `name -> target` (symlinks) or `name link to target` (hard links).
fn split_link(rest: &[u8], separator: &[u8]) -> (Vec<u8>, Option<Vec<u8>>) {
    match rest.windows(separator.len()).rposition(|w| w == separator) {
        Some(i) => (rest[..i].to_vec(), Some(rest[i + separator.len()..].to_vec())),
        None => (rest.to_vec(), None),
    }
}

/// Parses `tar -tv` output. The first line names the tar (`gnu`, `bsd`,
/// `other`): GNU tar and bsdtar escape names, BusyBox prints them raw.
/// GNU and BusyBox print `perm owner/group size date time name`, bsdtar
/// prints `ls -l` columns.
pub fn parse_tar_listing(out: &[u8], now: i64) -> Vec<ArchiveMember> {
    let mut lines = out.split(|b| *b == b'\n');
    let escaped = !matches!(lines.next(), Some(b"other"));
    lines
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let (head, _) = split_fields(line, 2)?;
            let gnu_style = head[1].contains(&b'/');
            let (fields, rest) = split_fields(line, if gnu_style { 5 } else { 8 })?;
            let (kind, mode) = parse_perm(fields[0]);
            let (size, mtime) = if gnu_style {
                (num(fields[2]), iso_time(fields[3], fields[4]))
            } else {
                (num(fields[4]), ls_time(fields[5], fields[6], fields[7], now))
            };
            let (name, target) = match (kind, fields[0].first()) {
                (EntryKind::Symlink, _) => split_link(rest, b" -> "),
                (_, Some(b'h')) => split_link(rest, b" link to "),
                _ => (rest.to_vec(), None),
            };
            let fix = |b: Vec<u8>| if escaped { unescape(&b) } else { b };
            let mut member = ArchiveMember::new(fix(name), kind);
            member.size = size.unwrap_or(0).max(0) as u64;
            member.mtime = mtime;
            member.mode = mode;
            member.link_target = target.map(fix);
            Some(member)
        })
        .collect()
}

/// The end of a zip archive large enough to hold its end record with the
/// longest comment and the zip64 records before it.
const ZIP_TAIL: usize = 22 + 65_535 + 20 + 56;

/// Where the central directory is, from the archive's tail.
#[derive(Debug, PartialEq, Eq)]
struct CentralDirectory {
    offset: u64,
    size: u64,
}

fn le16(b: &[u8], at: usize) -> u64 {
    u16::from_le_bytes([b[at], b[at + 1]]) as u64
}

fn le32(b: &[u8], at: usize) -> u64 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as u64
}

fn le64(b: &[u8], at: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(bytes)
}

/// Finds the end record in `tail` (the last bytes of the file, starting at
/// `tail_start`), following zip64 records when the sizes overflow.
fn find_central_directory(tail: &[u8], tail_start: u64) -> Option<CentralDirectory> {
    let eocd = (0..=tail.len().checked_sub(22)?)
        .rev()
        .find(|&i| tail[i..i + 4] == [0x50, 0x4b, 0x05, 0x06] && i + 22 + le16(tail, i + 20) as usize == tail.len())?;
    let (mut size, mut offset) = (le32(tail, eocd + 12), le32(tail, eocd + 16));
    // The directory ends where the records after it begin.
    let mut end = tail_start + eocd as u64;
    if size == 0xFFFF_FFFF || offset == 0xFFFF_FFFF || le16(tail, eocd + 10) == 0xFFFF {
        let locator = eocd.checked_sub(20)?;
        let record = locator.checked_sub(56)?;
        if tail[locator..locator + 4] != [0x50, 0x4b, 0x06, 0x07] || tail[record..record + 4] != [0x50, 0x4b, 0x06, 0x06] {
            return None;
        }
        size = le64(tail, record + 40);
        offset = le64(tail, record + 48);
        end = tail_start + record as u64;
    }
    // Data in front of the archive (self-extractors) shifts every offset.
    let shift = end.checked_sub(offset.checked_add(size)?)?;
    Some(CentralDirectory {
        offset: offset + shift,
        size,
    })
}

/// A DOS date and time, read as this machine's local time like the local
/// zip support does.
fn dos_time(date: u64, time: u64) -> Option<i64> {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = (date >> 9) as i32 + 80;
    tm.tm_mon = ((date >> 5) & 0xF) as i32 - 1;
    tm.tm_mday = (date & 0x1F) as i32;
    tm.tm_hour = (time >> 11) as i32;
    tm.tm_min = ((time >> 5) & 0x3F) as i32;
    tm.tm_sec = ((time & 0x1F) * 2) as i32;
    tm.tm_isdst = -1;
    if tm.tm_mday == 0 || tm.tm_mon < 0 {
        return None;
    }
    let t = unsafe { libc::mktime(&mut tm) };
    (t != -1).then_some(t as i64)
}

/// The members listed in a zip central directory.
fn parse_central_directory(cd: &[u8]) -> Vec<ArchiveMember> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 46 <= cd.len() && cd[i..i + 4] == [0x50, 0x4b, 0x01, 0x02] {
        let unix_made = cd[i + 5] == 3;
        let mut size = le32(cd, i + 24);
        let (name_len, extra_len, comment_len) = (le16(cd, i + 28) as usize, le16(cd, i + 30) as usize, le16(cd, i + 32) as usize);
        let attrs = (le32(cd, i + 38) >> 16) as u32;
        let name_start = i + 46;
        let extra_start = name_start + name_len;
        let next = extra_start + extra_len + comment_len;
        if next > cd.len() {
            break;
        }
        let mut mtime = dos_time(le16(cd, i + 14), le16(cd, i + 12));
        let extra = &cd[extra_start..extra_start + extra_len];
        let mut j = 0;
        while j + 4 <= extra.len() {
            let (id, len) = (le16(extra, j), le16(extra, j + 2) as usize);
            let data = &extra[j + 4..(j + 4 + len).min(extra.len())];
            match id {
                // zip64: the real size comes first when it overflowed.
                0x0001 if size == 0xFFFF_FFFF && data.len() >= 8 => size = le64(data, 0),
                // Extended timestamp: Unix modification time.
                0x5455 if data.len() >= 5 && data[0] & 1 != 0 => mtime = Some(le32(data, 1) as i64),
                _ => {}
            }
            j += 4 + len;
        }
        let name = cd[name_start..extra_start].to_vec();
        let kind = if name.ends_with(b"/") {
            EntryKind::Dir
        } else {
            match attrs & 0o170000 {
                0o120000 if unix_made => EntryKind::Symlink,
                0o040000 if unix_made => EntryKind::Dir,
                _ => EntryKind::File,
            }
        };
        let mut member = ArchiveMember::new(name, kind);
        member.size = if kind == EntryKind::File { size } else { 0 };
        member.mtime = mtime;
        member.mode = if unix_made { attrs & 0o7777 } else { 0 };
        out.push(member);
        i = next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_792_000_000; // 2026-10-14

    #[test]
    fn gnu_listing() {
        let out = b"gnu\n\
drwxr-xr-x 0/0               0 2026-10-15 16:43:03 ./proj/\n\
-rw-r--r-- 0/0               1 2026-10-15 16:43:03 ./proj/\\303\\274n\\\\x.txt\n\
-rw-r--r-- 1000/1000         6 2020-01-02 02:04:05 ./proj/sp ace.txt\n\
lrwxrwxrwx 0/0               0 2026-10-15 16:43:03 ./proj/link -> sub\n\
hrw-r--r-- 0/0               0 2026-10-15 16:43:03 ./proj/hard link to ./proj/sp ace.txt\n\
-rwsr-x--- 0/0              10 2026-10-15 16:43:03 ./bin\n\
crw-rw-rw- 0/0             1,3 2026-10-15 16:43:03 ./dev/null\n";
        let m = parse_tar_listing(out, NOW);
        assert_eq!(m.len(), 7);
        assert_eq!(m[0].kind, EntryKind::Dir);
        assert_eq!(m[0].raw, b"./proj/");
        assert_eq!(m[1].raw, "./proj/ün\\x.txt".as_bytes());
        assert_eq!(m[2].raw, b"./proj/sp ace.txt");
        assert_eq!(m[2].size, 6);
        assert_eq!(m[2].mtime, Some(1_577_930_645));
        assert_eq!(m[2].mode, 0o644);
        assert_eq!((m[3].kind, m[3].link_target.as_deref()), (EntryKind::Symlink, Some(&b"sub"[..])));
        assert_eq!((m[4].kind, m[4].raw.as_slice()), (EntryKind::File, &b"./proj/hard"[..]));
        assert_eq!(m[5].mode, 0o4750);
        assert_eq!((m[6].kind, m[6].size), (EntryKind::CharDevice, 0));
    }

    #[test]
    fn busybox_and_bsdtar_listings() {
        let busybox = "other\n-rw-r--r-- root/root         1 2026-10-15 16:43:03 ./proj/ün\\x.txt\n".as_bytes();
        let m = parse_tar_listing(busybox, NOW);
        assert_eq!(m[0].raw, "./proj/ün\\x.txt".as_bytes(), "raw names stay raw");

        let bsd = b"bsd\n\
drwxr-xr-x  0 f1nnix wheel       0 Oct 15 16:41 ./proj/\n\
-rw-r--r--  0 f1nnix wheel       6 Jan  2  2020 ./proj/README\n\
lrwxr-xr-x  0 f1nnix wheel       0 Oct 15 16:41 ./proj/link -> sub\n\
-rw-r--r--  0 f1nnix wheel       1 Oct 15 16:41 ./proj/u\\314\\210n\\\\x.txt\n";
        let m = parse_tar_listing(bsd, NOW);
        assert_eq!(m.len(), 4);
        assert_eq!(m[1].mtime, Some(1_577_923_200));
        assert_eq!(m[1].size, 6);
        assert_eq!(m[0].mtime, Some(1_792_082_460), "this year");
        assert_eq!(m[2].link_target.as_deref(), Some(&b"sub"[..]));
        assert_eq!(m[3].raw, "./proj/u\u{308}n\\x.txt".as_bytes());
        // December, seen in January, is last December.
        assert_eq!(ls_time(b"Dec", b"30", b"10:00", 1_767_312_000), epoch(2025, 12, 30, 10, 0, 0));
    }

    fn zip_bytes(large: bool, prefix: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(prefix.to_vec()));
        let opts = zip::write::SimpleFileOptions::default()
            .unix_permissions(0o640)
            .large_file(large)
            .last_modified_time(zip::DateTime::from_date_and_time(2024, 5, 6, 7, 8, 10).unwrap());
        zip.add_directory("proj/", opts).unwrap();
        zip.start_file("proj/€_日本.txt", opts).unwrap();
        zip.write_all(b"hello").unwrap();
        zip.add_symlink("proj/link", "€_日本.txt", opts).unwrap();
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn zip_central_directories() {
        for (large, prefix) in [(false, &b""[..]), (true, &b""[..]), (false, &b"#!/bin/sh\nexit 0\n"[..])] {
            let bytes = zip_bytes(large, prefix);
            // As fetched: the whole (small) file is the tail.
            let dir = find_central_directory(&bytes, 0).unwrap();
            let cd = &bytes[dir.offset as usize..(dir.offset + dir.size) as usize];
            let m = parse_central_directory(cd);
            assert_eq!(m.len(), 3, "large={large}");
            assert_eq!((m[0].raw.as_slice(), m[0].kind), (&b"proj/"[..], EntryKind::Dir));
            assert_eq!((m[1].raw.as_slice(), m[1].size, m[1].mode), ("proj/€_日本.txt".as_bytes(), 5, 0o640));
            assert_eq!(m[1].mtime, dos_time((44 << 9) | (5 << 5) | 6, (7 << 11) | (8 << 5) | 5));
            assert_eq!(m[2].kind, EntryKind::Symlink);
            // A tail that starts after the directory points at it.
            let start = bytes.len() as u64 - 30;
            let far = find_central_directory(&bytes[start as usize..], start).unwrap();
            assert_eq!(far, dir);
        }
        assert!(find_central_directory(b"garbage", 0).is_none());
    }

    #[test]
    fn escapes_and_names() {
        assert_eq!(unescape(br"a\\b\nc\303\274\q"), b"a\\b\nc\xc3\xbc\\q");
        assert_eq!(tar_escape(b"a*b?[c]\\"), b"a\\*b\\?\\[c]\\\\");
        assert_eq!(zip_escape(b"a*b?[c]"), b"a[*]b[?][[]c]");
        assert_eq!(year_of(0), 1970);
        assert_eq!(year_of(1_577_930_645), 2020);
        assert_eq!(days_from_civil(2020, 1, 2) * 86_400, 1_577_923_200);
    }

    #[test]
    fn stored_names_follow_the_archive() {
        let archive = RemoteArchive {
            remote: Arc::new(Remote::new(
                "ssh".into(),
                "/nonexistent".into(),
                "host".into(),
                "host".into(),
                "root".into(),
                Arc::new(tl_core::prompt::DenyPrompter),
            )),
            path: VPath::parse("/a.tar"),
            format: ArchiveFormat::Tar,
            tar_p: true,
            stored: Mutex::new(HashMap::new()),
        };
        archive.remember(&[
            ArchiveMember::new("./proj/src/main.rs", EntryKind::File),
            ArchiveMember::new("./proj/", EntryKind::Dir),
            ArchiveMember::new("top", EntryKind::File),
        ]);
        assert_eq!(archive.stored_name(b"proj"), (b"./proj".to_vec(), true));
        assert_eq!(archive.stored_name(b"proj/src"), (b"./proj/src".to_vec(), true), "implied folder");
        assert_eq!(archive.stored_name(b"proj/src/main.rs"), (b"./proj/src/main.rs".to_vec(), false));
        assert_eq!(archive.stored_name(b"top"), (b"top".to_vec(), false));
    }
}
