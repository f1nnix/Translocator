//! Translocator: a dual-pane file manager whose remote locations are
//! plain shells reached through the system's OpenSSH client.

mod actions;
mod app_state;
mod bookmarks;
mod bridge;
mod dialogs;
mod editor;
mod jobs;
mod pane;
mod properties;
mod util;
mod workspace;

use gpui_kit::component::{Theme, TitleBar};
use gpui_kit::*;

/// Density and fonts for a file manager (the component defaults are
/// sized for forms). Re-applied after theme mode changes.
pub fn apply_theme_tweaks(cx: &mut App) {
    Theme::update(cx, |t| {
        t.font_size = px(13.);
        t.mono_font_size = px(12.);
        t.radius = px(5.);
        t.radius_lg = px(8.);
    });
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
            Theme::sync_system_appearance(None, cx);
            apply_theme_tweaks(cx);
            app_state::init(cx);
            actions::bind_keys(cx);
            cx.on_action(|_: &actions::Quit, cx: &mut App| cx.quit());
            actions::set_menus(cx);

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1280.), px(820.)), cx)),
                window_min_size: Some(size(px(760.), px(480.))),
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
