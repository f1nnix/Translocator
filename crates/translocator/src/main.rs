//! Translocator: a dual-pane file manager whose remote locations are
//! plain shells reached through the system's OpenSSH client.

mod actions;
mod app_state;
mod bookmarks;
mod bridge;
mod dialogs;
mod editor;
mod jobs;
mod keymap;
mod pane;
mod properties;
mod rename;
mod settings;
mod settings_window;
mod sync;
mod terminal;
mod themes;
#[cfg(test)]
mod ui_tests;
mod util;
mod workspace;

use gpui_kit::component::TitleBar;
use gpui_kit::*;

/// Matches the basename of `packaging/translocator.desktop`: that is how a
/// Linux desktop finds this window's icon, through `app_id` on Wayland and
/// `WM_CLASS` on X11.
const APP_ID: &str = "translocator";

/// X11 takes a window's icon from the window itself, as pixels. Wayland and
/// the desktop menus use [`APP_ID`] and the installed icon instead, and macOS
/// takes it from the .app bundle.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn window_icon() -> Option<std::sync::Arc<image::RgbaImage>> {
    const PNG: &[u8] = include_bytes!("../../../packaging/translocator-256.png");
    match image::load_from_memory_with_format(PNG, image::ImageFormat::Png) {
        Ok(icon) => Some(std::sync::Arc::new(icon.into_rgba8())),
        Err(err) => {
            log::error!("cannot decode the window icon: {err}");
            None
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn window_icon() -> Option<std::sync::Arc<image::RgbaImage>> {
    None
}

fn main() {
    // `ssh` runs this executable as its SSH_ASKPASS helper: forward the
    // prompt to the running app instead of starting the UI.
    if std::env::var_os(tl_core::askpass::ENV_SOCKET).is_some() {
        let prompt = std::env::args().nth(1).unwrap_or_default();
        std::process::exit(tl_core::askpass::run_client(&prompt));
    }

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);
            app_state::init(cx);
            themes::register(cx);
            themes::apply(None, cx);
            // Builds the key map from the settings, and the menus from it.
            keymap::apply(cx);
            cx.on_action(|_: &actions::Quit, cx: &mut App| cx.quit());
            // Bound without a context, so ⌘, works from any window.
            cx.on_action(|_: &actions::OpenSettings, cx: &mut App| settings_window::open(cx));

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1280.), px(820.)), cx)),
                window_min_size: Some(size(px(760.), px(480.))),
                app_id: Some(APP_ID.to_string()),
                icon: window_icon(),
                ..TitleBar::window_options()
            };
            match gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| workspace::Workspace::new(window, cx))
            }) {
                Ok((handle, _)) => cx.global_mut::<app_state::AppState>().main_window = Some(handle),
                Err(err) => {
                    log::error!("cannot open the main window: {err}");
                    cx.quit();
                }
            }
            cx.activate(true);
        });
}
