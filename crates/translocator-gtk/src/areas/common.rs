//! What the area dialogs (rename, sync, properties) share:
//!
//! - [`CoreText`]: a text field whose text the core owns. Every edit is
//!   sent whole; the core's answers lag the typing, so its echoes of what
//!   was sent are told apart from its own changes (a placeholder button)
//!   and never overwrite what has been typed since. The GPUI shell's
//!   `CoreInput` and the Swift `CoreTextField` do the same.
//! - [`PagedModel`]: a `gio::ListModel` over a long list the core hands
//!   out by range, fetched by page as the view asks. Like
//!   `crate::file_list::RowModel` it is positional and refreshed in place
//!   when the list's version moves, so the view keeps its scroll position.
//! - [`cell_factory`]: list cells that redraw when their item is refreshed.
//! - [`enter_activates`]: Return runs the dialog's default button from
//!   anywhere in it but another button.
//! - the few style rules these dialogs add to libadwaita's.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::OnceLock;

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib};

// ----- style ------------------------------------------------------------------

const CSS: &str = r#"
.tl-token { padding: 2px 5px; min-height: 0; min-width: 0; font-family: monospace; font-size: smaller; font-weight: normal; }
.tl-property .title { font-size: 0.9em; opacity: 0.6; }
.tl-property .subtitle { font-size: inherit; opacity: 1; }
.tl-frame { border-radius: 8px; }
.tl-sync-action { min-width: 26px; min-height: 22px; padding: 0 2px; }
.tl-sync-action.accent { color: @accent_color; }
.tl-sync-action.warning { color: @warning_color; }
.tl-sync-action.muted { opacity: 0.55; }
.tl-compact-list > listview > row { min-height: 0; padding-top: 1px; padding-bottom: 1px; }
.tl-compact-list > listview > row > cell { padding-top: 2px; padding-bottom: 2px; }
.tl-perm-check { margin-left: 4px; margin-right: 4px; }
"#;

/// Adds the dialogs' style rules, once.
pub fn install_css() {
    thread_local! {
        static DONE: Cell<bool> = const { Cell::new(false) };
    }
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    let Some(display) = gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

pub fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

/// A label for a line of text in a dialog: left-aligned, wrapping.
pub fn note_label(classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .build();
    for class in classes {
        label.add_css_class(class);
    }
    label
}

/// Sets an optional line: hidden when there is none.
pub fn set_optional(label: &gtk::Label, text: Option<&str>) {
    label.set_text(text.unwrap_or(""));
    label.set_visible(text.is_some());
}

/// The header bar of an area dialog: Cancel at the start, the default
/// action at the end, no window buttons (Escape cancels).
pub fn header_bar(cancel: &gtk::Button, ok: &gtk::Button) -> adw::HeaderBar {
    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    header.pack_start(cancel);
    header.pack_end(ok);
    header
}

// ----- text the core owns ------------------------------------------------------

#[derive(Default)]
struct Echo {
    /// The texts sent that the core has not shown back yet, oldest first.
    pending: VecDeque<String>,
    /// The core's text at the last [`CoreText::sync`].
    last: String,
}

/// A text field (an `adw::EntryRow`, a `gtk::Entry`) whose text the core
/// owns.
pub struct CoreText {
    editable: gtk::Editable,
    echo: Rc<RefCell<Echo>>,
    updating: Rc<Cell<bool>>,
}

impl CoreText {
    /// Shows `text` in `editable`; each edit is sent whole with `send`.
    pub fn new(editable: &impl IsA<gtk::Editable>, text: &str, send: impl Fn(String) + 'static) -> CoreText {
        let editable = editable.clone().upcast::<gtk::Editable>();
        editable.set_text(text);
        let echo = Rc::new(RefCell::new(Echo {
            pending: VecDeque::new(),
            last: text.to_string(),
        }));
        let updating = Rc::new(Cell::new(false));
        let (sent, guard) = (echo.clone(), updating.clone());
        editable.connect_changed(move |editable| {
            if guard.get() {
                return;
            }
            let text = editable.text().to_string();
            sent.borrow_mut().pending.push_back(text.clone());
            send(text);
        });
        CoreText {
            editable,
            echo,
            updating,
        }
    }

    /// Takes in the core's text: an echo of an edit sent is dropped, a
    /// change of the core's own replaces the field's text (the caret goes
    /// to its end).
    pub fn sync(&self, core: &str) {
        {
            let mut echo = self.echo.borrow_mut();
            if echo.last == core {
                // Caught up with the typing (some echoes may have been
                // skipped): nothing is in flight any more.
                if self.editable.text() == core {
                    echo.pending.clear();
                }
                return;
            }
            echo.last = core.to_string();
            if let Some(pos) = echo.pending.iter().position(|t| t == core) {
                echo.pending.drain(..=pos);
                return;
            }
            echo.pending.clear();
        }
        if self.editable.text() != core {
            self.updating.set(true);
            self.editable.set_text(core);
            self.editable.set_position(-1);
            self.updating.set(false);
        }
    }

    /// Puts the keyboard in the field without selecting its text.
    pub fn focus_at_end(&self) {
        match self.editable.delegate().and_downcast::<gtk::Text>() {
            Some(text) => {
                text.grab_focus_without_selecting();
            }
            None => {
                self.editable.grab_focus();
            }
        }
        self.editable.set_position(-1);
    }
}

// ----- the paged list model ----------------------------------------------------

/// Rows fetched at a time.
const PAGE: u32 = 200;
/// Pages kept before the cache starts again.
const MAX_PAGES: usize = 32;

type Value = Rc<dyn Any>;
type Fetch = Box<dyn Fn(u32, u32) -> Vec<Value>>;

mod item_imp {
    use super::*;
    use gtk::glib::subclass::Signal;

    #[derive(Default)]
    pub struct PagedItem {
        pub value: RefCell<Option<Value>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PagedItem {
        const NAME: &'static str = "TlPagedItem";
        type Type = super::PagedItem;
    }

    impl ObjectImpl for PagedItem {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("changed").build()])
        }
    }
}

glib::wrapper! {
    /// Item *n* of a [`PagedModel`]: the row at that position, refreshed
    /// in place.
    pub struct PagedItem(ObjectSubclass<item_imp::PagedItem>);
}

impl PagedItem {
    fn new(value: Option<Value>) -> PagedItem {
        let item: PagedItem = glib::Object::new();
        item.imp().value.replace(value);
        item
    }

    fn value(&self) -> Option<Value> {
        self.imp().value.borrow().clone()
    }

    fn set(&self, value: Option<Value>) {
        self.imp().value.replace(value);
        self.emit_by_name::<()>("changed", &[]);
    }
}

mod model_imp {
    use super::*;

    #[derive(Default)]
    pub struct PagedModel {
        pub count: Cell<u32>,
        pub fetch: RefCell<Option<Fetch>>,
        /// Rows by page start.
        pub pages: RefCell<HashMap<u32, Vec<Value>>>,
        /// The items handed out, by position, while anyone holds them.
        pub live: RefCell<HashMap<u32, glib::WeakRef<PagedItem>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PagedModel {
        const NAME: &'static str = "TlPagedModel";
        type Type = super::PagedModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for PagedModel {}

    impl ListModelImpl for PagedModel {
        fn item_type(&self) -> glib::Type {
            PagedItem::static_type()
        }

        fn n_items(&self) -> u32 {
            self.count.get()
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            if position >= self.count.get() {
                return None;
            }
            if let Some(item) = self.live.borrow().get(&position).and_then(|w| w.upgrade()) {
                return Some(item.upcast());
            }
            // A row the core no longer has (its list is newer than the
            // count) is blank until the next snapshot.
            let item = PagedItem::new(self.row_at(position));
            let mut live = self.live.borrow_mut();
            if live.len() > 4096 {
                live.retain(|_, w| w.upgrade().is_some());
            }
            live.insert(position, item.downgrade());
            Some(item.upcast())
        }
    }

    impl PagedModel {
        pub fn row_at(&self, position: u32) -> Option<Value> {
            let start = position / PAGE * PAGE;
            let mut pages = self.pages.borrow_mut();
            if !pages.contains_key(&start) {
                let fetch = self.fetch.borrow();
                let fetch = fetch.as_ref()?;
                if pages.len() >= MAX_PAGES {
                    pages.clear();
                }
                pages.insert(start, fetch(start, PAGE));
            }
            pages.get(&start)?.get((position - start) as usize).cloned()
        }
    }
}

glib::wrapper! {
    /// A long list from the core, fetched by page.
    pub struct PagedModel(ObjectSubclass<model_imp::PagedModel>)
        @implements gio::ListModel;
}

impl PagedModel {
    /// A list of `count` rows; `fetch(start, count)` hands out rows
    /// `start..start + count` (fewer at the end).
    pub fn new<T: 'static>(count: u32, fetch: impl Fn(u32, u32) -> Vec<T> + 'static) -> PagedModel {
        let model: PagedModel = glib::Object::new();
        model.imp().count.set(count);
        model.imp().fetch.replace(Some(Box::new(move |start, count| {
            fetch(start, count).into_iter().map(|row| Rc::new(row) as Value).collect()
        })));
        model
    }

    /// Takes in the list's row count; with `changed`, its rows changed
    /// too: the rows shown are fetched again in place.
    pub fn update(&self, count: u32, changed: bool) {
        let imp = self.imp();
        let old = imp.count.replace(count);
        if changed {
            imp.pages.borrow_mut().clear();
            let live: Vec<(u32, PagedItem)> = imp
                .live
                .borrow_mut()
                .drain()
                .filter(|(pos, _)| *pos < count)
                .filter_map(|(pos, weak)| weak.upgrade().map(|item| (pos, item)))
                .collect();
            for (pos, item) in &live {
                item.set(imp.row_at(*pos));
            }
            imp.live
                .borrow_mut()
                .extend(live.into_iter().map(|(pos, item)| (pos, item.downgrade())));
        }
        if count > old {
            self.items_changed(old, 0, count - old);
        } else if count < old {
            self.items_changed(count, old - count, 0);
        }
    }
}

/// Shows a row in a cell's widgets.
type Render<S, T> = Rc<dyn Fn(&S, Option<&T>)>;

/// The widgets of one cell, and the item it shows.
struct Bound<S> {
    state: S,
    item: RefCell<Option<(PagedItem, glib::SignalHandlerId)>>,
}

const CELL_KEY: &str = "tl-paged-cell";

/// A factory for the cells of a [`PagedModel`] whose rows are `T`:
/// `setup` makes a cell's widget and whatever `render` needs of it, and
/// `render` shows a row in it (`None`: a row not there yet), again each
/// time the row is refreshed.
pub fn cell_factory<T: 'static, S: 'static>(
    setup: impl Fn() -> (gtk::Widget, S) + 'static,
    render: impl Fn(&S, Option<&T>) + 'static,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let render: Render<S, T> = Rc::new(render);
    factory.connect_setup(move |_, obj| {
        let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else { return };
        let (widget, state) = setup();
        list_item.set_child(Some(&widget));
        let bound = Rc::new(Bound {
            state,
            item: RefCell::new(None),
        });
        // SAFETY: a list item belongs to one factory, and this factory
        // only ever stores `Rc<Bound<S>>` under the key.
        unsafe { list_item.set_data(CELL_KEY, bound) };
    });
    let draw = |bound: &Bound<S>, render: &dyn Fn(&S, Option<&T>)| {
        let value = bound.item.borrow().as_ref().and_then(|(item, _)| item.value());
        render(&bound.state, value.as_deref().and_then(|v| v.downcast_ref::<T>()));
    };
    let on_bind = render.clone();
    factory.connect_bind(move |_, obj| {
        let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else { return };
        let Some(item) = list_item.item().and_downcast::<PagedItem>() else { return };
        // SAFETY: set in setup, with this type.
        let Some(bound) = (unsafe { list_item.data::<Rc<Bound<S>>>(CELL_KEY) }) else { return };
        let bound = unsafe { bound.as_ref() }.clone();
        let weak = Rc::downgrade(&bound);
        let render = on_bind.clone();
        let handler = item.connect_local("changed", false, move |_| {
            if let Some(bound) = weak.upgrade() {
                draw(&bound, render.as_ref());
            }
            None
        });
        if let Some((old, id)) = bound.item.replace(Some((item, handler))) {
            old.disconnect(id);
        }
        draw(&bound, on_bind.as_ref());
    });
    factory.connect_unbind(move |_, obj| {
        let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else { return };
        // SAFETY: set in setup, with this type.
        let Some(bound) = (unsafe { list_item.data::<Rc<Bound<S>>>(CELL_KEY) }) else { return };
        let bound = unsafe { bound.as_ref() };
        if let Some((item, id)) = bound.item.take() {
            item.disconnect(id);
        }
    });
    factory
}

// ----- Return ------------------------------------------------------------------

/// Return (or Enter) anywhere in `dialog` clicks `button` when it is
/// enabled, unless the keyboard is on another push button, which Return
/// presses. Check boxes and toggles do not take it: Space toggles them.
pub fn enter_activates(dialog: &adw::Dialog, button: &gtk::Button) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (weak_dialog, weak_button) = (dialog.downgrade(), button.downgrade());
    keys.connect_key_pressed(move |_, keyval, _, state| {
        use gdk::Key as K;
        if !matches!(keyval, K::Return | K::KP_Enter | K::ISO_Enter)
            || state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK)
        {
            return glib::Propagation::Proceed;
        }
        let (Some(dialog), Some(button)) = (weak_dialog.upgrade(), weak_button.upgrade()) else {
            return glib::Propagation::Proceed;
        };
        let focus = dialog.focus();
        let on_button = focus.as_ref().is_some_and(|f| {
            let button = f.downcast_ref::<gtk::Button>().cloned().or_else(|| f.ancestor(gtk::Button::static_type()).and_downcast());
            button.is_some_and(|b| !b.is::<gtk::ToggleButton>())
        });
        if on_button {
            return glib::Propagation::Proceed;
        }
        if button.is_sensitive() {
            button.emit_clicked();
        }
        glib::Propagation::Stop
    });
    dialog.add_controller(keys);
}
