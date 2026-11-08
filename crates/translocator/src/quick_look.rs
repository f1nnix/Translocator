//! Quick look: the pane you are not working in shows what the cursor is on.
//!
//! ⌘Y turns it on and off. While it is on the file list keeps the keyboard
//! — moving, marking and walking into folders all behave as they always do
//! — and this view follows the cursor: text with its syntax colours, images
//! as images, and for everything else what the listing already knows.
//!
//! The cursor moves faster than a file can be read, above all over a
//! connection, so a read starts only once the cursor has sat still for a
//! moment, and the one before it is dropped.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{prelude::*, *};
use tl_core::util::human_size;
use tl_core::{Entry, EntryKind, OpCtx, VPath, Vfs, VfsResult};

use crate::pane::FilePane;
use crate::util::{blocking, entry_icon, format_date_long, language_for};

/// How long the cursor has to sit still before the file is read: holding a
/// cursor key down must not start a read per row.
const SETTLE: Duration = Duration::from_millis(150);
/// How much of a text file is read and shown.
const TEXT_LIMIT: u64 = 256 * 1024;
/// An image larger than this is named rather than fetched; a preview is not
/// worth pulling tens of megabytes over a connection.
const IMAGE_LIMIT: u64 = 8 * 1024 * 1024;
/// A NUL in this much of the beginning means the file is not text.
const SNIFF: usize = 8 * 1024;

/// The file under a pane's cursor.
#[derive(Clone)]
struct Target {
    vfs: Arc<dyn Vfs>,
    dir: VPath,
    entry: Entry,
    elevated: bool,
}

impl Target {
    fn of(pane: &FilePane) -> Option<Target> {
        let row = pane.cursor_row()?;
        if row.parent {
            return None;
        }
        let loc = pane.location();
        Some(Target {
            vfs: loc.vfs.clone(),
            dir: loc.path.clone(),
            entry: row.entry.clone(),
            elevated: pane.elevated(),
        })
    }

    fn path(&self) -> VPath {
        self.dir.child(&self.entry.name)
    }
}

/// Two targets are the same file when nothing that would change what is
/// shown has changed: an edit elsewhere still re-reads it.
impl PartialEq for Target {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.vfs, &other.vfs)
            && self.dir == other.dir
            && self.entry.name == other.entry.name
            && self.entry.size == other.entry.size
            && self.entry.mtime == other.entry.mtime
            && self.elevated == other.elevated
    }
}

/// What is worth reading, decided from the listing alone.
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

/// The formats GPUI can decode, by extension.
fn image_format(name: &[u8]) -> Option<ImageFormat> {
    let extension = tl_core::entry::split_extension(name).1?;
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

/// What a finished read produced.
enum Loaded {
    Note(&'static str),
    Image(Arc<Image>),
    /// The text, and how much of the file it is.
    Text(String, u64),
}

/// What the view is showing.
enum Body {
    /// No file under the cursor: an empty folder, or the row above it.
    Nothing,
    Reading,
    Note(SharedString),
    Image(Arc<Image>),
    Text {
        state: Entity<EditorState>,
        /// Set when the file is longer than what was read.
        shown: Option<SharedString>,
    },
    Error(SharedString),
}

pub struct QuickLook {
    /// The pane whose cursor this follows. It changes when the panes swap,
    /// so the preview is always opposite the file list being worked in.
    source: Option<Entity<FilePane>>,
    /// What is on screen, so an unchanged cursor does not read again.
    showing: Option<Target>,
    body: Body,
    /// The wait and the read. Dropping it abandons both.
    task: Option<Task<()>>,
    _watch: Option<Subscription>,
}

impl QuickLook {
    pub fn new() -> Self {
        QuickLook {
            source: None,
            showing: None,
            body: Body::Nothing,
            task: None,
            _watch: None,
        }
    }

    /// Follows `pane`'s cursor from now on.
    pub fn watch(&mut self, pane: Entity<FilePane>, window: &mut Window, cx: &mut Context<Self>) {
        if self.source.as_ref().is_some_and(|p| p.entity_id() == pane.entity_id()) {
            return;
        }
        // Any change to a pane redraws it, so observing is enough to catch
        // every cursor move without the pane knowing this view exists.
        self._watch = Some(cx.observe_in(&pane, window, |this, _, window, cx| this.follow(window, cx)));
        self.source = Some(pane);
        // Cleared together: a cursor on the row above a folder has nothing
        // to show, and `follow` would take that for "no change" and leave
        // the other pane's file on screen.
        self.showing = None;
        self.body = Body::Nothing;
        self.task = None;
        self.follow(window, cx);
    }

    /// Reads the file under the cursor, unless it is the one already shown.
    fn follow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.source.clone() else { return };
        let target = Target::of(pane.read(cx));
        if target.as_ref() == self.showing.as_ref() {
            return;
        }
        self.showing = target.clone();
        cx.notify();
        let Some(target) = target else {
            self.body = Body::Nothing;
            self.task = None;
            return;
        };
        // What the listing alone can answer is answered at once; only a
        // file that has to be read waits.
        let (limit, format) = match plan_for(&target.entry) {
            Plan::Note(note) => {
                self.body = Body::Note(note.into());
                self.task = None;
                return;
            }
            Plan::Image(format) => (IMAGE_LIMIT, Some(format)),
            Plan::Text => (TEXT_LIMIT, None),
        };
        self.body = Body::Reading;
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            let loaded = load(&target, limit, format).await;
            let _ = this.update_in(cx, |this, window, cx| this.loaded(&target, loaded, window, cx));
        }));
    }

    /// Puts a finished read on screen, unless the cursor has moved on.
    fn loaded(
        &mut self,
        target: &Target,
        loaded: VfsResult<Loaded>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.showing.as_ref() != Some(target) {
            return;
        }
        self.body = match loaded {
            Ok(Loaded::Note(note)) => Body::Note(note.into()),
            Ok(Loaded::Image(image)) => Body::Image(image),
            Ok(Loaded::Text(text, read)) => {
                let language = language_for(&target.entry.name_lossy());
                let state = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(language)
                        .line_number(true)
                        .soft_wrap(false)
                        .default_value(text)
                });
                let size = target.entry.size;
                Body::Text {
                    state,
                    shown: (read < size).then(|| {
                        format!("first {} of {}", human_size(read), human_size(size)).into()
                    }),
                }
            }
            Err(err) => Body::Error(err.to_string().into()),
        };
        cx.notify();
    }

    fn render_header(&self, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let hint: SharedString = match crate::keymap::shortcut("quick-look", cx) {
            Some(keys) => format!("Quick Look · {keys}").into(),
            None => "Quick Look".into(),
        };
        let entry = self.showing.as_ref().map(|t| t.entry.clone());
        h_flex()
            .h(px(32.))
            .flex_shrink_0()
            .px_2()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .child(Icon::new(entry.as_ref().map_or(Lucide::Eye, entry_icon)).small().text_color(theme.muted_foreground))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .child(entry.map(|e| e.name_lossy()).unwrap_or_default()),
            )
            .child(div().flex_shrink_0().text_xs().text_color(theme.muted_foreground).child(hint))
    }

    fn render_body(&self, cx: &App) -> AnyElement {
        let theme = cx.theme().clone();
        let middle = |text: SharedString, danger: bool| {
            v_flex()
                .size_full()
                .justify_center()
                .items_center()
                .text_sm()
                .text_color(if danger { theme.danger } else { theme.muted_foreground })
                .child(text)
                .into_any_element()
        };
        match &self.body {
            Body::Nothing => middle("Nothing to preview".into(), false),
            Body::Reading => middle("Reading…".into(), false),
            Body::Note(note) => middle(note.clone(), false),
            Body::Error(err) => middle(err.clone(), true),
            Body::Image(image) => div()
                .size_full()
                .p_2()
                .child(img(image.clone()).size_full())
                .into_any_element(),
            Body::Text { state, .. } => div()
                .size_full()
                .child(Editor::new(state).h_full().readonly(true).bordered(false))
                .into_any_element(),
        }
    }

    fn render_footer(&self, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let shown = match &self.body {
            Body::Text { shown, .. } => shown.clone(),
            _ => None,
        };
        let details: SharedString = match self.showing.as_ref() {
            Some(target) => {
                let entry = &target.entry;
                let kind = if entry.is_dir() {
                    "folder".to_string()
                } else {
                    human_size(entry.size)
                };
                let owner = match (&entry.user, &entry.group) {
                    (Some(user), Some(group)) => format!(" · {user}:{group}"),
                    _ => String::new(),
                };
                format!(
                    "{kind} · {}{owner} · {}",
                    format_date_long(entry.mtime),
                    entry.mode_string()
                )
                .into()
            }
            None => SharedString::default(),
        };
        h_flex()
            .h(px(22.))
            .flex_shrink_0()
            .px_2()
            .gap_2()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.status_bar)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().flex_1().min_w_0().truncate().child(details))
            .children(shown.map(|shown| div().flex_shrink_0().child(shown)))
    }
}

/// Reads at most `limit` bytes of the file and makes something of them.
/// The read runs on a worker thread, so a slow server never blocks the
/// window, and whoever dropped the task has stopped waiting for it.
async fn load(target: &Target, limit: u64, format: Option<ImageFormat>) -> VfsResult<Loaded> {
    let (vfs, path, elevated) = (target.vfs.clone(), target.path(), target.elevated);
    let bytes = blocking(move || vfs.read_file(&path, limit, &OpCtx::new().elevated(elevated))).await?;
    if let Some(format) = format {
        return Ok(Loaded::Image(Arc::new(Image::from_bytes(format, bytes))));
    }
    if bytes.iter().take(SNIFF).any(|byte| *byte == 0) {
        return Ok(Loaded::Note("Binary file"));
    }
    let read = bytes.len() as u64;
    Ok(Loaded::Text(String::from_utf8_lossy(&bytes).into_owned(), read))
}

/// What the view is showing, for the tests.
#[cfg(test)]
#[derive(Debug, PartialEq)]
pub(crate) enum Preview {
    Nothing,
    Reading,
    Note(String),
    /// The size of the image that was read.
    Image(usize),
    Text(String),
    Error(String),
}

#[cfg(test)]
impl QuickLook {
    pub(crate) fn preview(&self, cx: &App) -> Preview {
        match &self.body {
            Body::Nothing => Preview::Nothing,
            Body::Reading => Preview::Reading,
            Body::Note(note) => Preview::Note(note.to_string()),
            Body::Image(image) => Preview::Image(image.bytes.len()),
            Body::Text { state, .. } => Preview::Text(state.read(cx).value().to_string()),
            Body::Error(err) => Preview::Error(err.to_string()),
        }
    }

    /// The name of the file being previewed.
    pub(crate) fn preview_name(&self) -> Option<String> {
        self.showing.as_ref().map(|target| target.entry.name_lossy())
    }
}

impl Render for QuickLook {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(self.render_header(cx))
            .child(div().flex_1().min_h_0().child(self.render_body(cx)))
            .child(self.render_footer(cx))
    }
}
