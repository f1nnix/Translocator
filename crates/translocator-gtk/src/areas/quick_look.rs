//! Quick look (Ctrl+Y), drawn in place of the pane opposite the active one
//! while `Snapshot.quick_look` is set ([`QuickLookSnapshot`],
//! `tl_ffi::quick_look`).
//!
//! A header with the entry's icon, its name and "Quick Look · Ctrl+Y"; the
//! body by `body`: a centred status page (the message, or the error in
//! the error colour), an image decoded off the main thread from
//! `Core::quick_look_image()` and scaled down to fit, a PDF from
//! `Core::quick_look_pdf()` with its pages in a column ([`PdfView`]), or
//! read-only
//! coloured text with line numbers from `Core::quick_look_text()` in a
//! [`CodeView`]; the footer (`footer`, and `shown` on the right) as a
//! pane's status line. The body is fetched again only when `revision`
//! moves. It has no input of its own: it follows the cursor, and
//! `Command::ToggleQuickLook` turns it off.
//!
//! Called by the shell as `QuickLookView::new()` once per side (its `root`
//! sits in that side's stack) and `QuickLookView::update(&QuickLookSnapshot)`
//! with every snapshot that has one for that side. Swift:
//! `QuickLookView.swift`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gdk_pixbuf, gio, glib};
use tl_ffi::quick_look::{QuickLookBody, QuickLookSnapshot};

use super::code::CodeView;
use super::pdf::PdfView;
use crate::file_list;
use crate::shell;

/// Images are decoded at most this many pixels on a side.
const IMAGE_MAX: i32 = 2048;

pub struct QuickLookView {
    pub root: gtk::Box,
    icon: gtk::Image,
    name: gtk::Label,
    shortcut: gtk::Label,
    body: gtk::Stack,
    status: adw::StatusPage,
    picture: gtk::Picture,
    code: CodeView,
    pdf: PdfView,
    footer: gtk::Label,
    shown: gtk::Label,
    /// The snapshot last drawn.
    last: RefCell<Option<QuickLookSnapshot>>,
    /// The image revision decoded or being decoded.
    image_revision: Rc<Cell<Option<u64>>>,
    /// The PDF revision loaded.
    pdf_revision: Cell<Option<u64>>,
}

impl QuickLookView {
    pub fn new() -> QuickLookView {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("tl-pane");
        root.add_css_class("view");
        root.update_property(&[gtk::accessible::Property::Label("Quick Look")]);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.add_css_class("tl-pane-header");
        header.add_css_class("tl-quick-look-header");
        let icon = gtk::Image::from_icon_name("view-reveal-symbolic");
        icon.add_css_class("tl-icon");
        let name = gtk::Label::new(None);
        name.set_xalign(0.0);
        name.set_hexpand(true);
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        name.add_css_class("heading");
        let shortcut = gtk::Label::new(Some("Quick Look"));
        shortcut.add_css_class("dim-label");
        shortcut.add_css_class("caption");
        header.append(&icon);
        header.append(&name);
        header.append(&shortcut);

        let status = adw::StatusPage::builder().vexpand(true).hexpand(true).build();
        status.add_css_class("compact");
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::ScaleDown)
            .can_shrink(true)
            .hexpand(true)
            .vexpand(true)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(8)
            .margin_end(8)
            .build();
        let code = CodeView::new(false);
        code.set_editable(false);
        let pdf = PdfView::new();
        let body = gtk::Stack::new();
        body.set_vexpand(true);
        body.add_named(&status, Some("status"));
        body.add_named(&picture, Some("image"));
        body.add_named(&code.root, Some("text"));
        body.add_named(&pdf.root, Some("pdf"));

        let status_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        status_bar.add_css_class("tl-status");
        let footer = gtk::Label::new(None);
        footer.set_xalign(0.0);
        footer.set_hexpand(true);
        footer.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        let shown = gtk::Label::new(None);
        status_bar.append(&footer);
        status_bar.append(&shown);

        root.append(&header);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&body);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&status_bar);
        QuickLookView {
            root,
            icon,
            name,
            shortcut,
            body,
            status,
            picture,
            code,
            pdf,
            footer,
            shown,
            last: RefCell::new(None),
            image_revision: Rc::new(Cell::new(None)),
            pdf_revision: Cell::new(None),
        }
    }

    pub fn update(&self, preview: &QuickLookSnapshot) {
        let shortcut = shell::with(|s| s.keys.borrow().label("quick-look"));
        let shortcut = shortcut.map_or("Quick Look".to_string(), |k| format!("Quick Look · {k}"));
        if self.shortcut.text() != shortcut {
            self.shortcut.set_text(&shortcut);
        }
        if self.last.borrow().as_ref() == Some(preview) {
            return;
        }
        self.last.replace(Some(preview.clone()));

        match preview.icon {
            Some(kind) => self.icon.set_from_gicon(&file_list::icon(kind)),
            None => self.icon.set_icon_name(Some("view-reveal-symbolic")),
        }
        self.name.set_text(&preview.name);
        self.footer.set_text(&preview.footer);
        self.shown.set_text(preview.shown.as_deref().unwrap_or(""));
        let label = if preview.name.is_empty() {
            "Quick Look".to_string()
        } else {
            format!("Quick Look: {}", preview.name)
        };
        self.root.update_property(&[gtk::accessible::Property::Label(&label)]);

        // A document is let go of as soon as something else is shown.
        if preview.body != QuickLookBody::Pdf && self.pdf_revision.take().is_some() {
            self.pdf.clear();
        }
        if let Some((message, error)) = preview.body.message() {
            self.show_message(preview, message, error);
            return;
        }
        match &preview.body {
            QuickLookBody::Text { language } => {
                if self.code.revision() != Some(preview.revision) {
                    self.code.load(shell::core().quick_look_text(), language, preview.revision);
                }
                self.body.set_visible_child_name("text");
            }
            QuickLookBody::Image { .. } => self.show_image(preview),
            QuickLookBody::Pdf => self.show_pdf(preview),
            _ => {}
        }
    }

    fn show_message(&self, preview: &QuickLookSnapshot, message: &str, error: bool) {
        let icon = match (&preview.body, preview.icon) {
            (QuickLookBody::Error { .. }, _) => "dialog-warning-symbolic".to_string(),
            (QuickLookBody::Nothing, _) | (_, None) => "view-reveal-symbolic".to_string(),
            (_, Some(kind)) => file_list::icon(kind)
                .names()
                .first()
                .map_or("text-x-generic-symbolic".to_string(), |n| n.to_string()),
        };
        self.status.set_icon_name(Some(&icon));
        self.status.set_title(message);
        if error {
            self.status.add_css_class("error");
        } else {
            self.status.remove_css_class("error");
        }
        self.body.set_visible_child_name("status");
    }

    /// Loads the PDF when its revision is new; one poppler cannot read says
    /// so in place of the pages.
    fn show_pdf(&self, preview: &QuickLookSnapshot) {
        if self.pdf_revision.get() != Some(preview.revision) {
            self.pdf_revision.set(Some(preview.revision));
            if !self.pdf.load(shell::core().quick_look_pdf()) {
                self.status.set_icon_name(Some("x-office-document-symbolic"));
                self.status.set_title("This PDF cannot be shown");
                self.status.remove_css_class("error");
                self.body.set_visible_child_name("status");
                return;
            }
        } else if self.pdf.is_empty() {
            return;
        }
        self.body.set_visible_child_name("pdf");
    }

    /// Decodes the image on a worker thread (scaled to at most
    /// [`IMAGE_MAX`] on a side, the right way up); shows it if the preview
    /// is still on that revision.
    fn show_image(&self, preview: &QuickLookSnapshot) {
        let revision = preview.revision;
        if self.image_revision.get() == Some(revision) {
            self.body.set_visible_child_name("image");
            return;
        }
        self.image_revision.set(Some(revision));
        self.picture.set_paintable(None::<&gdk::Paintable>);
        self.body.set_visible_child_name("image");
        let bytes = shell::core().quick_look_image();
        let work = gio::spawn_blocking(move || decode(&bytes));
        let wanted = self.image_revision.clone();
        let picture = self.picture.clone();
        let body = self.body.clone();
        let status = self.status.clone();
        let name = preview.name.clone();
        glib::spawn_future_local(async move {
            let decoded = work.await.ok().flatten();
            if wanted.get() != Some(revision) || body.visible_child_name().as_deref() != Some("image") {
                return;
            }
            match decoded {
                Some(image) => {
                    let format = if image.alpha {
                        gdk::MemoryFormat::R8g8b8a8
                    } else {
                        gdk::MemoryFormat::R8g8b8
                    };
                    let texture = gdk::MemoryTexture::new(image.width, image.height, format, &image.pixels, image.stride);
                    picture.set_paintable(Some(&texture));
                    picture.set_alternative_text(Some(&format!("Image {name}")));
                }
                None => {
                    status.set_icon_name(Some("image-missing-symbolic"));
                    status.set_title("This image cannot be shown");
                    status.remove_css_class("error");
                    body.set_visible_child_name("status");
                }
            }
        });
    }
}

/// Decoded pixels, which can cross threads (a pixbuf cannot).
struct Decoded {
    width: i32,
    height: i32,
    stride: usize,
    alpha: bool,
    pixels: glib::Bytes,
}

/// The bytes as an image no larger than [`IMAGE_MAX`] on a side, rotated as
/// its EXIF says; `None` when no loader reads it.
fn decode(bytes: &[u8]) -> Option<Decoded> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    loader.connect_size_prepared(|loader, width, height| {
        let longest = width.max(height);
        if longest > IMAGE_MAX {
            let scale = f64::from(IMAGE_MAX) / f64::from(longest);
            let w = ((f64::from(width) * scale).round() as i32).max(1);
            let h = ((f64::from(height) * scale).round() as i32).max(1);
            loader.set_size(w, h);
        }
    });
    let written = loader.write(bytes).and_then(|_| loader.close());
    if let Err(err) = written {
        log::debug!("quick look image: {err}");
        return None;
    }
    let pixbuf = loader.pixbuf()?;
    let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
    if pixbuf.bits_per_sample() != 8 || pixbuf.n_channels() != if pixbuf.has_alpha() { 4 } else { 3 } {
        return None;
    }
    Some(Decoded {
        width: pixbuf.width(),
        height: pixbuf.height(),
        stride: pixbuf.rowstride() as usize,
        alpha: pixbuf.has_alpha(),
        pixels: pixbuf.read_pixel_bytes(),
    })
}
