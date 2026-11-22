//! The dialogs of `Snapshot.dialogs`: the one on top is shown.
//!
//! A dialog never closes itself: its buttons send input, and it goes when
//! the core drops it from the snapshot. Alert dialogs are made with
//! `can-close` off, so a button (or Escape) only reports; the shell closes
//! them with `force_close` when the core is done with them. A dialog the
//! user closes some other way sends Cancel.
//!
//! The generic dialogs are here; the area dialogs (rename, sync,
//! properties, bookmarks) come from [`crate::areas`], through the same
//! [`DialogView`] interface.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};
use tl_ffi::input::{DialogInput, Input};
use tl_ffi::snapshot::{
    AboutSnapshot, ConfirmSnapshot, ConflictSnapshot, DialogKindSnapshot, DialogSnapshot, InputDialogSnapshot,
    Newer,
};
use tl_ffi::types::{ConflictChoice, Utf16Range};

use crate::areas;
use crate::shell;

/// One presented dialog.
pub trait DialogView {
    /// What is presented.
    fn dialog(&self) -> adw::Dialog;
    /// The same dialog (same id) in a newer snapshot.
    fn update(&self, _kind: &DialogKindSnapshot) {}
}

/// Sends a dialog's input, unless the shell itself is closing it.
#[derive(Clone)]
pub struct Sender {
    pub id: u64,
    /// The core dropped the dialog: closing it is not an answer.
    pub dropped: Rc<Cell<bool>>,
}

impl Sender {
    pub fn send(&self, input: DialogInput) {
        if !self.dropped.get() {
            shell::send(Input::Dialog { id: self.id, input });
        }
    }
}

struct Current {
    id: u64,
    view: Box<dyn DialogView>,
    sender: Sender,
    /// The dialog closed on its own (an about box, a placeholder).
    gone: Rc<Cell<bool>>,
}

#[derive(Default)]
pub struct Dialogs {
    current: RefCell<Option<Current>>,
}

impl Dialogs {
    pub fn apply(&self, dialogs: &[DialogSnapshot], parent: &impl IsA<gtk::Widget>) {
        let top = dialogs.last();
        let same = self.current.borrow().as_ref().map(|c| c.id) == top.map(|t| t.id);
        if same {
            if let (Some(current), Some(top)) = (self.current.borrow().as_ref(), top)
                && !current.gone.get()
            {
                current.view.update(&top.kind);
            }
            return;
        }
        // Out of the cell before closing: closing emits signals.
        if let Some(old) = self.current.take() {
            old.sender.dropped.set(true);
            if !old.gone.get() {
                old.view.dialog().force_close();
            }
        }
        let Some(top) = top else {
            // The keyboard goes back to the list, not nowhere.
            shell::restore_focus_soon();
            return;
        };
        let sender = Sender {
            id: top.id,
            dropped: Rc::new(Cell::new(false)),
        };
        let view = build(&top.kind, &sender);
        let dialog = view.dialog();
        let gone = Rc::new(Cell::new(false));
        let flag = gone.clone();
        let closed_sender = sender.clone();
        dialog.connect_closed(move |_| {
            if !flag.replace(true) {
                // Closed some other way than by the core.
                closed_sender.send(DialogInput::Cancel);
            }
        });
        dialog.present(Some(parent));
        self.current.replace(Some(Current {
            id: top.id,
            view,
            sender,
            gone,
        }));
    }
}

fn build(kind: &DialogKindSnapshot, sender: &Sender) -> Box<dyn DialogView> {
    match kind {
        DialogKindSnapshot::Input { dialog } => input(dialog, sender),
        DialogKindSnapshot::Confirm { dialog } => confirm(dialog, sender),
        DialogKindSnapshot::Message { title, message: text } => message(title, text, sender),
        DialogKindSnapshot::Secret { title, message, text } => secret(title, message, text, sender),
        DialogKindSnapshot::Conflict { dialog } => conflict(dialog, sender),
        DialogKindSnapshot::FindResults { title, count, version } => find_results(title, *count, *version, sender),
        DialogKindSnapshot::About { dialog } => about(dialog, sender),
        DialogKindSnapshot::Rename { dialog } => areas::rename::build(dialog, sender),
        DialogKindSnapshot::Sync { dialog } => areas::sync::build(dialog, sender),
        DialogKindSnapshot::Properties { dialog } => areas::properties::build(dialog, sender),
        DialogKindSnapshot::BookmarkEdit { dialog } => areas::bookmarks::build_edit(dialog, sender),
        DialogKindSnapshot::Bookmarks { dialog } => areas::bookmarks::build_manager(dialog, sender),
    }
}

/// An alert that reports its buttons and Escape instead of closing.
fn alert(heading: &str, body: &str) -> adw::AlertDialog {
    let alert = adw::AlertDialog::new(Some(heading), (!body.is_empty()).then_some(body));
    alert.set_can_close(false);
    alert
}

/// Brings the window forward: a provider's question may come while the
/// app is in the background.
fn raise() {
    shell::with(|s| s.window.present());
}

// ----- input -------------------------------------------------------------------

struct InputView {
    alert: adw::AlertDialog,
    entry: gtk::Entry,
    updating: Rc<Cell<bool>>,
}

impl DialogView for InputView {
    fn dialog(&self) -> adw::Dialog {
        self.alert.clone().upcast()
    }

    fn update(&self, kind: &DialogKindSnapshot) {
        let DialogKindSnapshot::Input { dialog } = kind else { return };
        self.alert.set_heading(Some(&dialog.title));
        self.alert.set_body(&dialog.message);
        if self.entry.text() != dialog.text {
            self.updating.set(true);
            self.entry.set_text(&dialog.text);
            self.updating.set(false);
        }
    }
}

/// A UTF-16 range of `text` as character offsets, for GTK's editables.
fn char_range(text: &str, range: Utf16Range) -> (i32, i32) {
    let (mut units, mut start, mut end) = (0u32, None, None);
    let stop = range.start + range.length;
    for (ix, c) in text.chars().enumerate() {
        if start.is_none() && units >= range.start {
            start = Some(ix as i32);
        }
        if end.is_none() && units >= stop {
            end = Some(ix as i32);
        }
        units += c.len_utf16() as u32;
    }
    let chars = text.chars().count() as i32;
    (start.unwrap_or(chars), end.unwrap_or(chars))
}

fn input(d: &InputDialogSnapshot, sender: &Sender) -> Box<dyn DialogView> {
    let alert = alert(&d.title, &d.message);
    alert.add_responses(&[("cancel", "Cancel"), ("ok", &d.ok)]);
    alert.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some("ok"));
    alert.set_close_response("cancel");
    let entry = gtk::Entry::builder().text(&d.text).build();
    entry.update_property(&[gtk::accessible::Property::Label(if d.message.is_empty() {
        &d.title
    } else {
        &d.message
    })]);
    alert.set_extra_child(Some(&entry));
    let updating = Rc::new(Cell::new(false));

    let s = sender.clone();
    let guard = updating.clone();
    entry.connect_changed(move |entry| {
        if !guard.get() {
            s.send(DialogInput::Text {
                text: entry.text().to_string(),
            });
        }
    });
    let submit = {
        let s = sender.clone();
        let entry = entry.clone();
        move || {
            // The field's text first, so the core answers what is shown.
            s.send(DialogInput::Text {
                text: entry.text().to_string(),
            });
            s.send(DialogInput::Submit);
        }
    };
    let on_enter = submit.clone();
    entry.connect_activate(move |_| on_enter());
    let s = sender.clone();
    alert.connect_response(None, move |_, response| match response {
        "ok" => submit(),
        _ => s.send(DialogInput::Cancel),
    });
    let s = sender.clone();
    alert.connect_close_attempt(move |_| s.send(DialogInput::Cancel));

    // Select what the core asks for once the field is up (the stem of a
    // name being renamed).
    let select = d.select.map(|r| char_range(&d.text, r));
    entry.connect_map(move |entry| {
        let entry = entry.clone();
        glib::idle_add_local_once(move || {
            entry.grab_focus();
            match select {
                Some((start, end)) => entry.select_region(start, end),
                None => entry.select_region(0, -1),
            }
        });
    });
    Box::new(InputView {
        alert,
        entry,
        updating,
    })
}

// ----- confirm, message, secret ----------------------------------------------------

struct AlertView(adw::AlertDialog);

impl DialogView for AlertView {
    fn dialog(&self) -> adw::Dialog {
        self.0.clone().upcast()
    }
}

fn confirm(d: &ConfirmSnapshot, sender: &Sender) -> Box<dyn DialogView> {
    let alert = alert(&d.title, &d.message);
    alert.add_responses(&[("cancel", "Cancel"), ("ok", &d.ok)]);
    alert.set_response_appearance(
        "ok",
        if d.danger {
            adw::ResponseAppearance::Destructive
        } else {
            adw::ResponseAppearance::Suggested
        },
    );
    alert.set_default_response(Some("ok"));
    alert.set_close_response("cancel");
    let s = sender.clone();
    alert.connect_response(None, move |_, response| {
        s.send(if response == "ok" {
            DialogInput::Submit
        } else {
            DialogInput::Cancel
        })
    });
    let s = sender.clone();
    alert.connect_close_attempt(move |_| s.send(DialogInput::Cancel));
    if d.from_provider {
        raise();
    }
    Box::new(AlertView(alert))
}

fn message(title: &str, text: &str, sender: &Sender) -> Box<dyn DialogView> {
    let alert = alert(title, "");
    alert.add_response("ok", "OK");
    alert.set_default_response(Some("ok"));
    alert.set_close_response("ok");
    let label = gtk::Label::builder()
        .label(text)
        .selectable(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .valign(gtk::Align::Start)
        .build();
    label.add_css_class("monospace");
    let scroll = gtk::ScrolledWindow::builder()
        .child(&label)
        .propagate_natural_height(true)
        .max_content_height(360)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    alert.set_extra_child(Some(&scroll));
    let s = sender.clone();
    alert.connect_response(None, move |_, _| s.send(DialogInput::Submit));
    let s = sender.clone();
    alert.connect_close_attempt(move |_| s.send(DialogInput::Cancel));
    Box::new(AlertView(alert))
}

fn secret(title: &str, message: &str, text: &str, sender: &Sender) -> Box<dyn DialogView> {
    let alert = alert(title, message);
    alert.add_responses(&[("cancel", "Cancel"), ("ok", "OK")]);
    alert.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some("ok"));
    alert.set_close_response("cancel");
    let row = adw::PasswordEntryRow::builder().title("Password").text(text).build();
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.append(&row);
    alert.set_extra_child(Some(&list));
    let s = sender.clone();
    row.connect_changed(move |row| {
        s.send(DialogInput::Text {
            text: row.text().to_string(),
        })
    });
    let submit = {
        let s = sender.clone();
        let row = row.clone();
        move || {
            s.send(DialogInput::Text {
                text: row.text().to_string(),
            });
            s.send(DialogInput::Submit);
        }
    };
    let on_enter = submit.clone();
    row.connect_entry_activated(move |_| on_enter());
    let s = sender.clone();
    alert.connect_response(None, move |_, response| match response {
        "ok" => submit(),
        _ => s.send(DialogInput::Cancel),
    });
    let s = sender.clone();
    alert.connect_close_attempt(move |_| s.send(DialogInput::Cancel));
    row.connect_map(|row| {
        let row = row.clone();
        glib::idle_add_local_once(move || {
            row.grab_focus();
        });
    });
    raise();
    Box::new(AlertView(alert))
}

// ----- conflict ----------------------------------------------------------------------

struct ConflictView {
    alert: adw::AlertDialog,
    apply_all: gtk::CheckButton,
    updating: Rc<Cell<bool>>,
}

impl DialogView for ConflictView {
    fn dialog(&self) -> adw::Dialog {
        self.alert.clone().upcast()
    }

    fn update(&self, kind: &DialogKindSnapshot) {
        let DialogKindSnapshot::Conflict { dialog } = kind else { return };
        if self.apply_all.is_active() != dialog.apply_all {
            self.updating.set(true);
            self.apply_all.set_active(dialog.apply_all);
            self.updating.set(false);
        }
    }
}

fn conflict(d: &ConflictSnapshot, sender: &Sender) -> Box<dyn DialogView> {
    let alert = alert(&d.title, &d.location);
    for (ix, choice) in d.choices.iter().enumerate() {
        let id = ix.to_string();
        alert.add_response(&id, &choice.label);
        if choice.primary {
            alert.set_response_appearance(&id, adw::ResponseAppearance::Suggested);
            alert.set_default_response(Some(&id));
        }
        if choice.choice == ConflictChoice::Cancel {
            alert.set_close_response(&id);
        }
    }
    let grid = gtk::Grid::builder().column_spacing(12).row_spacing(6).build();
    let add = |row: i32, label: &str, value: &str, newer: bool| {
        let l = gtk::Label::new(Some(label));
        l.add_css_class("dim-label");
        l.set_xalign(0.0);
        let v = gtk::Label::new(Some(value));
        v.set_xalign(0.0);
        v.set_wrap(true);
        if newer {
            v.add_css_class("success");
        }
        grid.attach(&l, 0, row, 1, 1);
        grid.attach(&v, 1, row, 1, 1);
    };
    add(0, "Existing", &d.existing, d.newer == Newer::Existing);
    add(1, &d.incoming_label, &d.incoming, d.newer == Newer::Incoming);
    let apply_all = gtk::CheckButton::with_label("Apply to all remaining conflicts");
    apply_all.set_active(d.apply_all);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.append(&grid);
    content.append(&apply_all);
    alert.set_extra_child(Some(&content));

    let updating = Rc::new(Cell::new(false));
    let s = sender.clone();
    let guard = updating.clone();
    apply_all.connect_toggled(move |check| {
        if !guard.get() {
            s.send(DialogInput::ApplyAll { on: check.is_active() });
        }
    });
    let choices: Vec<ConflictChoice> = d.choices.iter().map(|c| c.choice).collect();
    let s = sender.clone();
    alert.connect_response(None, move |_, response| {
        let choice = response
            .parse::<usize>()
            .ok()
            .and_then(|ix| choices.get(ix).copied())
            .unwrap_or(ConflictChoice::Cancel);
        s.send(DialogInput::Choose { choice });
    });
    let s = sender.clone();
    alert.connect_close_attempt(move |_| {
        s.send(DialogInput::Choose {
            choice: ConflictChoice::Cancel,
        })
    });
    raise();
    Box::new(ConflictView {
        alert,
        apply_all,
        updating,
    })
}

// ----- find results --------------------------------------------------------------------

mod found_imp {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    pub struct FoundModel {
        pub dialog: Cell<u64>,
        pub count: Cell<u32>,
        pub pages: RefCell<HashMap<u32, Vec<glib::BoxedAnyObject>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FoundModel {
        const NAME: &'static str = "TlFoundModel";
        type Type = super::FoundModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for FoundModel {}

    impl ListModelImpl for FoundModel {
        fn item_type(&self) -> glib::Type {
            glib::BoxedAnyObject::static_type()
        }

        fn n_items(&self) -> u32 {
            self.count.get()
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            if position >= self.count.get() {
                return None;
            }
            let start = position / 200 * 200;
            let mut pages = self.pages.borrow_mut();
            let page = pages.entry(start).or_insert_with(|| {
                shell::core()
                    .find_results(self.dialog.get(), start, 200)
                    .into_iter()
                    .map(glib::BoxedAnyObject::new)
                    .collect()
            });
            page.get((position - start) as usize).map(|o| o.clone().upcast())
        }
    }
}

glib::wrapper! {
    /// Search hits, fetched by page with `Core::find_results`.
    pub struct FoundModel(ObjectSubclass<found_imp::FoundModel>)
        @implements gio::ListModel;
}

impl FoundModel {
    fn new(dialog: u64, count: u32) -> FoundModel {
        let model: FoundModel = glib::Object::new();
        model.imp().dialog.set(dialog);
        model.imp().count.set(count);
        model
    }

    fn reset(&self, count: u32) {
        let old = self.imp().count.replace(count);
        self.imp().pages.borrow_mut().clear();
        self.items_changed(0, old, count);
    }
}

struct FindView {
    dialog: adw::Dialog,
    model: FoundModel,
    found: gtk::Label,
    version: Cell<u64>,
}

impl DialogView for FindView {
    fn dialog(&self) -> adw::Dialog {
        self.dialog.clone()
    }

    fn update(&self, kind: &DialogKindSnapshot) {
        let DialogKindSnapshot::FindResults { title, count, version } = kind else { return };
        self.dialog.set_title(title);
        if self.version.replace(*version) != *version || self.model.n_items() != *count {
            self.model.reset(*count);
        }
        self.found.set_text(&format!("{count} found"));
    }
}

fn find_results(title: &str, count: u32, version: u64, sender: &Sender) -> Box<dyn DialogView> {
    let model = FoundModel::new(sender.id, count);
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&gtk::Image::new());
        let label = gtk::Label::new(None);
        label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        label.set_xalign(0.0);
        row.append(&label);
        item.set_child(Some(&row));
    });
    factory.connect_bind(|_, obj| {
        let Some(item) = obj.downcast_ref::<gtk::ListItem>() else { return };
        let Some(hit) = item.item().and_then(|o| o.downcast::<glib::BoxedAnyObject>().ok()) else { return };
        let hit = hit.borrow::<tl_ffi::snapshot::FoundRow>();
        let Some(row) = item.child() else { return };
        if let Some(icon) = row.first_child().and_downcast::<gtk::Image>() {
            icon.set_icon_name(Some(if hit.is_dir { "folder-symbolic" } else { "text-x-generic-symbolic" }));
        }
        if let Some(label) = row.last_child().and_downcast::<gtk::Label>() {
            label.set_text(&hit.rel);
        }
    });
    let selection = gtk::NoSelection::new(Some(model.clone()));
    let list = gtk::ListView::new(Some(selection), Some(factory));
    list.set_single_click_activate(true);
    list.add_css_class("navigation-sidebar");
    let s = sender.clone();
    list.connect_activate(move |_, index| s.send(DialogInput::Pick { index }));
    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();

    let found = gtk::Label::new(Some(&format!("{count} found")));
    found.add_css_class("dim-label");
    found.set_hexpand(true);
    found.set_xalign(0.0);
    let close = gtk::Button::with_label("Close");
    close.add_css_class("suggested-action");
    let s = sender.clone();
    close.connect_clicked(move |_| s.send(DialogInput::Submit));
    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    bottom.set_margin_top(6);
    bottom.set_margin_bottom(6);
    bottom.set_margin_start(12);
    bottom.set_margin_end(12);
    bottom.append(&found);
    bottom.append(&close);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&scroll));
    view.add_bottom_bar(&bottom);
    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(720)
        .content_height(520)
        .child(&view)
        .can_close(false)
        .build();
    let s = sender.clone();
    dialog.connect_close_attempt(move |_| s.send(DialogInput::Cancel));
    Box::new(FindView {
        dialog,
        model,
        found,
        version: Cell::new(version),
    })
}

// ----- about -----------------------------------------------------------------------------

fn about(d: &AboutSnapshot, sender: &Sender) -> Box<dyn DialogView> {
    let website = d.license.rsplit_once("· ").map(|(_, url)| url.trim().to_string()).unwrap_or_default();
    let about = adw::AboutDialog::builder()
        .application_name(&d.title)
        .application_icon(crate::shell::APP_ID)
        .version(d.version.trim_start_matches("Version "))
        .comments(&d.description)
        .license_type(gtk::License::MitX11)
        .website(&website)
        .build();
    // The website is the README: the core opens it.
    let s = sender.clone();
    about.connect_activate_link(move |_, _| {
        s.send(DialogInput::OpenReadme);
        true
    });
    Box::new(AboutView(about))
}

struct AboutView(adw::AboutDialog);

impl DialogView for AboutView {
    fn dialog(&self) -> adw::Dialog {
        self.0.clone().upcast()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_ranges_become_character_offsets() {
        // "é" is one unit, "𝄞" two: the stem "é𝄞ab" is units 0..5, chars 0..4.
        let text = "é𝄞ab.txt";
        assert_eq!(char_range(text, Utf16Range { start: 0, length: 5 }), (0, 4));
        assert_eq!(char_range(text, Utf16Range { start: 6, length: 3 }), (5, 8));
        assert_eq!(char_range("a", Utf16Range { start: 0, length: 99 }), (0, 1));
    }
}
