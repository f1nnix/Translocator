//! A PDF in quick look: every page in one scrolling column, fitted to the
//! width, drawn by poppler. Swift: `PdfPreview` in `QuickLookView.swift`.
//!
//! The pages are rows of a `ListView`, so only those on screen are ever
//! drawn, however long the document. Each page is a [`PdfPage`] paintable:
//! its intrinsic size is the page's size in points, which gives a
//! `Picture` its height for the width it is given, and it draws the page
//! as vectors at whatever size that turns out to be.

use std::cell::{OnceCell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib, graphene};

mod page_imp {
    use super::*;

    #[derive(Default)]
    pub struct PdfPage {
        pub page: OnceCell<poppler::Page>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PdfPage {
        const NAME: &'static str = "TlPdfPage";
        type Type = super::PdfPage;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for PdfPage {}

    impl PdfPage {
        /// The page's size in points; a page that reports none counts as
        /// A4, so nothing divides by zero.
        pub fn points(&self) -> (f64, f64) {
            match self.page.get().map(poppler::Page::size) {
                Some((w, h)) if w > 0.0 && h > 0.0 => (w, h),
                _ => (595.0, 842.0),
            }
        }
    }

    impl PaintableImpl for PdfPage {
        fn intrinsic_width(&self) -> i32 {
            self.points().0.ceil() as i32
        }

        fn intrinsic_height(&self) -> i32 {
            self.points().1.ceil() as i32
        }

        fn intrinsic_aspect_ratio(&self) -> f64 {
            let (w, h) = self.points();
            w / h
        }

        fn flags(&self) -> gdk::PaintableFlags {
            gdk::PaintableFlags::STATIC_SIZE | gdk::PaintableFlags::STATIC_CONTENTS
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let Some(snapshot) = snapshot.downcast_ref::<gtk::Snapshot>() else { return };
            let bounds = graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
            // Paper is white whatever the theme; the page draws on top.
            snapshot.append_color(&gdk::RGBA::WHITE, &bounds);
            let Some(page) = self.page.get() else { return };
            let (w, h) = self.points();
            let cr = snapshot.append_cairo(&bounds);
            cr.scale(width / w, height / h);
            page.render(&cr);
        }
    }
}

glib::wrapper! {
    /// One page of a document, as something a `Picture` can show.
    pub struct PdfPage(ObjectSubclass<page_imp::PdfPage>) @implements gdk::Paintable;
}

impl PdfPage {
    fn new(page: poppler::Page) -> PdfPage {
        let paintable: PdfPage = glib::Object::new();
        let _ = paintable.imp().page.set(page);
        paintable
    }
}

/// Space between the pages and around them.
const GAP: i32 = 8;

pub struct PdfView {
    pub root: gtk::ScrolledWindow,
    pages: gio::ListStore,
    /// Kept for as long as its pages are shown: they draw from it.
    document: RefCell<Option<poppler::Document>>,
}

impl PdfView {
    pub fn new() -> PdfView {
        let pages = gio::ListStore::new::<PdfPage>();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
            let picture = gtk::Picture::builder()
                .content_fit(gtk::ContentFit::Contain)
                .can_shrink(true)
                .hexpand(true)
                .margin_start(GAP)
                .margin_end(GAP)
                .margin_top(GAP / 2)
                .margin_bottom(GAP / 2)
                .build();
            picture.add_css_class("tl-pdf-page");
            item.set_child(Some(&picture));
            item.set_activatable(false);
            item.set_selectable(false);
        });
        factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
            let Some(picture) = item.child().and_downcast::<gtk::Picture>() else { return };
            let page = item.item().and_downcast::<PdfPage>();
            picture.set_paintable(page.as_ref());
            picture.set_alternative_text(Some(&format!("Page {}", item.position() + 1)));
        });
        let list = gtk::ListView::new(Some(gtk::NoSelection::new(Some(pages.clone()))), Some(factory));
        list.add_css_class("tl-pdf");
        // Each page gets the height its width calls for, not the least it
        // could live with (which, for a picture that may shrink, is none).
        list.set_vscroll_policy(gtk::ScrollablePolicy::Natural);
        list.set_margin_top(GAP / 2);
        list.set_margin_bottom(GAP / 2);
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .hexpand(true)
            .child(&list)
            .build();
        PdfView {
            root,
            pages,
            document: RefCell::new(None),
        }
    }

    /// Shows `bytes`, from the first page; `false` (and nothing shown)
    /// when poppler cannot read them or they hold no pages.
    pub fn load(&self, bytes: Vec<u8>) -> bool {
        self.pages.remove_all();
        self.document.replace(None);
        let document = match poppler::Document::from_bytes(&glib::Bytes::from_owned(bytes), None) {
            Ok(document) => document,
            Err(err) => {
                log::debug!("quick look pdf: {err}");
                return false;
            }
        };
        let pages: Vec<PdfPage> = (0..document.n_pages()).filter_map(|i| document.page(i)).map(PdfPage::new).collect();
        if pages.is_empty() {
            return false;
        }
        self.pages.extend_from_slice(&pages);
        self.document.replace(Some(document));
        self.root.vadjustment().set_value(0.0);
        true
    }

    /// Whether no document is loaded.
    pub fn is_empty(&self) -> bool {
        self.document.borrow().is_none()
    }

    /// Lets go of the document while something else is shown.
    pub fn clear(&self) {
        self.pages.remove_all();
        self.document.replace(None);
    }
}
