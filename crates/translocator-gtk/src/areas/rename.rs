//! The multi-rename dialog (`DialogKindSnapshot::Rename`), after the GPUI
//! shell's `crates/translocator/src/rename.rs` and the Swift
//! `macos/.../Areas/RenameDialog.swift`.
//!
//! An `adw::Dialog` with Cancel / Rename in its header bar, built from
//! [`RenameSnapshot`]: the name and extension masks with their placeholder
//! buttons (each appends its token and puts the keyboard back in the
//! field), find / replace with Regex and Match case, the counter, the case
//! as a linked group of toggles, the help line, and the preview ("old →
//! new", problems in the error colour) as a `GtkColumnView` over
//! `Core::rename_preview`, fetched by page and refreshed in place when
//! `preview_version` moves; `error` replaces it. Under it the summary, the
//! problems and the sudo note. Every edit is `Input::Rename`; Rename (or
//! Return) is `DialogInput::Submit`, which the core takes as
//! `RenameInput::Apply` (with problems the dialog stays). The keyboard
//! starts in the name mask.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use tl_ffi::input::{DialogInput, Input};
use tl_ffi::rename::{CaseChange, RenameField, RenameInput, RenamePreviewRow, RenameSnapshot};
use tl_ffi::snapshot::DialogKindSnapshot;

use super::common::{self, CoreText, PagedModel, set_class, set_optional};
use crate::dialogs::{DialogView, Sender};
use crate::shell;

const FIELDS: [RenameField; 7] = [
    RenameField::Name,
    RenameField::Ext,
    RenameField::Find,
    RenameField::Replace,
    RenameField::Start,
    RenameField::Step,
    RenameField::Digits,
];

/// The row title of a field.
fn title(field: RenameField) -> &'static str {
    match field {
        RenameField::Name => "Name",
        RenameField::Ext => "Extension",
        RenameField::Find => "Find",
        RenameField::Replace => "Replace with",
        RenameField::Start => "Start",
        RenameField::Step => "Step",
        RenameField::Digits => "Digits",
    }
}

/// What a screen reader calls the field.
fn accessible_name(field: RenameField) -> &'static str {
    match field {
        RenameField::Name => "Name mask",
        RenameField::Ext => "Extension mask",
        RenameField::Find => "Find",
        RenameField::Replace => "Replace with",
        RenameField::Start => "Counter start",
        RenameField::Step => "Counter step",
        RenameField::Digits => "Counter digits",
    }
}

/// What the placeholder buttons' tooltips call the field.
fn mask_name(field: RenameField) -> &'static str {
    match field {
        RenameField::Ext => "extension mask",
        _ => "name mask",
    }
}

fn send(sender: &Sender, input: RenameInput) {
    if !sender.dropped.get() {
        shell::send(Input::Rename {
            dialog: sender.id,
            input,
        });
    }
}

struct RenameView {
    dialog: adw::Dialog,
    /// Shared with the placeholder buttons, which put the keyboard back.
    fields: Rc<Vec<(RenameField, CoreText)>>,
    regex: adw::SwitchRow,
    match_case: adw::SwitchRow,
    cases: Vec<(CaseChange, gtk::ToggleButton)>,
    model: PagedModel,
    version: Cell<u64>,
    body: gtk::Stack,
    error: gtk::Label,
    problem_column: gtk::ColumnViewColumn,
    summary: gtk::Label,
    problems: gtk::Label,
    elevated: gtk::Label,
    rename: gtk::Button,
    /// Set while the view itself sets the toggles.
    updating: Rc<Cell<bool>>,
}

impl DialogView for RenameView {
    fn dialog(&self) -> adw::Dialog {
        self.dialog.clone()
    }

    fn update(&self, kind: &DialogKindSnapshot) {
        if let DialogKindSnapshot::Rename { dialog } = kind {
            self.apply(dialog);
        }
    }
}

impl RenameView {
    fn apply(&self, d: &RenameSnapshot) {
        self.dialog.set_title(&d.title);
        for (field, text) in self.fields.iter() {
            text.sync(d.text(*field));
        }
        self.updating.set(true);
        if self.regex.is_active() != d.regex {
            self.regex.set_active(d.regex);
        }
        if self.match_case.is_active() != d.match_case {
            self.match_case.set_active(d.match_case);
        }
        for (case, button) in &self.cases {
            if button.is_active() != (*case == d.case) {
                button.set_active(*case == d.case);
            }
        }
        self.updating.set(false);

        match &d.error {
            Some(error) => {
                self.error.set_text(error);
                self.body.set_visible_child_name("error");
            }
            None => self.body.set_visible_child_name("list"),
        }
        let changed = self.version.replace(d.preview_version) != d.preview_version;
        self.model.update(d.preview_count, changed);
        self.problem_column.set_visible(d.problems > 0);

        self.summary.set_text(&d.summary);
        // Always there, so the dialog does not grow when problems appear.
        self.problems.set_text(d.problems_text.as_deref().unwrap_or(""));
        set_optional(&self.elevated, d.elevated_note.as_deref());
        self.rename.set_sensitive(d.can_apply);
    }
}

pub fn build(d: &RenameSnapshot, sender: &Sender) -> Box<dyn DialogView> {
    common::install_css();
    let updating = Rc::new(Cell::new(false));

    // The text fields, with the placeholder buttons for the masks.
    let rows: Vec<(RenameField, adw::EntryRow)> = FIELDS
        .iter()
        .map(|field| {
            let row = adw::EntryRow::builder().title(title(*field)).build();
            row.upcast_ref::<gtk::Widget>()
                .update_property(&[gtk::accessible::Property::Label(accessible_name(*field))]);
            if matches!(field, RenameField::Start | RenameField::Step | RenameField::Digits) {
                row.set_input_purpose(gtk::InputPurpose::Number);
            }
            (*field, row)
        })
        .collect();
    let row = |field: RenameField| rows.iter().find(|(f, _)| *f == field).map(|(_, r)| r.clone()).unwrap();
    let fields: Vec<(RenameField, CoreText)> = rows
        .iter()
        .map(|(field, row)| {
            let s = sender.clone();
            let field = *field;
            let text = CoreText::new(row, d.text(field), move |text| send(&s, RenameInput::Text { field, text }));
            (field, text)
        })
        .collect();
    let fields = Rc::new(fields);
    let tokens = |tokens: &[String], field: RenameField| {
        let group = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        group.set_valign(gtk::Align::Center);
        for token in tokens {
            let button = gtk::Button::with_label(token);
            button.add_css_class("flat");
            button.add_css_class("tl-token");
            button.set_can_focus(false);
            button.set_focus_on_click(false);
            button.set_tooltip_text(Some(&format!("Insert {token} into the {}", mask_name(field))));
            button.update_property(&[gtk::accessible::Property::Label(&format!("Insert {token}"))]);
            let (s, token, fields) = (sender.clone(), token.clone(), fields.clone());
            button.connect_clicked(move |_| {
                send(&s, RenameInput::Insert { field, token: token.clone() });
                if let Some((_, text)) = fields.iter().find(|(f, _)| *f == field) {
                    text.focus_at_end();
                }
            });
            group.append(&button);
        }
        group
    };
    row(RenameField::Name).add_suffix(&tokens(&d.name_tokens, RenameField::Name));
    row(RenameField::Ext).add_suffix(&tokens(&d.ext_tokens, RenameField::Ext));

    // Find and replace.
    let regex = adw::SwitchRow::builder().title("Regex").active(d.regex).build();
    let match_case = adw::SwitchRow::builder().title("Match case").active(d.match_case).build();
    let (s, guard) = (sender.clone(), updating.clone());
    regex.connect_active_notify(move |row| {
        if !guard.get() {
            send(&s, RenameInput::Regex { on: row.is_active() });
        }
    });
    let (s, guard) = (sender.clone(), updating.clone());
    match_case.connect_active_notify(move |row| {
        if !guard.get() {
            send(&s, RenameInput::MatchCase { on: row.is_active() });
        }
    });
    let find = adw::PreferencesGroup::builder().title("Find and Replace").build();
    find.add(&row(RenameField::Find));
    find.add(&row(RenameField::Replace));
    find.add(&regex);
    find.add(&match_case);

    // The case, under the masks.
    let case_group = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    case_group.add_css_class("linked");
    case_group.set_valign(gtk::Align::Center);
    let mut cases = Vec::new();
    let mut first: Option<gtk::ToggleButton> = None;
    for option in &d.cases {
        let button = gtk::ToggleButton::with_label(&option.label);
        button.set_active(option.case == d.case);
        if let Some(first) = &first {
            button.set_group(Some(first));
        } else {
            first = Some(button.clone());
        }
        let (s, guard, case) = (sender.clone(), updating.clone(), option.case);
        button.connect_toggled(move |button| {
            if button.is_active() && !guard.get() {
                send(&s, RenameInput::CaseChange { change: case });
            }
        });
        case_group.append(&button);
        cases.push((option.case, button));
    }
    let case_row = adw::ActionRow::builder().title("Case").build();
    case_row.add_suffix(&case_group);
    case_group.update_property(&[gtk::accessible::Property::Label("Case")]);
    let masks = boxed_list();
    masks.append(&row(RenameField::Name));
    masks.append(&row(RenameField::Ext));
    masks.append(&case_row);

    // The counter.
    let counter = adw::PreferencesGroup::builder().title("Counter").build();
    counter.add(&row(RenameField::Start));
    counter.add(&row(RenameField::Step));
    counter.add(&row(RenameField::Digits));

    let columns = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    columns.set_homogeneous(true);
    find.set_valign(gtk::Align::Start);
    counter.set_valign(gtk::Align::Start);
    columns.append(&find);
    columns.append(&counter);

    let help = common::note_label(&["caption", "dim-label"]);
    help.set_text(&d.help);

    // The preview, or why the masks cannot be used.
    let model = PagedModel::new(d.preview_count, {
        let id = sender.id;
        move |start, count| shell::core().rename_preview(id, start, count)
    });
    let view = gtk::ColumnView::new(Some(gtk::NoSelection::new(Some(model.clone()))));
    view.add_css_class("data-table");
    view.add_css_class("tl-compact-list");
    view.set_reorderable(false);
    view.set_can_focus(false);
    view.update_property(&[gtk::accessible::Property::Label("Preview")]);
    let column = |title: &str, factory: gtk::SignalListItemFactory, expand: bool| {
        let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
        column.set_expand(expand);
        column.set_resizable(true);
        view.append_column(&column);
        column
    };
    column("Name", text_cell(|row| (row.old.clone(), vec!["dim-label"])), true);
    column(
        "New Name",
        text_cell(|row| {
            let class = if row.problem.is_some() {
                "error"
            } else if row.changed {
                ""
            } else {
                "dim-label"
            };
            (row.new.clone(), vec![class])
        }),
        true,
    );
    let problem_column = column(
        "Problem",
        text_cell(|row| (row.problem.clone().unwrap_or_default(), vec!["error", "caption"])),
        false,
    );
    problem_column.set_fixed_width(240);
    let list = gtk::ScrolledWindow::builder()
        .child(&view)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    let error = common::note_label(&["error"]);
    error.set_selectable(true);
    error.set_valign(gtk::Align::Start);
    error.set_margin_top(10);
    error.set_margin_bottom(10);
    error.set_margin_start(12);
    error.set_margin_end(12);
    let error_scroll = gtk::ScrolledWindow::builder()
        .child(&error)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let body = gtk::Stack::new();
    body.add_named(&list, Some("list"));
    body.add_named(&error_scroll, Some("error"));
    body.set_vexpand(true);
    body.set_size_request(-1, 180);
    let frame = gtk::Frame::new(None);
    frame.add_css_class("view");
    frame.add_css_class("tl-frame");
    frame.set_child(Some(&body));

    // The totals and notes.
    let summary = common::note_label(&["dim-label"]);
    let problems = common::note_label(&["error"]);
    let elevated = common::note_label(&["error", "caption"]);
    let footer = gtk::Box::new(gtk::Orientation::Vertical, 2);
    footer.append(&summary);
    footer.append(&problems);
    footer.append(&elevated);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(6);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&masks);
    content.append(&columns);
    content.append(&help);
    content.append(&frame);
    content.append(&footer);

    let cancel = gtk::Button::with_label("Cancel");
    let rename = gtk::Button::with_label("Rename");
    rename.add_css_class("suggested-action");
    let s = sender.clone();
    cancel.connect_clicked(move |_| s.send(DialogInput::Cancel));
    let s = sender.clone();
    rename.connect_clicked(move |_| s.send(DialogInput::Submit));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&common::header_bar(&cancel, &rename));
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .title(&d.title)
        .content_width(960)
        .content_height(820)
        .child(&toolbar)
        .can_close(false)
        .build();
    dialog.set_default_widget(Some(&rename));
    let s = sender.clone();
    dialog.connect_close_attempt(move |_| s.send(DialogInput::Cancel));
    common::enter_activates(&dialog, &rename);

    // The keyboard starts in the name mask, its text selected.
    let name = row(RenameField::Name);
    dialog.connect_map(move |_| {
        let name = name.clone();
        glib::idle_add_local_once(move || {
            name.grab_focus();
        });
    });

    let view = RenameView {
        dialog,
        fields,
        regex,
        match_case,
        cases,
        model,
        version: Cell::new(d.preview_version),
        body,
        error,
        problem_column,
        summary,
        problems,
        elevated,
        rename,
        updating,
    };
    view.apply(d);
    Box::new(view)
}

fn boxed_list() -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_valign(gtk::Align::Start);
    list
}

/// A one-line cell of the preview: its text and style classes from a row.
fn text_cell(text: impl Fn(&RenamePreviewRow) -> (String, Vec<&'static str>) + 'static) -> gtk::SignalListItemFactory {
    const CLASSES: [&str; 3] = ["dim-label", "error", "caption"];
    common::cell_factory::<RenamePreviewRow, gtk::Label>(
        || {
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .margin_start(6)
                .margin_end(6)
                .build();
            (label.clone().upcast(), label)
        },
        move |label, row| {
            let Some(row) = row else {
                label.set_text("");
                return;
            };
            let (value, classes) = text(row);
            for class in CLASSES {
                set_class(label, class, classes.contains(&class));
            }
            label.set_tooltip_text((!value.is_empty()).then_some(value.as_str()));
            label.set_text(&value);
            let description = match &row.problem {
                Some(problem) => format!("{} to {}, {problem}", row.old, row.new),
                None => format!("{} to {}", row.old, row.new),
            };
            label.update_property(&[gtk::accessible::Property::Description(&description)]);
        },
    )
}
