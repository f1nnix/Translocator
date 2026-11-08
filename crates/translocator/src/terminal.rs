//! Opening a terminal window, per platform.
//!
//! macOS has one answer (Terminal.app, driven through a temporary
//! `.command` script so no automation permission is needed). Linux has
//! many, so we try the user's `$TERMINAL`, then the Debian/Fedora
//! alternatives, then the emulators people actually run — each with the
//! flag it uses to take a command.
//!
//! The settings can name one, which comes before all of that: on macOS an
//! application for `open -a`, elsewhere a program on `$PATH`.

use std::io;
use std::path::Path;
use std::process::Command;

/// Opens a terminal window: in `cwd`, running `argv` if given (otherwise a
/// login shell). `preferred` is the terminal from the settings, if any.
pub fn open(cwd: Option<&Path>, argv: Option<&[String]>, preferred: Option<&str>) -> io::Result<()> {
    let preferred = preferred.map(str::trim).filter(|t| !t.is_empty());
    #[cfg(target_os = "macos")]
    return macos::open(cwd, argv, preferred);
    #[cfg(not(target_os = "macos"))]
    return unix::open(cwd, argv, preferred);
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    pub fn open(
        cwd: Option<&Path>,
        argv: Option<&[String]>,
        app: Option<&str>,
    ) -> io::Result<()> {
        let mut cmd = Command::new("open");
        cmd.args(["-a", app.unwrap_or("Terminal")]);
        match argv {
            // Terminal.app takes a file to run, not a command line.
            Some(argv) => cmd.arg(script_for(argv, cwd)?),
            None => cmd.arg(cwd.unwrap_or_else(|| Path::new("."))),
        };
        cmd.spawn().map(drop)
    }

    /// Writes a self-deleting script that runs `argv`.
    fn script_for(argv: &[String], cwd: Option<&Path>) -> io::Result<std::path::PathBuf> {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("Translocator");
        std::fs::create_dir_all(&dir)?;
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = dir.join(format!("terminal-{}-{n}.command", std::process::id()));
        let line = argv
            .iter()
            .map(|a| tl_core::shell::quote_str(a))
            .collect::<Vec<_>>()
            .join(" ");
        let cd = match cwd {
            Some(dir) => format!("cd {} || exit 1\n", tl_core::shell::quote_str(&dir.to_string_lossy())),
            None => String::new(),
        };
        std::fs::write(&path, format!("#!/bin/sh\nrm -f \"$0\"\n{cd}exec {line}\n"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        Ok(path)
    }
}

#[cfg(not(target_os = "macos"))]
mod unix {
    use super::*;

    /// Terminal emulators and the flag each uses before a command. An empty
    /// flag means the command follows directly.
    const TERMINALS: &[(&str, &str)] = &[
        // Distribution-managed choices first: they point at whatever the
        // user's desktop installed.
        ("x-terminal-emulator", "-e"),
        ("gnome-terminal", "--"),
        ("ptyxis", "--"),
        ("konsole", "-e"),
        ("xfce4-terminal", "-x"),
        ("mate-terminal", "-x"),
        ("tilix", "-e"),
        ("terminator", "-x"),
        ("alacritty", "-e"),
        ("kitty", ""),
        ("foot", ""),
        ("wezterm", "-e"),
        ("urxvt", "-e"),
        ("st", "-e"),
        ("xterm", "-e"),
    ];

    pub fn open(
        cwd: Option<&Path>,
        argv: Option<&[String]>,
        preferred: Option<&str>,
    ) -> io::Result<()> {
        let preferred = preferred
            .map(str::to_string)
            .or_else(|| std::env::var("TERMINAL").ok().filter(|t| !t.is_empty()));
        let candidates = preferred
            .as_deref()
            .map(|t| (t, flag_for(t)))
            .into_iter()
            .chain(TERMINALS.iter().copied());

        let mut last_err = None;
        for (program, flag) in candidates {
            if which(program).is_none() {
                continue;
            }
            let mut cmd = Command::new(program);
            if let Some(cwd) = cwd {
                cmd.current_dir(cwd);
            }
            if let Some(argv) = argv {
                if !flag.is_empty() {
                    cmd.arg(flag);
                }
                // A login shell keeps the window open if the command ends.
                cmd.args(argv);
            }
            match cmd.spawn() {
                Ok(_) => return Ok(()),
                Err(err) => last_err = Some(err),
            }
        }
        Err(last_err.unwrap_or_else(|| {
            io::Error::other(
                "no terminal emulator found. Set $TERMINAL, or install one (gnome-terminal, konsole, alacritty, …)",
            )
        }))
    }

    fn flag_for(program: &str) -> &'static str {
        let name = program.rsplit('/').next().unwrap_or(program);
        TERMINALS
            .iter()
            .find(|(t, _)| *t == name)
            .map(|(_, flag)| *flag)
            .unwrap_or("-e")
    }

    /// First match for `program` on `$PATH`.
    fn which(program: &str) -> Option<std::path::PathBuf> {
        if program.contains('/') {
            let path = std::path::PathBuf::from(program);
            return path.is_file().then_some(path);
        }
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(program))
                .find(|candidate| candidate.is_file())
        })
    }
}
