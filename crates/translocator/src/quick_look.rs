//! Quick look (⌘Y): the preview drawn in place of the pane opposite the one
//! with the keyboard.
//!
//! Called by the workspace ([`crate::workspace`]) on every render while
//! `view.quick_look` is `Some`, for the side that is not `view.active`. The
//! core follows the cursor, waits for it to settle, reads the file and puts
//! what to show in the [`QuickLookView`]; this only draws it. What has to be
//! built from the body (the decoded image, the text widget) is kept across
//! renders and rebuilt only when the view's revision changes.

use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{prelude::*, *};
use tl_app::quick_look::{Body, ImageFormat as Format, QuickLookView};

use crate::util::{entry_icon, format_date_long};

/// What was built from the body of revision `revision`.
struct Shown {
    revision: Option<u64>,
    image: Option<Arc<Image>>,
    text: Option<Entity<EditorState>>,
}

pub fn render(view: &QuickLookView, window: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    let shown = window.use_keyed_state("quick-look", cx, |_, _| Shown {
        revision: None,
        image: None,
        text: None,
    });
    if shown.read(cx).revision != Some(view.revision) {
        let (image, text) = match &view.body {
            Body::Image { format, bytes } => {
                (Some(Arc::new(Image::from_bytes(image_format(*format), bytes.to_vec()))), None)
            }
            Body::Text { text, language, .. } => {
                let (text, language) = (text.to_string(), *language);
                let state = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(language)
                        .line_number(true)
                        .soft_wrap(false)
                        .default_value(text)
                });
                (None, Some(state))
            }
            _ => (None, None),
        };
        // No notify: this render is drawing it already.
        shown.update(cx, |shown, _| {
            *shown = Shown {
                revision: Some(view.revision),
                image,
                text,
            }
        });
    }
    let (image, text) = {
        let shown = shown.read(cx);
        (shown.image.clone(), shown.text.clone())
    };

    let hint: SharedString = match crate::keymap::shortcut("quick-look", cx) {
        Some(keys) => format!("Quick Look · {keys}").into(),
        None => "Quick Look".into(),
    };
    let header = h_flex()
        .h(px(32.))
        .flex_shrink_0()
        .px_2()
        .gap_2()
        .border_b_1()
        .border_color(theme.border)
        .bg(theme.title_bar)
        .child(
            Icon::new(view.entry.as_ref().map_or(Lucide::Eye, entry_icon))
                .small()
                .text_color(theme.muted_foreground),
        )
        .child(div().flex_1().min_w_0().truncate().text_sm().child(view.name.clone()))
        .child(div().flex_shrink_0().text_xs().text_color(theme.muted_foreground).child(hint));

    let body = match (view.body.message(), image, text) {
        (Some((message, danger)), _, _) => v_flex()
            .size_full()
            .justify_center()
            .items_center()
            .text_sm()
            .text_color(if danger { theme.danger } else { theme.muted_foreground })
            .child(SharedString::from(message.to_string()))
            .into_any_element(),
        (None, Some(image), _) => div().size_full().p_2().child(img(image).size_full()).into_any_element(),
        (None, None, Some(state)) => div()
            .size_full()
            .child(Editor::new(&state).h_full().readonly(true).bordered(false))
            .into_any_element(),
        (None, None, None) => div().size_full().into_any_element(),
    };

    let details = view
        .footer
        .as_ref()
        .map(|footer| footer.text(&format_date_long(footer.mtime)))
        .unwrap_or_default();
    let footer = h_flex()
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
        .children(view.shown.clone().map(|shown| div().flex_shrink_0().child(shown)));

    v_flex()
        .size_full()
        .bg(theme.background)
        .child(header)
        .child(div().flex_1().min_h_0().child(body))
        .child(footer)
        .into_any_element()
}

fn image_format(format: Format) -> ImageFormat {
    match format {
        Format::Png => ImageFormat::Png,
        Format::Jpeg => ImageFormat::Jpeg,
        Format::Webp => ImageFormat::Webp,
        Format::Gif => ImageFormat::Gif,
        Format::Svg => ImageFormat::Svg,
        Format::Bmp => ImageFormat::Bmp,
        Format::Tiff => ImageFormat::Tiff,
        Format::Ico => ImageFormat::Ico,
        Format::Pnm => ImageFormat::Pnm,
    }
}
