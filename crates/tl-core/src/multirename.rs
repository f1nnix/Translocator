//! Multi-rename: new names for a batch of files from masks with
//! placeholders, a counter, find & replace (plain or regex) and case
//! conversion, checked for clashes and applied in a safe order.
//!
//! Placeholders (case-sensitive): `[N]` name without extension, `[E]`
//! extension, `[N2-5]` / `[N3,2]` / `[N-3-]` parts of them (characters,
//! 1-based, negative counts from the end), `[C]` counter (`[C10+5:3]`
//! overrides start, step and digits), `[P]` parent folder, `[YMD]`, `[hms]`,
//! `[Y]` `[M]` `[D]` `[h]` `[m]` `[s]` modification time. `[[` and `]]`
//! are literal brackets.

use std::collections::{HashMap, HashSet};

use regex::{NoExpand, Regex, RegexBuilder};

use crate::cancel::CancelToken;
use crate::entry::split_extension;
use crate::error::{VfsError, VfsResult};
use crate::path::VPath;
use crate::progress::Progress;
use crate::util::name_key;
use crate::vfs::{OpCtx, Vfs};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaseChange {
    #[default]
    Keep,
    Lower,
    Upper,
    /// First letter upper case, the rest lower case.
    FirstUpper,
    /// Every word starts with an upper-case letter.
    Title,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameSpec {
    pub name_mask: String,
    pub ext_mask: String,
    pub find: String,
    pub replace: String,
    /// `find` is a regular expression; `replace` may use `$1`.
    pub regex: bool,
    pub match_case: bool,
    pub case: CaseChange,
    pub counter_start: i64,
    pub counter_step: i64,
    /// Minimum digits, zero-padded.
    pub counter_width: usize,
}

impl Default for RenameSpec {
    fn default() -> Self {
        RenameSpec {
            name_mask: "[N]".into(),
            ext_mask: "[E]".into(),
            find: String::new(),
            replace: String::new(),
            regex: false,
            match_case: false,
            case: CaseChange::Keep,
            counter_start: 1,
            counter_step: 1,
            counter_width: 1,
        }
    }
}

/// A character range of `[N…]` / `[E…]`; positions are 1-based, negative
/// ones count from the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slice {
    All,
    /// From `start` to `end` inclusive; `None` = to the end.
    Range(i64, Option<i64>),
    /// `len` characters from `start`.
    Len(i64, usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DatePart {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Text(String),
    Name(Slice),
    Ext(Slice),
    Counter {
        start: Option<i64>,
        step: Option<i64>,
        width: Option<usize>,
    },
    Parent,
    Date(Vec<DatePart>),
}

fn parse_int(s: &str) -> Option<(i64, &str)> {
    let digits_start = usize::from(s.starts_with('-'));
    let end = s[digits_start..]
        .find(|c: char| !c.is_ascii_digit())
        .map_or(s.len(), |i| i + digits_start);
    if end == digits_start {
        return None;
    }
    Some((s[..end].parse().ok()?, &s[end..]))
}

fn parse_slice(spec: &str) -> Option<Slice> {
    if spec.is_empty() {
        return Some(Slice::All);
    }
    let (start, rest) = parse_int(spec)?;
    if start == 0 {
        return None;
    }
    if rest.is_empty() {
        return Some(Slice::Range(start, Some(start)));
    }
    if let Some(len) = rest.strip_prefix(',') {
        return Some(Slice::Len(start, len.parse().ok()?));
    }
    let rest = rest.strip_prefix('-')?;
    if rest.is_empty() {
        return Some(Slice::Range(start, None));
    }
    let (end, tail) = parse_int(rest)?;
    (end != 0 && tail.is_empty()).then_some(Slice::Range(start, Some(end)))
}

fn parse_counter(spec: &str) -> Option<Token> {
    let (mut start, mut step, mut width) = (None, None, None);
    let mut rest = spec;
    let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if digits > 0 {
        start = Some(rest[..digits].parse().ok()?);
        rest = &rest[digits..];
    }
    if let Some(sign) = rest.chars().next().filter(|c| *c == '+' || *c == '-') {
        let tail = &rest[1..];
        let digits = tail.find(|c: char| !c.is_ascii_digit()).unwrap_or(tail.len());
        let value: i64 = tail[..digits].parse().ok()?;
        step = Some(if sign == '-' { -value } else { value });
        rest = &tail[digits..];
    }
    if let Some(w) = rest.strip_prefix(':') {
        width = Some(w.parse().ok()?);
        rest = "";
    }
    rest.is_empty().then_some(Token::Counter { start, step, width })
}

fn parse_placeholder(body: &str) -> Option<Token> {
    use DatePart::*;
    Some(match body {
        "P" => Token::Parent,
        "YMD" => Token::Date(vec![Year, Month, Day]),
        "hms" => Token::Date(vec![Hour, Minute, Second]),
        "Y" => Token::Date(vec![Year]),
        "M" => Token::Date(vec![Month]),
        "D" => Token::Date(vec![Day]),
        "h" => Token::Date(vec![Hour]),
        "m" => Token::Date(vec![Minute]),
        "s" => Token::Date(vec![Second]),
        _ => match body.split_at(1) {
            ("N", rest) => Token::Name(parse_slice(rest)?),
            ("E", rest) => Token::Ext(parse_slice(rest)?),
            ("C", rest) => parse_counter(rest)?,
            _ => return None,
        },
    })
}

fn parse_mask(mask: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut text = String::new();
    let mut rest = mask;
    while let Some(c) = rest.chars().next() {
        if rest.starts_with("[[") || rest.starts_with("]]") {
            text.push(c);
            rest = &rest[2..];
            continue;
        }
        if c != '[' {
            text.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        }
        let Some(close) = rest.find(']') else {
            return Err(format!("“{rest}” is missing a closing ]. Use [[ for a literal [."));
        };
        let body = &rest[1..close];
        let token = (!body.is_empty())
            .then(|| parse_placeholder(body))
            .flatten()
            .ok_or_else(|| format!("Unknown placeholder [{body}]"))?;
        if !text.is_empty() {
            tokens.push(Token::Text(std::mem::take(&mut text)));
        }
        tokens.push(token);
        rest = &rest[close + 1..];
    }
    if !text.is_empty() {
        tokens.push(Token::Text(text));
    }
    Ok(tokens)
}

fn slice(text: &str, slice: Slice) -> String {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len() as i64;
    // 1-based (or negative from the end) position → 0-based index.
    let index = |pos: i64| if pos > 0 { pos - 1 } else { len + pos };
    let (from, to) = match slice {
        Slice::All => return text.to_string(),
        Slice::Range(start, end) => (index(start), end.map_or(len - 1, index)),
        Slice::Len(start, n) => (index(start), index(start) + n as i64 - 1),
    };
    let (from, to) = (from.max(0), to.min(len - 1));
    if from > to {
        return String::new();
    }
    chars[from as usize..=to as usize].iter().collect()
}

/// Local calendar fields of a Unix time.
fn local_time(secs: i64) -> Option<[i64; 6]> {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return None;
    }
    Some([
        tm.tm_year as i64 + 1900,
        tm.tm_mon as i64 + 1,
        tm.tm_mday as i64,
        tm.tm_hour as i64,
        tm.tm_min as i64,
        tm.tm_sec as i64,
    ])
}

fn change_case(name: &str, case: CaseChange) -> String {
    match case {
        CaseChange::Keep => name.to_string(),
        CaseChange::Lower => name.to_lowercase(),
        CaseChange::Upper => name.to_uppercase(),
        CaseChange::FirstUpper => {
            let lower = name.to_lowercase();
            let mut chars = lower.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        }
        CaseChange::Title => {
            let mut out = String::with_capacity(name.len());
            let mut start = true;
            for c in name.chars() {
                if start {
                    out.extend(c.to_uppercase());
                } else {
                    out.extend(c.to_lowercase());
                }
                start = !c.is_alphanumeric() && c != '\'';
            }
            out
        }
    }
}

/// A file as the masks see it.
#[derive(Clone, Copy, Debug)]
pub struct RenameSource<'a> {
    pub name: &'a [u8],
    pub is_dir: bool,
    pub mtime: Option<i64>,
}

/// A compiled [`RenameSpec`].
pub struct Renamer {
    name: Vec<Token>,
    ext: Vec<Token>,
    find: Option<Regex>,
    replace: String,
    expand: bool,
    case: CaseChange,
    counter: (i64, i64, usize),
}

impl Renamer {
    pub fn new(spec: &RenameSpec) -> Result<Self, String> {
        let find = if spec.find.is_empty() {
            None
        } else {
            let pattern = if spec.regex { spec.find.clone() } else { regex::escape(&spec.find) };
            Some(
                RegexBuilder::new(&pattern)
                    .case_insensitive(!spec.match_case)
                    .build()
                    .map_err(|e| format!("Invalid regular expression: {e}"))?,
            )
        };
        Ok(Renamer {
            name: parse_mask(&spec.name_mask)?,
            ext: parse_mask(&spec.ext_mask)?,
            find,
            replace: spec.replace.clone(),
            expand: spec.regex,
            case: spec.case,
            counter: (spec.counter_start, spec.counter_step, spec.counter_width),
        })
    }

    fn expand(&self, tokens: &[Token], index: usize, stem: &str, ext: &str, file: &RenameSource, parent: &str) -> String {
        let mut out = String::new();
        let time = file.mtime.and_then(local_time);
        for token in tokens {
            match token {
                Token::Text(text) => out.push_str(text),
                Token::Name(s) => out.push_str(&slice(stem, *s)),
                Token::Ext(s) => out.push_str(&slice(ext, *s)),
                Token::Parent => out.push_str(parent),
                Token::Counter { start, step, width } => {
                    let value = start.unwrap_or(self.counter.0) + index as i64 * step.unwrap_or(self.counter.1);
                    let width = width.unwrap_or(self.counter.2);
                    if value < 0 {
                        out.push_str(&format!("-{:0width$}", -value));
                    } else {
                        out.push_str(&format!("{value:0width$}"));
                    }
                }
                Token::Date(parts) => {
                    if let Some(t) = time {
                        for part in parts {
                            let (value, width) = match part {
                                DatePart::Year => (t[0], 4),
                                DatePart::Month => (t[1], 2),
                                DatePart::Day => (t[2], 2),
                                DatePart::Hour => (t[3], 2),
                                DatePart::Minute => (t[4], 2),
                                DatePart::Second => (t[5], 2),
                            };
                            out.push_str(&format!("{value:0width$}"));
                        }
                    }
                }
            }
        }
        out
    }

    /// The new name of the `index`-th file (0-based) in a folder named `parent`.
    pub fn rename(&self, index: usize, file: &RenameSource, parent: &str) -> String {
        let full = String::from_utf8_lossy(file.name);
        let (stem, ext) = if file.is_dir {
            (full.to_string(), String::new())
        } else {
            let (s, e) = split_extension(file.name);
            (String::from_utf8_lossy(s).into_owned(), e.map(|e| String::from_utf8_lossy(e).into_owned()).unwrap_or_default())
        };
        let mut name = self.expand(&self.name, index, &stem, &ext, file, parent);
        let ext = self.expand(&self.ext, index, &stem, &ext, file, parent);
        if !ext.is_empty() {
            name.push('.');
            name.push_str(&ext);
        }
        if let Some(find) = &self.find {
            name = if self.expand {
                find.replace_all(&name, self.replace.as_str()).into_owned()
            } else {
                find.replace_all(&name, NoExpand(&self.replace)).into_owned()
            };
        }
        change_case(&name, self.case)
    }
}

/// One line of the preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub old: Vec<u8>,
    pub new: Vec<u8>,
    /// Why this file cannot get its new name.
    pub problem: Option<String>,
}

impl Preview {
    pub fn changed(&self) -> bool {
        self.old != self.new
    }
}

/// New names for `files` (in this order), checked against each other and
/// against `existing` (every name in the folder). Fails only when the
/// spec itself is invalid (a bad mask or regex).
pub fn preview(
    spec: &RenameSpec,
    files: &[RenameSource],
    parent: &str,
    existing: &[Vec<u8>],
    case_insensitive: bool,
) -> Result<Vec<Preview>, String> {
    let renamer = Renamer::new(spec)?;
    let mut out: Vec<Preview> = files
        .iter()
        .enumerate()
        .map(|(i, file)| {
            let new = renamer.rename(i, file, parent);
            let problem = if new.is_empty() {
                Some("The new name is empty".to_string())
            } else if new.contains('/') || new.contains('\0') {
                Some("A name cannot contain “/”".to_string())
            } else if new == "." || new == ".." {
                Some("Not a valid name".to_string())
            } else if new.len() > 255 {
                Some("Longer than 255 bytes".to_string())
            } else if std::str::from_utf8(file.name).is_err() && new != String::from_utf8_lossy(file.name) {
                Some("The current name is not valid UTF-8".to_string())
            } else {
                None
            };
            let new = if std::str::from_utf8(file.name).is_err() && new == String::from_utf8_lossy(file.name) {
                file.name.to_vec()
            } else {
                new.into_bytes()
            };
            Preview {
                old: file.name.to_vec(),
                new,
                problem,
            }
        })
        .collect();

    let key = |n: &[u8]| name_key(n, case_insensitive);
    let existing: HashSet<Vec<u8>> = existing.iter().map(|n| key(n)).collect();
    // Files with a problem keep their name, which can clash with others in
    // turn: repeat until nothing changes.
    loop {
        let final_name = |p: &Preview| if p.problem.is_none() { p.new.clone() } else { p.old.clone() };
        let mut finals: HashMap<Vec<u8>, usize> = HashMap::new();
        for p in &out {
            *finals.entry(key(&final_name(p))).or_insert(0) += 1;
        }
        let leaving: HashSet<Vec<u8>> = out
            .iter()
            .filter(|p| p.problem.is_none() && p.changed())
            .map(|p| key(&p.old))
            .collect();
        let mut flagged = false;
        for p in out.iter_mut().filter(|p| p.problem.is_none() && p.changed()) {
            let k = key(&p.new);
            if finals.get(&k).copied().unwrap_or(0) > 1 {
                p.problem = Some("Another file would get the same name".into());
                flagged = true;
            } else if existing.contains(&k) && !leaving.contains(&k) {
                p.problem = Some(format!("“{}” already exists", String::from_utf8_lossy(&p.new)));
                flagged = true;
            }
        }
        if !flagged {
            return Ok(out);
        }
    }
}

/// One rename: old name, new name.
pub type Rename = (Vec<u8>, Vec<u8>);

/// Orders renames so none lands on a name still in use: chains go
/// back to front, cycles (`a → b`, `b → a`) and case-only changes on
/// case-insensitive file systems go through a temporary name. Fails if
/// two files would get the same name or a target belongs to a file that
/// is not renamed.
pub fn plan_steps(renames: &[Rename], existing: &[Vec<u8>], case_insensitive: bool) -> Result<Vec<Rename>, String> {
    let key = |n: &[u8]| name_key(n, case_insensitive);
    let mut occupied: HashSet<Vec<u8>> = existing.iter().map(|n| key(n)).collect();
    let mut pending: Vec<Rename> = renames.iter().filter(|(o, n)| o != n).cloned().collect();
    let sources: HashSet<Vec<u8>> = pending.iter().map(|(o, _)| key(o)).collect();
    let mut targets = HashSet::new();
    for (old, new) in &pending {
        let k = key(new);
        if !targets.insert(k.clone()) {
            return Err(format!("Two files would be named “{}”", String::from_utf8_lossy(new)));
        }
        if occupied.contains(&k) && !sources.contains(&k) && k != key(old) {
            return Err(format!("“{}” already exists", String::from_utf8_lossy(new)));
        }
    }
    occupied.extend(sources);
    let mut serial = 0;
    let mut temp_name = |occupied: &HashSet<Vec<u8>>, old: &[u8]| loop {
        serial += 1;
        let mut name = format!(".tl-rename-{serial}-").into_bytes();
        name.extend_from_slice(&old[..old.len().min(200)]);
        if !occupied.contains(&key(&name)) {
            return name;
        }
    };
    let mut steps = Vec::new();
    while !pending.is_empty() {
        let mut progressed = false;
        let mut rest = Vec::new();
        for (old, new) in pending {
            if key(&old) == key(&new) {
                // Case-only change where case does not count.
                let tmp = temp_name(&occupied, &old);
                steps.push((old.clone(), tmp.clone()));
                steps.push((tmp, new));
                progressed = true;
            } else if !occupied.contains(&key(&new)) {
                occupied.remove(&key(&old));
                occupied.insert(key(&new));
                steps.push((old, new));
                progressed = true;
            } else {
                rest.push((old, new));
            }
        }
        if !progressed {
            // Every remaining target is taken by another pending file: a
            // cycle. Move one file out of the way.
            let (old, new) = rest.remove(0);
            let tmp = temp_name(&occupied, &old);
            occupied.remove(&key(&old));
            occupied.insert(key(&tmp));
            steps.push((old, tmp.clone()));
            rest.push((tmp, new));
        }
        pending = rest;
    }
    Ok(steps)
}

/// Performs `steps` in `dir`. If one fails (or the job is cancelled), the
/// renames already done are undone, so the folder is left as it was.
pub fn apply(vfs: &dyn Vfs, dir: &VPath, steps: &[Rename], ctx: &OpCtx, progress: &Progress) -> VfsResult<()> {
    progress.set_totals(0, steps.len() as u64);
    let paths: Vec<(VPath, VPath)> = steps.iter().map(|(a, b)| (dir.child(a), dir.child(b))).collect();
    let mut done = 0;
    let result = vfs.rename_all(&paths, ctx, &mut done);
    progress.set_done(0, done as u64);
    let Err(err) = result else { return Ok(()) };
    let undo: Vec<(VPath, VPath)> = paths[..done.min(paths.len())]
        .iter()
        .rev()
        .map(|(from, to)| (to.clone(), from.clone()))
        .collect();
    let undo_ctx = OpCtx {
        cancel: CancelToken::new(),
        ..ctx.clone()
    };
    let mut undone = 0;
    match vfs.rename_all(&undo, &undo_ctx, &mut undone) {
        Ok(()) => Err(err),
        Err(undo_err) => Err(VfsError::other(format!(
            "{err}\nUndoing the {done} renames already done failed too: {undo_err}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> RenameSource<'_> {
        RenameSource {
            name: name.as_bytes(),
            is_dir: false,
            mtime: Some(1_700_000_000),
        }
    }

    fn names(spec: &RenameSpec, files: &[&str]) -> Vec<String> {
        let renamer = Renamer::new(spec).unwrap();
        files
            .iter()
            .enumerate()
            .map(|(i, f)| renamer.rename(i, &file(f), "photos"))
            .collect()
    }

    fn spec(name: &str, ext: &str) -> RenameSpec {
        RenameSpec {
            name_mask: name.into(),
            ext_mask: ext.into(),
            ..Default::default()
        }
    }

    #[test]
    fn masks_and_counters() {
        assert_eq!(names(&RenameSpec::default(), &["a.txt", "b"]), ["a.txt", "b"]);
        assert_eq!(
            names(&RenameSpec { counter_width: 3, ..spec("[P]_[C]", "[E]") }, &["x.JPG", "y.jpg"]),
            ["photos_001.JPG", "photos_002.jpg"]
        );
        assert_eq!(names(&spec("[C10+5:2]-[N]", ""), &["a.txt", "b.txt"]), ["10-a", "15-b"]);
        assert_eq!(names(&spec("[N2-4]", "[E1]"), &["abcdef.txt"]), ["bcd.t"]);
        assert_eq!(names(&spec("[N-3-]", "[E]"), &["abcdef.txt"]), ["def.txt"]);
        assert_eq!(names(&spec("[N2,2]|[N-1]", "[E]"), &["abcdef.txt"]), ["bc|f.txt"]);
        assert_eq!(names(&spec("[N2--2]", ""), &["abcdef"]), ["bcde"]);
        assert_eq!(names(&spec("[N9-]x", "[E]"), &["abc.md"]), ["x.md"]);
        assert_eq!(names(&spec("[[[N]]]", "[E]"), &["a.b"]), ["[a].b"]);
        assert_eq!(names(&spec("[N]", "[E]"), &["archive.tar.gz"]), ["archive.tar.gz"]);
        let dated = names(&spec("[YMD]-[hms]", ""), &["a"]);
        assert_eq!(dated[0].len(), 15, "{dated:?}");
        assert_eq!(names(&spec("[Y]", ""), &["a"])[0], dated[0][..4]);
        // Folders have no extension.
        let r = Renamer::new(&spec("[N]-[E]", "[E]")).unwrap();
        let folder = RenameSource { name: b"v1.2", is_dir: true, mtime: None };
        assert_eq!(r.rename(0, &folder, "p"), "v1.2-");
    }

    #[test]
    fn bad_masks() {
        assert!(Renamer::new(&spec("[X]", "")).err().unwrap().contains("[X]"));
        assert!(Renamer::new(&spec("[N", "")).is_err());
        assert!(Renamer::new(&spec("[N0]", "")).is_err());
        assert!(Renamer::new(&spec("[C+x]", "")).is_err());
        assert!(Renamer::new(&RenameSpec { find: "(".into(), regex: true, ..Default::default() }).is_err());
    }

    #[test]
    fn find_replace_and_case() {
        let plain = RenameSpec {
            find: "IMG".into(),
            replace: "Photo $1".into(),
            ..Default::default()
        };
        assert_eq!(names(&plain, &["img_01.JPG"]), ["Photo $1_01.JPG"]);
        let exact = RenameSpec { match_case: true, ..plain.clone() };
        assert_eq!(names(&exact, &["img_01.JPG"]), ["img_01.JPG"]);
        let regex = RenameSpec {
            find: r"^(\w+)_(\d+)".into(),
            replace: "${2}-$1".into(),
            regex: true,
            ..Default::default()
        };
        assert_eq!(names(&regex, &["img_01.JPG"]), ["01-img.JPG"]);
        let lower = RenameSpec { case: CaseChange::Lower, ..Default::default() };
        assert_eq!(names(&lower, &["IMG_01.JPG"]), ["img_01.jpg"]);
        assert_eq!(change_case("hello wORLD-foo's bar", CaseChange::Title), "Hello World-Foo's Bar");
        assert_eq!(change_case("hELLO", CaseChange::FirstUpper), "Hello");
        assert_eq!(change_case("ß", CaseChange::Upper), "SS");
    }

    #[test]
    fn preview_flags_clashes() {
        let files = [file("a.txt"), file("b.txt"), file("c.txt")];
        let existing: Vec<Vec<u8>> = ["a.txt", "b.txt", "c.txt", "x.txt"].iter().map(|n| n.as_bytes().to_vec()).collect();
        // Everything to the same name.
        let p = preview(&spec("x", "[E]"), &files, "p", &existing, false).unwrap();
        assert!(p.iter().all(|p| p.problem.is_some()), "{p:?}");
        // A shift onto names that are being vacated is fine.
        let numbered = [file("1.txt"), file("2.txt")];
        let taken: Vec<Vec<u8>> = vec![b"1.txt".to_vec(), b"2.txt".to_vec()];
        let shift = RenameSpec { counter_start: 2, ..spec("[C]", "[E]") };
        let p = preview(&shift, &numbered, "p", &taken, false).unwrap();
        assert_eq!(p.iter().map(|p| String::from_utf8_lossy(&p.new).into_owned()).collect::<Vec<_>>(), ["2.txt", "3.txt"]);
        assert!(p.iter().all(|p| p.problem.is_none() && p.changed()), "{p:?}");
        // Upper-casing onto an unchanged sibling, case-insensitively.
        let files2 = [file("a.txt"), file("A.TXT")];
        let p = preview(&RenameSpec { case: CaseChange::Upper, ..Default::default() }, &files2, "p", &existing, true).unwrap();
        assert_eq!(p[0].problem.as_deref(), Some("Another file would get the same name"));
        assert!(p[1].problem.is_none() && !p[1].changed());
        // Clashes with a file outside the batch.
        let p = preview(&spec("x", "txt"), &[file("a.txt")], "p", &existing, false).unwrap();
        assert_eq!(p[0].problem.as_deref(), Some("“x.txt” already exists"));
        // Empty and slash.
        let p = preview(&spec("", ""), &[file("a.txt")], "p", &existing, false).unwrap();
        assert!(p[0].problem.is_some());
        let p = preview(&RenameSpec { find: "a".into(), replace: "a/b".into(), ..Default::default() }, &[file("a.txt")], "p", &existing, false).unwrap();
        assert!(p[0].problem.as_deref().unwrap().contains('/'));
    }

    #[test]
    fn non_utf8_names_are_left_alone() {
        let raw = [0xff, b'.', b't', b'x', b't'];
        let f = RenameSource { name: &raw, is_dir: false, mtime: None };
        let p = preview(&RenameSpec::default(), &[f], "p", &[raw.to_vec()], false).unwrap();
        assert!(!p[0].changed() && p[0].problem.is_none());
        let p = preview(&spec("[N]x", "[E]"), &[f], "p", &[raw.to_vec()], false).unwrap();
        assert!(p[0].problem.is_some());
    }

    fn b(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    /// Simulates `steps` on a set of names; fails on any collision.
    fn simulate(names: &[&str], steps: &[Rename], ci: bool) -> Vec<String> {
        let key = |n: &[u8]| name_key(n, ci);
        let mut set: Vec<Vec<u8>> = names.iter().map(|n| b(n)).collect();
        for (from, to) in steps {
            let pos = set.iter().position(|n| n == from).expect("source exists");
            assert!(!set.iter().any(|n| key(n) == key(to)), "{to:?} taken");
            set[pos] = to.clone();
        }
        let mut out: Vec<String> = set.iter().map(|n| String::from_utf8_lossy(n).into_owned()).collect();
        out.sort();
        out
    }

    #[test]
    fn steps_handle_chains_cycles_and_case() {
        let names = ["1", "2", "3", "other"];
        let existing: Vec<Vec<u8>> = names.iter().map(|n| b(n)).collect();
        // Chain 1→2→3→4: must go back to front.
        let chain = [(b("1"), b("2")), (b("2"), b("3")), (b("3"), b("4"))];
        let steps = plan_steps(&chain, &existing, false).unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(simulate(&names, &steps, false), ["2", "3", "4", "other"]);
        // Swap 1↔2 needs a temporary name.
        let swap = [(b("1"), b("2")), (b("2"), b("1"))];
        let steps = plan_steps(&swap, &existing, false).unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(simulate(&names, &steps, false), ["1", "2", "3", "other"]);
        // Case-only change on a case-insensitive file system.
        let steps = plan_steps(&[(b("other"), b("OTHER"))], &existing, true).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(simulate(&names, &steps, true), ["1", "2", "3", "OTHER"]);
        // Unchanged names produce no steps.
        assert!(plan_steps(&[(b("1"), b("1"))], &existing, false).unwrap().is_empty());
        // Targets taken by files outside the batch, or claimed twice.
        assert!(plan_steps(&[(b("1"), b("other"))], &existing, false).is_err());
        assert!(plan_steps(&[(b("1"), b("9")), (b("2"), b("9"))], &existing, false).is_err());
    }
}
