//! The properties dialog (`DialogKindSnapshot::Properties`), after the
//! GPUI shell's `crates/translocator/src/properties.rs` and the Swift
//! `macos/.../Areas/PropertiesDialog.swift`.
//!
//! An `adw::Dialog` with Cancel / Apply in its header bar, built from
//! [`PropertiesSnapshot`]: the details as property rows (label over a
//! selectable value), the permission grid (owner / group / others × read /
//! write / execute, then setuid, setgid, sticky) as check boxes enabled by
//! `can_chmod`, the mode in octal and symbolic form (monospace), owner and
//! group entry rows when `can_chown`, the recursive switch when a folder is
//! selected, and the sudo note in the error colour. Every change is
//! `Input::Properties`; Apply (or Return) is `DialogInput::Submit`, which
//! the core takes as `PropertiesInput::Apply`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use tl_ffi::input::{DialogInput, Input};
use tl_ffi::properties::{PermissionBit, PropertiesInput, PropertiesSnapshot, PropertyDetail};
use tl_ffi::snapshot::DialogKindSnapshot;

use super::common::{self, CoreText, set_optional};
use crate::dialogs::{DialogView, Sender};
use crate::shell;

fn send(sender: &Sender, input: PropertiesInput) {
    if !sender.dropped.get() {
        shell::send(Input::Properties {
            dialog: sender.id,
            input,
        });
    }
}

struct PropertiesView {
    dialog: adw::Dialog,
    details: adw::PreferencesGroup,
    detail_rows: RefCell<(Vec<PropertyDetail>, Vec<adw::ActionRow>)>,
    /// Every permission box, by its bit.
    checks: Vec<(u32, gtk::CheckButton)>,
    bit_rows: Vec<adw::ActionRow>,
    mode: gtk::Label,
    ownership: adw::PreferencesGroup,
    owner: CoreText,
    group: CoreText,
    recursive_group: adw::PreferencesGroup,
    recursive: adw::SwitchRow,
    elevated: gtk::Label,
    /// Set while the view itself sets the boxes.
    updating: Rc<Cell<bool>>,
}

impl DialogView for PropertiesView {
    fn dialog(&self) -> adw::Dialog {
        self.dialog.clone()
    }

    fn update(&self, kind: &DialogKindSnapshot) {
        if let DialogKindSnapshot::Properties { dialog } = kind {
            self.apply(dialog);
        }
    }
}

impl PropertiesView {
    fn apply(&self, d: &PropertiesSnapshot) {
        self.dialog.set_title(&d.title);
        self.set_details(&d.details);

        let bits = d.rows.iter().flat_map(|r| r.bits.iter()).chain(d.specials.iter());
        self.updating.set(true);
        for bit in bits {
            if let Some((_, check)) = self.checks.iter().find(|(b, _)| *b == bit.bit)
                && check.is_active() != bit.on
            {
                check.set_active(bit.on);
            }
        }
        if self.recursive.is_active() != d.recursive {
            self.recursive.set_active(d.recursive);
        }
        self.updating.set(false);
        for row in &self.bit_rows {
            row.set_sensitive(d.can_chmod);
        }
        self.mode.set_text(&d.mode_text);
        self.mode
            .update_property(&[gtk::accessible::Property::Label(&format!("Mode {}", d.mode_text))]);

        self.ownership.set_visible(d.can_chown);
        self.owner.sync(&d.owner);
        self.group.sync(&d.group);
        self.recursive.set_title(&d.recursive_label);
        self.recursive_group.set_visible(d.any_dir);
        set_optional(&self.elevated, d.elevated_note.as_deref());
    }

    /// The detail rows, made again only when their labels change.
    fn set_details(&self, details: &[PropertyDetail]) {
        let mut shown = self.detail_rows.borrow_mut();
        if shown.0 == details {
            return;
        }
        let same_labels = shown.0.len() == details.len() && shown.0.iter().zip(details).all(|(a, b)| a.label == b.label);
        if same_labels {
            for (row, detail) in shown.1.iter().zip(details) {
                row.set_subtitle(&detail.value);
            }
        } else {
            for row in shown.1.drain(..) {
                self.details.remove(&row);
            }
            for detail in details {
                let row = adw::ActionRow::builder()
                    .title(&detail.label)
                    .subtitle(&detail.value)
                    .subtitle_selectable(true)
                    .use_markup(false)
                    .build();
                row.add_css_class("property");
                row.add_css_class("tl-property");
                self.details.add(&row);
                shown.1.push(row);
            }
        }
        shown.0 = details.to_vec();
    }
}

pub fn build(d: &PropertiesSnapshot, sender: &Sender) -> Box<dyn DialogView> {
    common::install_css();
    let updating = Rc::new(Cell::new(false));

    let details = adw::PreferencesGroup::new();

    // The permission grid: a row per class, a box per bit.
    let permissions = adw::PreferencesGroup::builder().title("Permissions").build();
    let mut checks = Vec::new();
    let mut bit_rows = Vec::new();
    let sizes = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
    let mut check = |bit: &PermissionBit, accessible: String| {
        let check = gtk::CheckButton::with_label(&bit.label);
        check.set_active(bit.on);
        check.set_valign(gtk::Align::Center);
        check.add_css_class("tl-perm-check");
        check.update_property(&[gtk::accessible::Property::Label(&accessible)]);
        sizes.add_widget(&check);
        let (s, guard, b) = (sender.clone(), updating.clone(), bit.bit);
        check.connect_toggled(move |check| {
            if !guard.get() {
                send(&s, PropertiesInput::Bit { bit: b, on: check.is_active() });
            }
        });
        checks.push((bit.bit, check.clone()));
        check
    };
    for class in &d.rows {
        let row = adw::ActionRow::builder().title(&class.label).build();
        for bit in &class.bits {
            row.add_suffix(&check(bit, format!("{} {}", class.label, bit.label.to_lowercase())));
        }
        permissions.add(&row);
        bit_rows.push(row);
    }
    let specials = adw::ActionRow::builder().title("Special").build();
    for bit in &d.specials {
        specials.add_suffix(&check(bit, bit.label.clone()));
    }
    permissions.add(&specials);
    bit_rows.push(specials);
    let mode = gtk::Label::builder().selectable(true).valign(gtk::Align::Center).build();
    mode.add_css_class("monospace");
    mode.add_css_class("dim-label");
    let mode_row = adw::ActionRow::builder().title("Mode").build();
    mode_row.add_suffix(&mode);
    permissions.add(&mode_row);

    // Owner and group.
    let ownership = adw::PreferencesGroup::builder().title("Ownership").build();
    let owner_row = adw::EntryRow::builder().title("Owner").build();
    let group_row = adw::EntryRow::builder().title("Group").build();
    ownership.add(&owner_row);
    ownership.add(&group_row);
    let s = sender.clone();
    let owner = CoreText::new(&owner_row, &d.owner, move |name| send(&s, PropertiesInput::Owner { name }));
    let s = sender.clone();
    let group = CoreText::new(&group_row, &d.group, move |name| send(&s, PropertiesInput::Group { name }));

    // Enclosed items, and the sudo note.
    let recursive_group = adw::PreferencesGroup::new();
    let recursive = adw::SwitchRow::builder()
        .title(&d.recursive_label)
        .active(d.recursive)
        .build();
    let (s, guard) = (sender.clone(), updating.clone());
    recursive.connect_active_notify(move |row| {
        if !guard.get() {
            send(&s, PropertiesInput::Recursive { on: row.is_active() });
        }
    });
    recursive_group.add(&recursive);
    let elevated = common::note_label(&["error", "caption"]);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
    content.set_margin_top(6);
    content.set_margin_bottom(18);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&details);
    content.append(&permissions);
    content.append(&ownership);
    content.append(&recursive_group);
    content.append(&elevated);
    let clamp = adw::Clamp::builder().maximum_size(560).child(&content).build();
    let scroll = gtk::ScrolledWindow::builder()
        .child(&clamp)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(720)
        .build();

    let cancel = gtk::Button::with_label("Cancel");
    let apply = gtk::Button::with_label("Apply");
    apply.add_css_class("suggested-action");
    let s = sender.clone();
    cancel.connect_clicked(move |_| s.send(DialogInput::Cancel));
    let s = sender.clone();
    apply.connect_clicked(move |_| s.send(DialogInput::Submit));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&common::header_bar(&cancel, &apply));
    toolbar.set_content(Some(&scroll));
    let dialog = adw::Dialog::builder()
        .title(&d.title)
        .content_width(560)
        .child(&toolbar)
        .can_close(false)
        .build();
    dialog.set_default_widget(Some(&apply));
    let s = sender.clone();
    dialog.connect_close_attempt(move |_| s.send(DialogInput::Cancel));
    common::enter_activates(&dialog, &apply);
    // The keyboard starts on the first box (or the owner, with no boxes to
    // change), not on a detail's selectable text.
    let first = checks.first().map(|(_, c)| c.clone().upcast::<gtk::Widget>());
    let start = if d.can_chmod { first } else { Some(owner_row.clone().upcast()) };
    if let Some(start) = start {
        dialog.connect_map(move |_| {
            let start = start.clone();
            glib::idle_add_local_once(move || {
                start.grab_focus();
            });
        });
    }

    let view = PropertiesView {
        dialog,
        details,
        detail_rows: RefCell::new((Vec::new(), Vec::new())),
        checks,
        bit_rows,
        mode,
        ownership,
        owner,
        group,
        recursive_group,
        recursive,
        elevated,
        updating,
    };
    view.apply(d);
    Box::new(view)
}
