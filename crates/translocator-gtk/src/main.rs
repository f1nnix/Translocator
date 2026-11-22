//! Translocator's GTK 4 and libadwaita shell, for Linux.
//!
//! Like the macOS app, it is a presentation layer over the shared Crux
//! core: `tl-ffi` starts the core and its runtime, takes the shell's inputs
//! (`Input`) and hands back small snapshots and display-ready rows. Every
//! decision is the core's; this crate turns snapshots into widgets and
//! widget events into inputs.
//!
//! The pieces:
//! - [`host`]: the core's `Host`, which hops to the GLib main loop;
//! - [`shell`]: the one window and its state, the snapshot loop;
//! - [`pane`] and [`file_list`]: a pane and its virtualized file list;
//! - [`keys`], [`actions`], [`menus`]: the shared key table as GActions,
//!   accelerators and menus;
//! - [`bottom`], [`header`]: the console, command line, jobs and function
//!   bar, and the header bar;
//! - [`dialogs`]: the generic dialogs; [`areas`] the area dialogs and
//!   windows (stubs for now);
//! - [`theme`]: dark and light, and the content colour schemes.

#[cfg(not(target_os = "macos"))]
mod actions;
#[cfg(not(target_os = "macos"))]
mod areas;
#[cfg(not(target_os = "macos"))]
mod bottom;
#[cfg(not(target_os = "macos"))]
mod dialogs;
#[cfg(not(target_os = "macos"))]
mod file_list;
#[cfg(not(target_os = "macos"))]
mod header;
#[cfg(not(target_os = "macos"))]
mod host;
#[cfg(not(target_os = "macos"))]
mod keys;
#[cfg(not(target_os = "macos"))]
mod menus;
#[cfg(not(target_os = "macos"))]
mod pane;
#[cfg(not(target_os = "macos"))]
mod shell;
#[cfg(not(target_os = "macos"))]
mod theme;

fn main() {
    // `ssh` runs this executable as its SSH_ASKPASS helper: forward the
    // prompt to the running app instead of starting the UI.
    if std::env::var_os(tl_ffi::askpass_socket_env()).is_some() {
        let prompt = std::env::args().nth(1).unwrap_or_default();
        std::process::exit(tl_ffi::askpass_client(prompt));
    }

    #[cfg(not(target_os = "macos"))]
    std::process::exit(shell::run());

    #[cfg(target_os = "macos")]
    {
        log::error!("translocator-gtk is the Linux app; on macOS, build the native app in macos/");
        eprintln!("translocator-gtk is the Linux app; on macOS, build the native app in macos/");
        std::process::exit(1);
    }
}
