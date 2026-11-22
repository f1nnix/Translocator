//! AREA: quick look (⌘Y), the preview drawn in place of the pane opposite
//! the active one.
//!
//! Owns [`QuickLookSnapshot`], carried in `Snapshot.quick_look` while the
//! preview is on. Quick look has no input of its own: it follows the
//! cursor, and ⌘Y (`Command::ToggleQuickLook`) turns it off.
//!
//! The snapshot says what kind of body there is; the body itself (text up
//! to 256 KB, image bytes up to 8 MB) is fetched with
//! [`Core::quick_look_text`] or [`Core::quick_look_image`] only when
//! `revision` moves. The Swift side is
//! `macos/Sources/Translocator/Areas/QuickLookView.swift`.

use tl_app::quick_look::{Body, ImageFormat as CoreFormat, QuickLookView};

use crate::Core;
use crate::rows::IconKind;
use crate::types::Side;

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct QuickLookSnapshot {
    /// Drawn in place of this pane: the one opposite the keyboard.
    pub side: Side,
    /// The file previewed; empty when there is none.
    pub name: String,
    /// The header's icon; `None` (no file) shows an eye.
    pub icon: Option<IconKind>,
    pub body: QuickLookBody,
    /// Changes whenever the body changes: fetch the text or the image
    /// again then.
    pub revision: u64,
    /// `{size or folder} · {date} · user:group · {mode}`; empty when there
    /// is no file.
    pub footer: String,
    /// The right of the footer: `first 256 KB of 3 MB`.
    pub shown: Option<String>,
}

/// What the preview's body shows.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq, Hash)]
pub enum QuickLookBody {
    /// No file under the cursor: "Nothing to preview".
    Nothing,
    /// "Reading…".
    Reading,
    /// What the listing says instead ("Folder", "Binary file").
    Note { message: String },
    /// Bytes to decode (`Core::quick_look_image`), scaled to fit.
    Image { format: ImageFormat },
    /// Read-only text with line numbers, no wrapping
    /// (`Core::quick_look_text`), coloured as `language`.
    Text { language: String },
    /// Drawn in the danger colour.
    Error { message: String },
}

impl QuickLookBody {
    /// The line in the middle of the preview and whether it is an error;
    /// `None` for an image or text.
    pub fn message(&self) -> Option<(&str, bool)> {
        match self {
            QuickLookBody::Nothing => Some(("Nothing to preview", false)),
            QuickLookBody::Reading => Some(("Reading…", false)),
            QuickLookBody::Note { message } => Some((message, false)),
            QuickLookBody::Error { message } => Some((message, true)),
            QuickLookBody::Image { .. } | QuickLookBody::Text { .. } => None,
        }
    }
}

/// The image formats the core recognises by extension.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

impl From<CoreFormat> for ImageFormat {
    fn from(format: CoreFormat) -> Self {
        match format {
            CoreFormat::Png => ImageFormat::Png,
            CoreFormat::Jpeg => ImageFormat::Jpeg,
            CoreFormat::Webp => ImageFormat::Webp,
            CoreFormat::Gif => ImageFormat::Gif,
            CoreFormat::Svg => ImageFormat::Svg,
            CoreFormat::Bmp => ImageFormat::Bmp,
            CoreFormat::Tiff => ImageFormat::Tiff,
            CoreFormat::Ico => ImageFormat::Ico,
            CoreFormat::Pnm => ImageFormat::Pnm,
        }
    }
}

pub(crate) fn snapshot(view: &QuickLookView) -> QuickLookSnapshot {
    let body = match &view.body {
        Body::Nothing => QuickLookBody::Nothing,
        Body::Reading => QuickLookBody::Reading,
        Body::Note(message) => QuickLookBody::Note { message: message.clone() },
        Body::Image { format, .. } => QuickLookBody::Image { format: (*format).into() },
        Body::Text { language, .. } => QuickLookBody::Text {
            language: language.to_string(),
        },
        Body::Error(message) => QuickLookBody::Error { message: message.clone() },
    };
    QuickLookSnapshot {
        side: view.side.into(),
        name: view.name.clone(),
        icon: view.entry.as_ref().map(crate::rows::icon),
        body,
        revision: view.revision,
        footer: view
            .footer
            .as_ref()
            .map(|f| f.text(&crate::format::date_long(f.mtime)))
            .unwrap_or_default(),
        shown: view.shown.clone(),
    }
}

#[uniffi::export]
impl Core {
    /// The preview's text as of the last snapshot; empty unless its body
    /// is `Text`.
    pub fn quick_look_text(&self) -> String {
        match self.view().quick_look.as_ref().map(|q| &q.body) {
            Some(Body::Text { text, .. }) => text.to_string(),
            _ => String::new(),
        }
    }

    /// The preview's image bytes as of the last snapshot; empty unless its
    /// body is `Image`.
    pub fn quick_look_image(&self) -> Vec<u8> {
        match self.view().quick_look.as_ref().map(|q| &q.body) {
            Some(Body::Image { bytes, .. }) => bytes.as_ref().clone(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tl_app::quick_look::Footer;
    use tl_app::tl_model::{Entry, EntryKind};

    use super::*;

    fn view(body: Body) -> QuickLookView {
        let mut entry = Entry::new("main.rs", EntryKind::File);
        entry.size = 3000;
        QuickLookView {
            side: tl_app::Side::Right,
            entry: Some(entry),
            name: "main.rs".into(),
            body,
            revision: 5,
            footer: Some(Footer {
                kind: "2.9 KB".into(),
                mtime: None,
                owner: Some("me:staff".into()),
                mode: "-rw-r--r--".into(),
            }),
            shown: Some("first 256 KB of 3 MB".into()),
        }
    }

    #[test]
    fn bodies_and_footer_map() {
        let text = Body::Text {
            text: Arc::from("fn main() {}"),
            language: "rust",
            shown: None,
        };
        let s = snapshot(&view(text));
        assert_eq!(s.side, Side::Right);
        assert_eq!(s.name, "main.rs");
        assert_eq!(s.icon, Some(IconKind::Code));
        assert_eq!(s.body, QuickLookBody::Text { language: "rust".into() });
        assert_eq!(s.revision, 5);
        assert_eq!(s.footer, "2.9 KB ·  · me:staff · -rw-r--r--");
        assert_eq!(s.shown.as_deref(), Some("first 256 KB of 3 MB"));
        assert_eq!(s.body.message(), None);

        let image = Body::Image {
            format: CoreFormat::Png,
            bytes: Arc::new(vec![1, 2, 3]),
        };
        assert_eq!(snapshot(&view(image)).body, QuickLookBody::Image { format: ImageFormat::Png });
        let error = snapshot(&view(Body::Error("denied".into()))).body;
        assert_eq!(error.message(), Some(("denied", true)));
        assert_eq!(snapshot(&view(Body::Reading)).body.message(), Some(("Reading…", false)));
        assert_eq!(snapshot(&view(Body::Note("Folder".into()))).body.message(), Some(("Folder", false)));

        let mut none = view(Body::Nothing);
        none.entry = None;
        none.footer = None;
        none.name.clear();
        let s = snapshot(&none);
        assert_eq!((s.icon, s.footer.as_str()), (None, ""));
        assert_eq!(s.body.message(), Some(("Nothing to preview", false)));
    }

    #[test]
    fn the_footer_writes_the_date_in_local_time() {
        let mut v = view(Body::Nothing);
        let when = 1_792_247_520;
        v.footer.as_mut().unwrap().mtime = Some(when);
        let footer = snapshot(&v).footer;
        assert!(footer.contains(&crate::format::date_long(Some(when))), "{footer}");
    }
}
