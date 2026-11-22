//! STUB: quick look (⌘Y), drawn in place of the pane opposite the active
//! one while `Snapshot.quick_look` is set.
//!
//! Must become a widget built from [`QuickLookSnapshot`]
//! (`tl_ffi::quick_look`): a header with the icon (`icon`, see
//! `crate::file_list::icon`) and `name`; the body by `body`: a centred
//! message (`QuickLookBody::message`, the error colour for errors), an
//! image decoded from `Core::quick_look_image()` (`gdk::Texture` from the
//! bytes, scaled to fit, in a `gtk::Picture`), or read-only monospace text
//! with line numbers from `Core::quick_look_text()` coloured with
//! `tl_ffi::highlight` for its `language`; fetched again only when
//! `revision` moves; `footer` and `shown` in a status line like a pane's.
//! It has no input of its own: it follows the cursor, and
//! `Command::ToggleQuickLook` turns it off.
//!
//! Called by the shell as `QuickLookView::new()` once per side (its `root`
//! sits in that side's stack) and `QuickLookView::update(&QuickLookSnapshot)`
//! with every snapshot that has one for that side. Swift:
//! `macos/.../Areas/QuickLookView.swift`.

use gtk::prelude::*;
use tl_ffi::quick_look::QuickLookSnapshot;

pub struct QuickLookView {
    pub root: gtk::Box,
    status: adw::StatusPage,
    footer: gtk::Label,
}

impl QuickLookView {
    pub fn new() -> QuickLookView {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("tl-pane");
        root.add_css_class("view");
        let status = adw::StatusPage::builder()
            .icon_name("view-reveal-symbolic")
            .description("The preview is not available in the GTK app yet.")
            .vexpand(true)
            .build();
        status.add_css_class("compact");
        let footer = gtk::Label::new(None);
        footer.add_css_class("tl-status");
        footer.add_css_class("dim-label");
        footer.set_xalign(0.0);
        footer.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        root.append(&status);
        root.append(&footer);
        QuickLookView { root, status, footer }
    }

    pub fn update(&self, preview: &QuickLookSnapshot) {
        let title = if preview.name.is_empty() { "Nothing to preview" } else { &preview.name };
        self.status.set_title(title);
        self.footer.set_text(&preview.footer);
    }
}
