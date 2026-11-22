//! Quick look: the pane you are not working in shows what the cursor is on.
//!
//! ⌘Y turns it on and off. While it is on the file list keeps the keyboard
//! — moving, marking and walking into folders all behave as they always do
//! — and the preview follows the cursor of the active pane: text (with its
//! language, for the shell to colour), images (as bytes and a format, for
//! the shell to decode), and for everything else what the listing already
//! knows.
//!
//! The cursor moves faster than a file can be read, above all over a
//! connection, so a read starts only once the cursor has sat still for
//! [`SETTLE_MS`]. Every change of target bumps a generation; a wait or a
//! read that comes back with an older one is dropped, so only the last
//! position is ever read.

use std::sync::Arc;

use crux_core::Command;
use tl_model::util::human_size;
use tl_model::{Entry, EntryKind, VPath, VfsResult};

use crate::capabilities::*;
use crate::editor::language_for;
use crate::ids::ConnId;
use crate::model::{Model, Reply, shell};
use crate::pane::Side;
use crate::{Cmd, Out};

/// How long the cursor has to sit still before the file is read: holding a
/// cursor key down must not start a read per row.
pub const SETTLE_MS: u64 = 150;
/// How much of a text file is read and shown.
pub const TEXT_LIMIT: u64 = 256 * 1024;
/// An image larger than this is named rather than fetched; a preview is not
/// worth pulling tens of megabytes over a connection.
pub const IMAGE_LIMIT: u64 = 8 * 1024 * 1024;
/// A NUL in this much of the beginning means the file is not text.
const SNIFF: usize = 8 * 1024;

/// The file under the active pane's cursor.
#[derive(Clone, Debug)]
pub struct Target {
    pub conn: ConnId,
    pub dir: VPath,
    pub entry: Entry,
    pub elevated: bool,
}

impl Target {
    fn of(model: &Model) -> Option<Target> {
        let pane = model.pane(model.active);
        let entry = pane.tab().cursor_entry()?;
        Some(Target {
            conn: pane.loc().conn,
            dir: pane.loc().path.clone(),
            entry: entry.clone(),
            elevated: pane.elevated(),
        })
    }

    pub fn path(&self) -> VPath {
        self.dir.child(&self.entry.name)
    }
}

/// Two targets are the same file when nothing that would change what is
/// shown has changed: an edit elsewhere still re-reads it.
impl PartialEq for Target {
    fn eq(&self, other: &Self) -> bool {
        self.conn == other.conn
            && self.dir == other.dir
            && self.entry.name == other.entry.name
            && self.entry.size == other.entry.size
            && self.entry.mtime == other.entry.mtime
            && self.elevated == other.elevated
    }
}

/// The image formats the shell decodes, by extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
    Gif,
    Svg,
    Bmp,
    Tiff,
    Ico,
    Pnm,
}

pub fn image_format(name: &[u8]) -> Option<ImageFormat> {
    let extension = tl_model::entry::split_extension(name).1?;
    match String::from_utf8_lossy(extension).to_ascii_lowercase().as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "webp" => Some(ImageFormat::Webp),
        "gif" => Some(ImageFormat::Gif),
        "svg" => Some(ImageFormat::Svg),
        "bmp" => Some(ImageFormat::Bmp),
        "tif" | "tiff" => Some(ImageFormat::Tiff),
        "ico" => Some(ImageFormat::Ico),
        "pnm" | "pbm" | "pgm" | "ppm" | "pam" => Some(ImageFormat::Pnm),
        _ => None,
    }
}

/// What is worth reading, decided from the listing alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Plan {
    /// Nothing to read: the listing already says all there is to say.
    Note(&'static str),
    Image(ImageFormat),
    Text,
}

fn plan_for(entry: &Entry) -> Plan {
    // A symlink is previewed as whatever it points at, which is what
    // reading it would give.
    let kind = if entry.is_symlink() { entry.target_kind } else { Some(entry.kind) };
    match kind {
        None => Plan::Note("Broken link"),
        Some(EntryKind::Dir) => Plan::Note("Folder"),
        Some(EntryKind::File) => {
            if entry.size == 0 {
                return Plan::Note("Empty file");
            }
            match image_format(&entry.name) {
                Some(_) if entry.size > IMAGE_LIMIT => Plan::Note("Image too large to preview"),
                Some(format) => Plan::Image(format),
                None => Plan::Text,
            }
        }
        Some(_) => Plan::Note("Not a regular file"),
    }
}

/// What the preview is showing.
#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    /// No file under the cursor: an empty folder, or the row above it.
    Nothing,
    Reading,
    Note(String),
    /// Bytes for the shell to decode.
    Image { format: ImageFormat, bytes: Arc<Vec<u8>> },
    /// Read-only text with line numbers and no soft wrap.
    Text {
        text: Arc<str>,
        language: &'static str,
        /// "first 256 KB of 3 MB" when the file is longer than what was read.
        shown: Option<String>,
    },
    /// Drawn in the danger colour.
    Error(String),
}

impl Body {
    /// The line drawn in the middle of the preview, and whether it is an
    /// error; `None` for an image or text.
    pub fn message(&self) -> Option<(&str, bool)> {
        match self {
            Body::Nothing => Some(("Nothing to preview", false)),
            Body::Reading => Some(("Reading…", false)),
            Body::Note(note) => Some((note, false)),
            Body::Error(err) => Some((err, true)),
            Body::Image { .. } | Body::Text { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct QuickLook {
    /// The pane whose cursor this follows: the active one when last looked.
    source: Side,
    /// What is on screen, so an unchanged cursor does not read again.
    pub showing: Option<Target>,
    pub body: Body,
    /// Bumped on every change of target; older answers are dropped.
    pub generation: u64,
    /// Bumped on every change of `body`.
    pub revision: u64,
}

impl QuickLook {
    fn new(source: Side) -> Self {
        QuickLook {
            source,
            showing: None,
            body: Body::Nothing,
            generation: 0,
            revision: 0,
        }
    }

    fn set_body(&mut self, body: Body) {
        self.body = body;
        self.revision += 1;
    }
}

#[derive(Debug)]
pub enum QuickLookReply {
    /// The cursor has sat still since `generation` began.
    Settled { generation: u64 },
    Read { generation: u64, out: Box<FsOutput> },
}

/// Turns quick look on or off.
pub fn toggle(model: &mut Model, _out: &mut Out) {
    model.quick_look = match model.quick_look.take() {
        Some(_) => None,
        None => Some(QuickLook::new(model.active)),
    };
}

/// Runs after every update: points the preview at the cursor of the pane
/// with the keyboard, and starts the wait before a read when that changed.
pub fn follow(model: &mut Model) -> Cmd {
    let target = Target::of(model);
    let active = model.active;
    let Some(ql) = model.quick_look.as_mut() else { return Command::done() };
    if ql.source != active {
        // Cleared together: a cursor on the row above a folder has nothing
        // to show, and would pass for "no change" below, leaving the other
        // pane's file on screen.
        ql.source = active;
        ql.showing = None;
        ql.set_body(Body::Nothing);
        ql.generation += 1;
    }
    if target == ql.showing {
        return Command::done();
    }
    ql.generation += 1;
    ql.showing = target.clone();
    let Some(target) = target else {
        ql.set_body(Body::Nothing);
        return Command::done();
    };
    // What the listing alone can answer is answered at once; only a file
    // that has to be read waits.
    if let Plan::Note(note) = plan_for(&target.entry) {
        ql.set_body(Body::Note(note.into()));
        return Command::done();
    }
    ql.set_body(Body::Reading);
    let generation = ql.generation;
    Command::request_from_shell(SleepOp(SETTLE_MS))
        .then_send(move |()| shell(Reply::QuickLook(QuickLookReply::Settled { generation })))
}

pub fn reply(model: &mut Model, reply: QuickLookReply, out: &mut Out) {
    let Some(ql) = model.quick_look.as_mut() else { return };
    match reply {
        QuickLookReply::Settled { generation } => {
            if generation != ql.generation {
                return;
            }
            let Some(target) = ql.showing.clone() else { return };
            let limit = match plan_for(&target.entry) {
                Plan::Image(_) => IMAGE_LIMIT,
                Plan::Text => TEXT_LIMIT,
                Plan::Note(_) => return,
            };
            let op = FsOp::Read {
                conn: target.conn,
                path: target.path(),
                limit,
                elevated: target.elevated,
            };
            out.fs(op, move |out| Reply::QuickLook(QuickLookReply::Read { generation, out: Box::new(out) }));
        }
        QuickLookReply::Read { generation, out } => {
            if generation != ql.generation {
                return;
            }
            let Some(target) = &ql.showing else { return };
            let FsOutput::Read(result) = *out else { return };
            let body = loaded(target, result);
            ql.set_body(body);
        }
    }
}

/// Makes something of a finished read.
fn loaded(target: &Target, result: VfsResult<(Vec<u8>, u64)>) -> Body {
    let bytes = match result {
        Ok((bytes, _)) => bytes,
        Err(err) => return Body::Error(err.to_string()),
    };
    if let Plan::Image(format) = plan_for(&target.entry) {
        return Body::Image {
            format,
            bytes: Arc::new(bytes),
        };
    }
    if bytes.iter().take(SNIFF).any(|byte| *byte == 0) {
        return Body::Note("Binary file".into());
    }
    let read = bytes.len() as u64;
    let size = target.entry.size;
    Body::Text {
        text: Arc::from(String::from_utf8_lossy(&bytes).into_owned()),
        language: language_for(&target.entry.name_lossy()),
        shown: (read < size).then(|| format!("first {} of {}", human_size(read), human_size(size))),
    }
}

/// The details under the preview.
#[derive(Clone, Debug, PartialEq)]
pub struct Footer {
    /// "folder", or the size.
    pub kind: String,
    /// For the shell to write as a long local date.
    pub mtime: Option<i64>,
    /// `user:group`, when both are known.
    pub owner: Option<String>,
    /// `-rw-r--r--`.
    pub mode: String,
}

impl Footer {
    /// `{kind} · {date} · user:group · {mode}`, given the date as written.
    pub fn text(&self, date: &str) -> String {
        let owner = self.owner.as_ref().map(|o| format!(" · {o}")).unwrap_or_default();
        format!("{} · {date}{owner} · {}", self.kind, self.mode)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct QuickLookView {
    /// Drawn in place of this pane: the one opposite the keyboard.
    pub side: Side,
    /// The file previewed, for the header's icon and name; `None` shows the
    /// eye icon and no name.
    pub entry: Option<Entry>,
    pub name: String,
    pub body: Body,
    /// Changes whenever the body changes, so the shell knows when to load
    /// new text into its widget.
    pub revision: u64,
    pub footer: Option<Footer>,
    /// The right of the footer: how much of a long text is shown.
    pub shown: Option<String>,
}

pub fn view(model: &Model) -> Option<QuickLookView> {
    let ql = model.quick_look.as_ref()?;
    let entry = ql.showing.as_ref().map(|t| t.entry.clone());
    let footer = entry.as_ref().map(|entry| Footer {
        kind: if entry.is_dir() { "folder".into() } else { human_size(entry.size) },
        mtime: entry.mtime,
        owner: match (&entry.user, &entry.group) {
            (Some(user), Some(group)) => Some(format!("{user}:{group}")),
            _ => None,
        },
        mode: entry.mode_string(),
    });
    let shown = match &ql.body {
        Body::Text { shown, .. } => shown.clone(),
        _ => None,
    };
    Some(QuickLookView {
        side: model.active.other(),
        name: entry.as_ref().map(Entry::name_lossy).unwrap_or_default(),
        entry,
        body: ql.body.clone(),
        revision: ql.revision,
        footer,
        shown,
    })
}

#[cfg(test)]
mod tests {
    use crux_core::{App as _, Request};
    use tl_model::{Capabilities, Entry, EntryKind, VfsError};

    use super::*;
    use crate::conn::ConnInfo;
    use crate::ids::LOCAL;
    use crate::model::Command as Key;
    use crate::pane::{Listing, PARENT};
    use crate::{Effect, Event, Translocator};

    struct Harness {
        model: Model,
        cmds: Vec<Cmd>,
    }

    impl Harness {
        fn new() -> Self {
            let mut model = Model {
                ready: true,
                ..Model::default()
            };
            model.conns.insert(ConnInfo {
                id: LOCAL,
                plugin: "local".into(),
                label: "Local".into(),
                location_key: "local".into(),
                caps: Capabilities::default(),
                home: VPath::root(),
                bookmark: None,
                host: None,
                server_info: None,
                local_home: None,
                machine: None,
            });
            Harness { model, cmds: Vec::new() }
        }

        fn send(&mut self, event: Event) -> Vec<Effect> {
            let mut cmd = Translocator.update(event, &mut self.model);
            let effects = cmd.effects().filter(|e| !e.is_render()).collect();
            self.cmds.push(cmd);
            effects
        }

        fn key(&mut self, key: Key) -> Vec<Effect> {
            self.send(Event::Command(key))
        }

        fn resolve<Op: crux_core::capability::Operation>(
            &mut self,
            mut req: Request<Op>,
            output: Op::Output,
        ) -> Vec<Effect> {
            req.resolve(output).unwrap();
            let events: Vec<Event> = self.cmds.iter_mut().flat_map(|c| c.events().collect::<Vec<_>>()).collect();
            events.into_iter().flat_map(|e| self.send(e)).collect()
        }

        /// Shows `entries` in `side`'s folder `dir`, with ".." first and
        /// the cursor on row `cursor`.
        fn list(&mut self, side: Side, dir: &str, entries: Vec<Entry>, cursor: usize) {
            let tab = self.model.pane_mut(side).tab_mut();
            tab.loc.path = VPath::parse(dir);
            let mut order = vec![PARENT];
            order.extend(0..entries.len() as u32);
            tab.order = Arc::new(order);
            tab.listing = Listing::new(entries);
            tab.cursor = cursor;
        }

        fn body(&self) -> Body {
            view(&self.model).unwrap().body
        }

        fn name(&self) -> String {
            view(&self.model).unwrap().name
        }
    }

    fn file(name: &str, size: u64) -> Entry {
        let mut entry = Entry::new(name, EntryKind::File);
        entry.size = size;
        entry.mtime = Some(1_700_000_000);
        entry.mode = 0o100644;
        entry
    }

    fn dir(name: &str) -> Entry {
        let mut entry = Entry::new(name, EntryKind::Dir);
        entry.mode = 0o040755;
        entry
    }

    fn sleep(effects: Vec<Effect>) -> Request<SleepOp> {
        let mut sleeps: Vec<_> = effects.into_iter().filter_map(Effect::into_sleep).collect();
        assert_eq!(sleeps.len(), 1, "{sleeps:?}");
        let req = sleeps.pop().unwrap();
        assert_eq!(req.operation, SleepOp(SETTLE_MS));
        req
    }

    fn read(effects: Vec<Effect>) -> Request<FsOp> {
        let mut reads: Vec<_> = effects.into_iter().filter_map(Effect::into_fs).collect();
        assert_eq!(reads.len(), 1, "{reads:?}");
        reads.pop().unwrap()
    }

    fn text(s: &str) -> Body {
        Body::Text {
            text: Arc::from(s),
            language: "text",
            shown: None,
        }
    }

    /// Waits out the settle time and answers the read with `data`.
    fn settle_and_read(h: &mut Harness, effects: Vec<Effect>, data: &[u8]) -> FsOp {
        let req = read(h.resolve(sleep(effects), ()));
        let op = req.operation.clone();
        let size = data.len() as u64;
        h.resolve(req, FsOutput::Read(Ok((data.to_vec(), size))));
        op
    }

    #[test]
    fn plans_come_from_the_listing() {
        assert_eq!(plan_for(&dir("d")), Plan::Note("Folder"));
        assert_eq!(plan_for(&file("e.txt", 0)), Plan::Note("Empty file"));
        assert_eq!(plan_for(&file("a.txt", 1)), Plan::Text);
        assert_eq!(plan_for(&file("a.PNG", 1)), Plan::Image(ImageFormat::Png));
        assert_eq!(plan_for(&file("a.png", IMAGE_LIMIT + 1)), Plan::Note("Image too large to preview"));
        assert_eq!(plan_for(&Entry::new("fifo", EntryKind::Fifo)), Plan::Note("Not a regular file"));
        assert_eq!(plan_for(&Entry::new("gone", EntryKind::Symlink)), Plan::Note("Broken link"));
        let mut link = Entry::new("link", EntryKind::Symlink);
        link.target_kind = Some(EntryKind::Dir);
        assert_eq!(plan_for(&link), Plan::Note("Folder"));
        for (name, format) in [
            ("a.jpg", ImageFormat::Jpeg),
            ("a.jpeg", ImageFormat::Jpeg),
            ("a.webp", ImageFormat::Webp),
            ("a.gif", ImageFormat::Gif),
            ("a.svg", ImageFormat::Svg),
            ("a.bmp", ImageFormat::Bmp),
            ("a.tif", ImageFormat::Tiff),
            ("a.tiff", ImageFormat::Tiff),
            ("a.ico", ImageFormat::Ico),
            ("a.pnm", ImageFormat::Pnm),
            ("a.pbm", ImageFormat::Pnm),
            ("a.pgm", ImageFormat::Pnm),
            ("a.ppm", ImageFormat::Pnm),
            ("a.pam", ImageFormat::Pnm),
        ] {
            assert_eq!(image_format(name.as_bytes()), Some(format), "{name}");
        }
        assert_eq!(image_format(b"png"), None);
    }

    #[test]
    fn quick_look_previews_what_the_cursor_is_on() {
        let mut h = Harness::new();
        h.list(Side::Left, "/w", vec![file("a.txt", 5), file("bin.dat", 9), dir("sub")], 1);
        assert!(view(&h.model).is_none());

        let effects = h.key(Key::ToggleQuickLook);
        let v = view(&h.model).unwrap();
        assert_eq!(v.side, Side::Right);
        assert_eq!(v.name, "a.txt");
        assert_eq!(v.body, Body::Reading);
        let op = settle_and_read(&mut h, effects, b"hello");
        assert_eq!(
            op,
            FsOp::Read {
                conn: LOCAL,
                path: VPath::parse("/w/a.txt"),
                limit: TEXT_LIMIT,
                elevated: false
            }
        );
        assert_eq!(h.body(), text("hello"));
        let footer = view(&h.model).unwrap().footer.unwrap();
        assert_eq!(footer.text("14 Nov 2023"), "5 B · 14 Nov 2023 · -rw-r--r--");

        let effects = h.key(Key::CursorDown);
        settle_and_read(&mut h, effects, b"head\0tail");
        assert_eq!(h.body(), Body::Note("Binary file".into()));

        // A folder is answered from the listing, without a read.
        let effects = h.key(Key::CursorDown);
        assert!(effects.is_empty(), "{} effects", effects.len());
        assert_eq!(h.body(), Body::Note("Folder".into()));
        assert_eq!(view(&h.model).unwrap().footer.unwrap().kind, "folder");

        // Entering a folder keeps quick look on; ".." shows nothing.
        h.list(Side::Left, "/w/sub", vec![file("inner.txt", 5)], 0);
        h.send(Event::Focus(Side::Left));
        assert_eq!(h.body(), Body::Nothing);
        assert_eq!(view(&h.model).unwrap().footer, None);
        let effects = h.key(Key::CursorDown);
        settle_and_read(&mut h, effects, b"inner");
        assert_eq!(h.body(), text("inner"));

        h.key(Key::ToggleQuickLook);
        assert!(view(&h.model).is_none());
    }

    #[test]
    fn quick_look_moves_to_the_other_side_with_the_keyboard() {
        let mut h = Harness::new();
        h.list(Side::Left, "/l", vec![file("left.txt", 4)], 1);
        h.list(Side::Right, "/r", vec![file("right.txt", 5)], 0);
        let effects = h.key(Key::ToggleQuickLook);
        settle_and_read(&mut h, effects, b"left");
        assert_eq!(h.body(), text("left"));

        h.send(Event::Focus(Side::Right));
        let v = view(&h.model).unwrap();
        assert_eq!(v.side, Side::Left);
        assert_eq!(v.body, Body::Nothing, "not the stale left file");
        assert_eq!(v.name, "");

        let effects = h.key(Key::CursorDown);
        assert_eq!(h.name(), "right.txt");
        settle_and_read(&mut h, effects, b"right");
        assert_eq!(h.body(), text("right"));

        h.key(Key::ToggleMark);
        assert!(h.model.pane(Side::Right).tab().marked.contains(&b"right.txt".to_vec()));
    }

    #[test]
    fn quick_look_reads_an_image_as_an_image() {
        let mut h = Harness::new();
        h.list(Side::Left, "/i", vec![file("dot.png", 4), file("empty.txt", 0)], 1);
        let effects = h.key(Key::ToggleQuickLook);
        let op = settle_and_read(&mut h, effects, b"\x89PNG");
        assert!(matches!(op, FsOp::Read { limit: IMAGE_LIMIT, .. }));
        assert_eq!(
            h.body(),
            Body::Image {
                format: ImageFormat::Png,
                bytes: Arc::new(b"\x89PNG".to_vec())
            }
        );
        h.key(Key::CursorDown);
        assert_eq!(h.body(), Body::Note("Empty file".into()));
    }

    #[test]
    fn only_the_position_the_cursor_rests_on_is_read() {
        let mut h = Harness::new();
        h.list(Side::Left, "/s", vec![file("a.rs", 3), file("b.rs", 3), file("c.rs", 3)], 1);
        let first = sleep(h.key(Key::ToggleQuickLook));
        let second = sleep(h.key(Key::CursorDown));
        // The first wait ends after the cursor moved on: no read.
        assert!(h.resolve(first, ()).is_empty());
        let req = read(h.resolve(second, ()));
        assert!(matches!(&req.operation, FsOp::Read { path, .. } if *path == VPath::parse("/s/b.rs")));
        // The cursor moves while that read runs: its answer is dropped.
        let third = sleep(h.key(Key::CursorDown));
        h.resolve(req, FsOutput::Read(Ok((b"bbb".to_vec(), 3))));
        assert_eq!(h.body(), Body::Reading);
        assert_eq!(h.name(), "c.rs");
        let req = read(h.resolve(third, ()));
        h.resolve(req, FsOutput::Read(Ok((b"ccc".to_vec(), 3))));
        assert_eq!(
            h.body(),
            Body::Text {
                text: Arc::from("ccc"),
                language: "rust",
                shown: None
            }
        );
        // An unchanged cursor reads nothing again.
        assert!(h.send(Event::Focus(Side::Left)).is_empty());
    }

    #[test]
    fn long_texts_and_failures() {
        let mut h = Harness::new();
        let mut long = file("long.txt", 10);
        long.user = Some("me".into());
        long.group = Some("staff".into());
        h.list(Side::Left, "/t", vec![long, file("locked.txt", 3)], 1);
        let effects = h.key(Key::ToggleQuickLook);
        let req = read(h.resolve(sleep(effects), ()));
        h.resolve(req, FsOutput::Read(Ok((b"abcd".to_vec(), 10))));
        let v = view(&h.model).unwrap();
        assert_eq!(v.shown.as_deref(), Some("first 4 B of 10 B"));
        assert_eq!(v.footer.unwrap().text("today"), "10 B · today · me:staff · -rw-r--r--");

        let effects = h.key(Key::CursorDown);
        let req = read(h.resolve(sleep(effects), ()));
        h.resolve(req, FsOutput::Read(Err(VfsError::PermissionDenied("/t/locked.txt".into()))));
        assert_eq!(h.body(), Body::Error("Permission denied: /t/locked.txt".into()));
        assert_eq!(h.body().message(), Some(("Permission denied: /t/locked.txt", true)));
    }
}
