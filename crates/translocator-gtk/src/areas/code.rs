//! The text view of the viewer, the editor and quick look: a GtkSourceView
//! in the system's monospace font, with line numbers, no wrapping, a
//! search bar (Ctrl+F, Ctrl+G, Shift+Ctrl+G), undo, and the colours of
//! the core's highlighter (`tl_ffi::highlight`) as text tags.
//!
//! The text is replaced only by [`CodeView::load`] (the editor's
//! `content_version`, quick look's `revision`); after that the view owns
//! it. GtkSourceView's own highlighting is off: counting lines and
//! colouring run off the main thread, again a moment after the typing
//! pauses, and the tags go on in small batches on idle, so an 8 MB file
//! opens without freezing the window. Tags move with the text, so what
//! was coloured stays right while the next colouring is computed.
//!
//! The colours come from the content scheme in effect
//! ([`set_content_theme`], called by `crate::theme`): a scheme's ANSI
//! colours over its canvas, or for "Default" a libadwaita-like light and
//! dark palette over GtkSourceView's Adwaita schemes.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use sourceview5::prelude::*;
use tl_ffi::highlight::{HighlightKind, HighlightSpan};
use tl_ffi::themes::ThemePalette;

/// The kinds painted, in tag order. Operators stay plain.
const KINDS: [HighlightKind; 17] = [
    HighlightKind::Keyword,
    HighlightKind::String,
    HighlightKind::Escape,
    HighlightKind::Number,
    HighlightKind::Constant,
    HighlightKind::Comment,
    HighlightKind::Function,
    HighlightKind::Type,
    HighlightKind::Property,
    HighlightKind::Punctuation,
    HighlightKind::Tag,
    HighlightKind::Attribute,
    HighlightKind::Heading,
    HighlightKind::Link,
    HighlightKind::Literal,
    HighlightKind::Inserted,
    HighlightKind::Deleted,
];

fn kind_index(kind: HighlightKind) -> Option<usize> {
    KINDS.iter().position(|k| *k == kind)
}

/// Spans painted per idle turn.
const PAINT_BATCH: usize = 1500;
/// How long the typing must pause before the text is counted and coloured again.
const DEBOUNCE: Duration = Duration::from_millis(300);

// ----- the colours ---------------------------------------------------------

/// What the code views are drawn in.
#[derive(Clone, PartialEq)]
struct Look {
    /// The GtkSourceView style scheme: canvas, gutter, selection, matches.
    scheme: String,
    /// A colour for each of [`KINDS`].
    kinds: [String; 17],
}

impl Look {
    fn new(palette: Option<&ThemePalette>, dark: bool) -> Look {
        match palette {
            Some(p) => {
                let fg = p.foreground.as_str();
                let punct = blend(fg, &p.background, 0.7);
                Look {
                    scheme: scheme_for(p),
                    kinds: [
                        &p.magenta, &p.green, &p.cyan, &p.yellow, &p.yellow, &p.muted, &p.blue, &p.cyan, &p.red,
                        &punct, &p.red, &p.yellow, &p.blue, &p.cyan, &p.green, &p.green, &p.red,
                    ]
                    .map(|c| c.to_string()),
                }
            }
            None => {
                // The GNOME palette, darker shades on light and lighter on dark.
                let kinds: [&str; 17] = if dark {
                    [
                        "#dc8add", "#8ff0a4", "#f9f06b", "#ffa348", "#ffa348", "#9a9996", "#99c1f1", "#5bc8af",
                        "#f66151", "#c0bfbc", "#62a0ea", "#f9f06b", "#99c1f1", "#62a0ea", "#8ff0a4", "#8ff0a4",
                        "#f66151",
                    ]
                } else {
                    [
                        "#8a3ea8", "#1d7f4e", "#865e3c", "#b04c00", "#b04c00", "#6e6d72", "#1a5fb4", "#0d7377",
                        "#a51d2d", "#5e5c64", "#1a5fb4", "#8f5902", "#1a5fb4", "#1c71d8", "#1d7f4e", "#1d7f4e",
                        "#c01c28",
                    ]
                };
                Look {
                    scheme: if dark { "Adwaita-dark" } else { "Adwaita" }.to_string(),
                    kinds: kinds.map(str::to_string),
                }
            }
        }
    }
}

/// `#rrggbb` (or `#rrggbbaa`) as red, green, blue, alpha.
fn rgba(hex: &str) -> (f32, f32, f32, f32) {
    let digits = hex.trim_start_matches('#');
    let byte = |i: usize| {
        digits
            .get(i..i + 2)
            .and_then(|d| u8::from_str_radix(d, 16).ok())
            .map_or(0.0, |b| f32::from(b) / 255.0)
    };
    let alpha = if digits.len() >= 8 { byte(6) } else { 1.0 };
    (byte(0), byte(2), byte(4), alpha)
}

/// `color` at `amount` (times its own alpha) over `base`, as `#rrggbb`.
fn blend(color: &str, base: &str, amount: f32) -> String {
    let (r, g, b, a) = rgba(color);
    let (br, bg, bb, _) = rgba(base);
    let t = (a * amount).clamp(0.0, 1.0);
    let mix = |c: f32, base: f32| ((c * t + base * (1.0 - t)) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", mix(r, br), mix(g, bg), mix(b, bb))
}

/// A style scheme for a content scheme, written once into the cache
/// folder GtkSourceView is told to look in; its id.
fn scheme_for(p: &ThemePalette) -> String {
    let slug: String = p
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let id = format!("tl-{slug}");
    let bg = p.background.as_str();
    let solid = |c: &str, amount: f32| blend(c, bg, amount);
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<style-scheme id="{id}" name="{name}" version="1.0">
  <style name="text" foreground="{fg}" background="{bg}"/>
  <style name="selection" background="{selection}"/>
  <style name="selection-unfocused" background="{inactive}"/>
  <style name="cursor" foreground="{fg}"/>
  <style name="current-line" background="{current}"/>
  <style name="line-numbers" foreground="{muted}" background="{bg}"/>
  <style name="current-line-number" foreground="{fg}" background="{current}"/>
  <style name="bracket-match" background="{inactive}" bold="true"/>
  <style name="bracket-mismatch" foreground="{red}" underline="error"/>
  <style name="search-match" background="{search}"/>
  <style name="draw-spaces" foreground="{border}"/>
  <style name="background-pattern" background="{current}"/>
</style-scheme>
"#,
        name = glib::markup_escape_text(&p.name),
        fg = solid(&p.foreground, 1.0),
        bg = solid(bg, 1.0),
        selection = solid(&p.selection, 1.0),
        inactive = solid(&p.selection_inactive, 1.0),
        current = solid(&p.hover, 1.0),
        muted = solid(&p.muted, 1.0),
        red = solid(&p.red, 1.0),
        search = solid(&p.yellow, 0.35),
        border = solid(&p.border, 1.0),
    );
    let dir = styles_dir();
    let path = dir.join(format!("{id}.xml"));
    if std::fs::read_to_string(&path).ok().as_deref() != Some(xml.as_str()) {
        let written = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &xml));
        match written {
            Ok(()) => sourceview5::StyleSchemeManager::default().force_rescan(),
            Err(err) => log::warn!("cannot write the style scheme {}: {err}", path.display()),
        }
    }
    id
}

fn styles_dir() -> PathBuf {
    glib::user_cache_dir().join("translocator").join("gtksourceview-styles")
}

/// The look in effect, what it was made from, and every live view, to
/// repaint when it changes.
#[derive(Default)]
struct Looks {
    look: Option<Look>,
    key: Option<(String, bool)>,
    views: Vec<Weak<Inner>>,
}

thread_local! {
    static LOOK: RefCell<Looks> = RefCell::default();
}

/// The content scheme in effect changed (or was set): every code view
/// follows. `name` is the scheme for the side in effect, `dark` the
/// appearance.
pub fn set_content_theme(name: &str, dark: bool) {
    let key = (name.to_string(), dark);
    let changed = LOOK.with(|l| l.borrow().key.as_ref() != Some(&key));
    if !changed {
        return;
    }
    let look = Look::new(tl_ffi::themes::palette(name.to_string()).as_ref(), dark);
    let views: Vec<Rc<Inner>> = LOOK.with(|l| {
        let mut l = l.borrow_mut();
        l.look = Some(look.clone());
        l.key = Some(key);
        l.views.retain(|w| w.strong_count() > 0);
        l.views.iter().filter_map(Weak::upgrade).collect()
    });
    for view in views {
        view.apply_look(&look);
    }
}

fn current_look() -> Look {
    LOOK.with(|l| l.borrow().look.clone())
        .unwrap_or_else(|| Look::new(None, adw::StyleManager::default().is_dark()))
}

// ----- the view ------------------------------------------------------------

struct Inner {
    view: sourceview5::View,
    scrolled: gtk::ScrolledWindow,
    buffer: sourceview5::Buffer,
    /// One tag per painted kind, created before the search context's so
    /// matches draw over them.
    tags: Vec<gtk::TextTag>,
    look: RefCell<Option<Look>>,
    language: RefCell<String>,
    revision: Cell<Option<u64>>,
    /// Bumped by each load and edit: older colourings are dropped.
    generation: Cell<u64>,
    /// Set while `load` replaces the text, which is no edit.
    loading: Cell<bool>,
    /// The buffer holds tags from an earlier colouring.
    painted: Cell<bool>,
    debounce: RefCell<Option<glib::SourceId>>,
    on_edit: RefCell<Option<Box<dyn Fn()>>>,
    on_lines: RefCell<Option<LinesCallback>>,
}

type LinesCallback = Box<dyn Fn(usize)>;

/// A code view and its search bar.
pub struct CodeView {
    /// The search bar over the scrolled text.
    pub root: gtk::Box,
    inner: Rc<Inner>,
    _search: Rc<Search>,
}

impl CodeView {
    /// `editing`: the editor's view (current line, auto-indent); quick
    /// look and the viewer leave those off.
    pub fn new(editing: bool) -> CodeView {
        let buffer = sourceview5::Buffer::new(None::<&gtk::TextTagTable>);
        buffer.set_highlight_syntax(false);
        buffer.set_highlight_matching_brackets(editing);
        buffer.set_language(None);
        let tags = KINDS
            .iter()
            .map(|kind| {
                let tag = gtk::TextTag::new(Some(&format!("tl-{kind:?}").to_lowercase()));
                match kind {
                    HighlightKind::Comment => tag.set_style(gtk::pango::Style::Italic),
                    HighlightKind::Heading => tag.set_weight(700),
                    HighlightKind::Link => tag.set_underline(gtk::pango::Underline::Single),
                    _ => {}
                }
                buffer.tag_table().add(&tag);
                tag
            })
            .collect();

        let view = sourceview5::View::with_buffer(&buffer);
        view.set_monospace(true);
        view.set_show_line_numbers(true);
        view.set_highlight_current_line(editing);
        view.set_wrap_mode(gtk::WrapMode::None);
        view.set_tab_width(4);
        view.set_auto_indent(editing);
        view.set_smart_backspace(editing);
        view.set_left_margin(6);
        view.set_top_margin(4);
        view.set_bottom_margin(4);
        view.add_css_class("tl-code");
        view.upcast_ref::<gtk::Widget>()
            .update_property(&[gtk::accessible::Property::Label("Text")]);

        let scrolled = gtk::ScrolledWindow::builder()
            .child(&view)
            .hexpand(true)
            .vexpand(true)
            .build();

        let inner = Rc::new(Inner {
            view,
            scrolled: scrolled.clone(),
            buffer,
            tags,
            look: RefCell::new(None),
            language: RefCell::new("text".into()),
            revision: Cell::new(None),
            generation: Cell::new(0),
            loading: Cell::new(false),
            painted: Cell::new(false),
            debounce: RefCell::new(None),
            on_edit: RefCell::new(None),
            on_lines: RefCell::new(None),
        });
        inner.apply_look(&current_look());
        LOOK.with(|l| l.borrow_mut().views.push(Rc::downgrade(&inner)));

        let search = Search::new(&inner);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&search.bar);
        root.append(&scrolled);
        search.install(&root);

        let weak = Rc::downgrade(&inner);
        inner.buffer.connect_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.edited();
            }
        });
        CodeView {
            root,
            inner,
            _search: search,
        }
    }

    /// What was loaded last; `None` before the first load.
    pub fn revision(&self) -> Option<u64> {
        self.inner.revision.get()
    }

    /// Replaces the text (and forgets its undo), then counts and colours it.
    pub fn load(&self, text: String, language: &str, revision: u64) {
        // A text buffer cannot hold NUL (a binary file shown as text):
        // one visible character for one, so the offsets stay.
        let text = if text.contains('\0') { text.replace('\0', "\u{2400}") } else { text };
        let inner = &self.inner;
        inner.revision.set(Some(revision));
        inner.language.replace(language.to_string());
        inner.generation.set(inner.generation.get() + 1);
        if let Some(id) = inner.debounce.take() {
            id.remove();
        }
        inner.loading.set(true);
        let buffer = &inner.buffer;
        buffer.begin_irreversible_action();
        // Nothing to clear after this: the new text has no tags.
        buffer.set_text(&text);
        buffer.end_irreversible_action();
        buffer.set_modified(false);
        buffer.place_cursor(&buffer.start_iter());
        inner.loading.set(false);
        inner.painted.set(false);
        // Back to the top left, also when the view is not shown yet.
        inner.scrolled.hadjustment().set_value(0.0);
        inner.scrolled.vadjustment().set_value(0.0);
        analyse(inner, text);
    }

    pub fn set_editable(&self, editable: bool) {
        let view = &self.inner.view;
        if view.is_editable() != editable {
            view.set_editable(editable);
        }
    }

    /// The text as the user has it now.
    pub fn text(&self) -> String {
        let buffer = &self.inner.buffer;
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true).to_string()
    }

    /// Every edit the user makes (not a load).
    pub fn connect_edit(&self, f: impl Fn() + 'static) {
        self.inner.on_edit.replace(Some(Box::new(f)));
    }

    /// The number of lines, after a load and a moment after edits.
    pub fn connect_lines(&self, f: impl Fn(usize) + 'static) {
        self.inner.on_lines.replace(Some(Box::new(f)));
    }

    pub fn grab_focus(&self) {
        self.inner.view.grab_focus();
    }
}

impl Inner {
    fn apply_look(&self, look: &Look) {
        if self.look.borrow().as_ref() == Some(look) {
            return;
        }
        let manager = sourceview5::StyleSchemeManager::default();
        let dir = styles_dir();
        let dir = dir.to_string_lossy();
        if !manager.search_path().iter().any(|p| p.as_str() == dir) {
            manager.append_search_path(&dir);
        }
        let scheme = manager.scheme(&look.scheme).or_else(|| manager.scheme("Adwaita"));
        self.buffer.set_style_scheme(scheme.as_ref());
        for (tag, color) in self.tags.iter().zip(&look.kinds) {
            if let Ok(rgba) = gdk::RGBA::parse(color.as_str()) {
                tag.set_foreground_rgba(Some(&rgba));
            }
        }
        self.look.replace(Some(look.clone()));
    }

    /// The user changed the text: say so, and colour it again a moment
    /// after the typing pauses.
    fn edited(self: &Rc<Self>) {
        if self.loading.get() {
            return;
        }
        self.generation.set(self.generation.get() + 1);
        if let Some(f) = &*self.on_edit.borrow() {
            f();
        }
        if let Some(id) = self.debounce.take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(DEBOUNCE, move || {
            let Some(inner) = weak.upgrade() else { return };
            inner.debounce.take();
            let buffer = &inner.buffer;
            // Copied once the typing has paused, not on every keystroke.
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true).to_string();
            analyse(&inner, text);
        });
        self.debounce.replace(Some(id));
    }
}

/// Counts the lines and colours `text` on a worker thread; the result is
/// painted unless a load or an edit came first.
fn analyse(inner: &Rc<Inner>, text: String) {
    let generation = inner.generation.get();
    let language = inner.language.borrow().clone();
    let weak = Rc::downgrade(inner);
    let work = gio::spawn_blocking(move || {
        let lines = count_lines(&text);
        let spans = if language == "text" {
            Vec::new()
        } else {
            char_spans(&text, &tl_ffi::highlight::highlight(text.clone(), language))
        };
        (lines, spans)
    });
    glib::spawn_future_local(async move {
        let Ok((lines, spans)) = work.await else { return };
        let Some(inner) = weak.upgrade() else { return };
        if inner.generation.get() != generation {
            return;
        }
        if let Some(f) = &*inner.on_lines.borrow() {
            f(lines);
        }
        paint(&inner, generation, spans);
    });
}

/// Puts the tags on in batches on idle, clearing the old ones batch by
/// batch ahead of the new, so the text never flashes plain.
fn paint(inner: &Rc<Inner>, generation: u64, spans: Vec<(i32, i32, usize)>) {
    let clear = inner.painted.get();
    if spans.is_empty() && !clear {
        return;
    }
    inner.painted.set(true);
    let weak = Rc::downgrade(inner);
    let mut next = 0usize;
    let mut cleared = 0i32;
    glib::idle_add_local_full(glib::Priority::DEFAULT_IDLE, move || {
        let Some(inner) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if inner.generation.get() != generation {
            return glib::ControlFlow::Break;
        }
        let buffer = &inner.buffer;
        let started = Instant::now();
        loop {
            let end = (next + PAINT_BATCH).min(spans.len());
            let done = end == spans.len();
            if clear {
                let to = if done { buffer.char_count() } else { spans[end - 1].1 };
                if to > cleared {
                    let (a, b) = (buffer.iter_at_offset(cleared), buffer.iter_at_offset(to));
                    for tag in &inner.tags {
                        buffer.remove_tag(tag, &a, &b);
                    }
                    cleared = to;
                }
            }
            for &(start, stop, kind) in &spans[next..end] {
                let (a, b) = (buffer.iter_at_offset(start), buffer.iter_at_offset(stop));
                buffer.apply_tag(&inner.tags[kind], &a, &b);
            }
            next = end;
            if done {
                return glib::ControlFlow::Break;
            }
            if started.elapsed() > Duration::from_millis(8) {
                return glib::ControlFlow::Continue;
            }
        }
    });
}

/// `1 +` the line feeds, as the other shells count.
fn count_lines(text: &str) -> usize {
    1 + text.bytes().filter(|b| *b == b'\n').count()
}

/// The highlighter's UTF-16 spans as character offsets (what a text
/// buffer counts), with the index of their tag; operators are dropped.
fn char_spans(text: &str, spans: &[HighlightSpan]) -> Vec<(i32, i32, usize)> {
    let mut chars = text.chars();
    let (mut utf16, mut offset) = (0u32, 0i32);
    let mut advance = |to: u32| {
        while utf16 < to {
            let Some(c) = chars.next() else { break };
            utf16 += c.len_utf16() as u32;
            offset += 1;
        }
        offset
    };
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        let start = advance(span.start_utf16);
        let end = advance(span.start_utf16 + span.len_utf16);
        if let Some(kind) = kind_index(span.kind)
            && end > start
        {
            out.push((start, end, kind));
        }
    }
    out
}

// ----- search --------------------------------------------------------------

/// The search bar: Ctrl+F opens it, Enter and Ctrl+G find the next match,
/// Shift+Enter and Shift+Ctrl+G the one before, Escape closes it.
struct Search {
    bar: gtk::SearchBar,
    entry: gtk::SearchEntry,
    count: gtk::Label,
    context: sourceview5::SearchContext,
    inner: Weak<Inner>,
}

impl Search {
    fn new(inner: &Rc<Inner>) -> Rc<Search> {
        let settings = sourceview5::SearchSettings::new();
        settings.set_wrap_around(true);
        settings.set_case_sensitive(false);
        let context = sourceview5::SearchContext::new(&inner.buffer, Some(&settings));
        context.set_highlight(true);

        let entry = gtk::SearchEntry::builder()
            .placeholder_text("Find")
            .hexpand(true)
            .build();
        let count = gtk::Label::new(None);
        count.add_css_class("dim-label");
        count.add_css_class("numeric");
        let previous = gtk::Button::from_icon_name("go-up-symbolic");
        previous.set_tooltip_text(Some("Previous Match (Shift+Ctrl+G)"));
        let next = gtk::Button::from_icon_name("go-down-symbolic");
        next.set_tooltip_text(Some("Next Match (Ctrl+G)"));
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        buttons.add_css_class("linked");
        buttons.append(&previous);
        buttons.append(&next);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.set_width_request(360);
        row.append(&entry);
        row.append(&count);
        row.append(&buttons);
        let bar = gtk::SearchBar::builder().child(&row).show_close_button(true).build();
        bar.connect_entry(&entry);

        let search = Rc::new(Search {
            bar,
            entry,
            count,
            context,
            inner: Rc::downgrade(inner),
        });
        // Weak: the widgets live in the search, which the view owns.
        let weak = || Rc::downgrade(&search);
        let s = weak();
        search.entry.connect_search_changed(move |entry| {
            let Some(s) = s.upgrade() else { return };
            let text = entry.text();
            s.context
                .settings()
                .set_search_text((!text.is_empty()).then_some(text.as_str()));
            s.find(true, true);
        });
        let on = |f: fn(&Search)| {
            let s = weak();
            move || {
                if let Some(s) = s.upgrade() {
                    f(&s);
                }
            }
        };
        let next_match: fn(&Search) = |s| s.find(true, false);
        let previous_match: fn(&Search) = |s| s.find(false, false);
        let f = on(next_match);
        search.entry.connect_activate(move |_| f());
        let f = on(next_match);
        search.entry.connect_next_match(move |_| f());
        let f = on(previous_match);
        search.entry.connect_previous_match(move |_| f());
        let f = on(|s| s.close());
        search.entry.connect_stop_search(move |_| f());
        let f = on(next_match);
        next.connect_clicked(move |_| f());
        let f = on(previous_match);
        previous.connect_clicked(move |_| f());
        let f = on(|s| s.show_count());
        search.context.connect_occurrences_count_notify(move |_| f());
        let s = weak();
        search.bar.connect_search_mode_enabled_notify(move |bar| {
            let Some(s) = s.upgrade() else { return };
            if !bar.is_search_mode() {
                s.context.settings().set_search_text(None);
                if let Some(inner) = s.inner.upgrade() {
                    inner.view.grab_focus();
                }
            } else if !s.entry.text().is_empty() {
                let text = s.entry.text();
                s.context.settings().set_search_text(Some(text.as_str()));
            }
        });
        // Shift+Enter: the match before.
        let keys = gtk::EventControllerKey::new();
        let f = on(previous_match);
        keys.connect_key_pressed(move |_, key, _, state| {
            if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) && state.contains(gdk::ModifierType::SHIFT_MASK) {
                f();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        search.entry.add_controller(keys);
        search
    }

    /// Ctrl+F, Ctrl+G and Shift+Ctrl+G anywhere in the view.
    fn install(self: &Rc<Self>, root: &gtk::Box) {
        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_scope(gtk::ShortcutScope::Local);
        let add = |accel: &str, f: fn(&Search)| {
            let s = Rc::downgrade(self);
            shortcuts.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(accel),
                Some(gtk::CallbackAction::new(move |_, _| {
                    if let Some(s) = s.upgrade() {
                        f(&s);
                    }
                    glib::Propagation::Stop
                })),
            ));
        };
        add("<Control>f", |s| s.open());
        add("<Control>g", |s| s.find(true, false));
        add("<Control><Shift>g", |s| s.find(false, false));
        root.add_controller(shortcuts);
    }

    fn open(&self) {
        let Some(inner) = self.inner.upgrade() else { return };
        // The selection, if it is on one line, is what to look for.
        if let Some((a, b)) = inner.buffer.selection_bounds()
            && a.line() == b.line()
        {
            self.entry.set_text(&inner.buffer.text(&a, &b, false));
        }
        self.bar.set_search_mode(true);
        self.entry.grab_focus();
        self.entry.select_region(0, -1);
    }

    fn close(&self) {
        self.bar.set_search_mode(false);
    }

    /// Selects the next (or previous) match from the selection; `again`
    /// starts at the selection's start, so typing grows the match in place.
    fn find(&self, forward: bool, again: bool) {
        let Some(inner) = self.inner.upgrade() else { return };
        if self.context.settings().search_text().is_none() {
            self.count.set_text("");
            return;
        }
        let buffer = &inner.buffer;
        let (start, end) = buffer
            .selection_bounds()
            .unwrap_or_else(|| (buffer.iter_at_mark(&buffer.get_insert()), buffer.iter_at_mark(&buffer.get_insert())));
        let found = if forward {
            self.context.forward(if again { &start } else { &end })
        } else {
            self.context.backward(&start)
        };
        if let Some((mut a, b, _)) = found {
            buffer.select_range(&a, &b);
            inner.view.scroll_to_iter(&mut a, 0.1, false, 0.0, 0.0);
        }
        self.show_count();
    }

    /// "3 of 12", "No matches", or nothing while it is counting.
    fn show_count(&self) {
        let Some(inner) = self.inner.upgrade() else { return };
        if self.context.settings().search_text().is_none() {
            self.count.set_text("");
            return;
        }
        let total = self.context.occurrences_count();
        let text = match total {
            n if n < 0 => String::new(),
            0 => "No matches".to_string(),
            n => match inner.buffer.selection_bounds() {
                Some((a, b)) if self.context.occurrence_position(&a, &b) > 0 => {
                    format!("{} of {n}", self.context.occurrence_position(&a, &b))
                }
                _ => format!("{n} matches"),
            },
        };
        self.count.set_text(&text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_move_from_utf16_to_characters() {
        // "é" is one character and one UTF-16 unit; the emoji one
        // character and two units.
        let text = "let s = \"é😀\"; // ok\n";
        let spans = tl_ffi::highlight::highlight(text.into(), "rust".into());
        let chars = char_spans(text, &spans);
        let string = chars.iter().find(|s| KINDS[s.2] == HighlightKind::String).unwrap();
        assert_eq!((string.0, string.1), (8, 12));
        let comment = chars.iter().find(|s| KINDS[s.2] == HighlightKind::Comment).unwrap();
        let start = text.find("//").unwrap();
        assert_eq!(comment.0 as usize, text[..start].chars().count());
        assert_eq!(comment.1 - comment.0, 5);
    }

    #[test]
    fn operators_stay_plain_and_lines_count_like_the_others() {
        let spans = [HighlightSpan {
            start_utf16: 0,
            len_utf16: 1,
            kind: HighlightKind::Operator,
        }];
        assert!(char_spans("+", &spans).is_empty());
        assert_eq!(count_lines(""), 1);
        assert_eq!(count_lines("a\nb\n"), 3);
    }

    #[test]
    fn colours_blend_over_the_canvas() {
        assert_eq!(blend("#ffffff", "#000000", 0.5), "#808080");
        assert_eq!(blend("#ff000080", "#000000", 1.0), "#800000");
        assert_eq!(blend("#123456", "#ffffff", 1.0), "#123456");
    }
}
