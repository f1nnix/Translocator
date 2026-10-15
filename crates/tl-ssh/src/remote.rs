//! Running POSIX shell scripts on the server over the control connection.
//!
//! Every operation is a short `sh` script. The script is wrapped so it
//! survives any login shell (bash, zsh, fish, csh...), optionally runs under
//! sudo, and prints a marker first: anything a noisy `.bashrc` prints before
//! the marker is discarded, so outputs (even binary tar streams) are clean.

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use base64::Engine as _;
use parking_lot::Mutex;
use tl_core::shell::quote;
use tl_core::{CancelGuard, CancelToken, Prompter, TarSink, TarSource, VfsError, VfsResult};

use crate::master::detach;

/// Printed by every script before its real output.
pub const MARKER: &[u8] = b"\x1fTLX1\x1f";
const MARKER_PRINTF: &str = "printf '\\037TLX1\\037'";
/// Prefix of structured errors scripts print to stderr (`TLERR:ENOENT`).
pub const TLERR: &str = "TLERR:";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginShell {
    /// sh, bash, zsh, dash, ksh, ash: POSIX quoting works.
    Posix,
    /// fish: single quotes treat `\\` and `\'` as escapes.
    Fish,
    /// csh/tcsh/nushell/unknown: the script travels base64-encoded.
    Exotic,
}

impl LoginShell {
    pub fn from_path(shell: &str) -> Self {
        let name = shell.trim().rsplit('/').next().unwrap_or("");
        match name {
            "" | "sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "ash" | "busybox" | "rbash"
            | "yash" | "posh" => LoginShell::Posix,
            "fish" => LoginShell::Fish,
            _ => LoginShell::Exotic,
        }
    }
}

#[derive(Clone)]
enum Sudo {
    Unknown,
    NoPassword,
    Password(String),
}

#[derive(Clone, Copy, Default)]
pub struct RunOpts<'a> {
    pub elevated: bool,
    /// Bytes fed to the script's stdin.
    pub stdin: Option<&'a [u8]>,
    /// Keep stdin open while the script runs; closing it (on cancel)
    /// makes the script's watchdog kill the remote work. Use with
    /// [`crate::scripts::with_watchdog`].
    pub watchdog: bool,
}

#[derive(Debug, Default)]
pub struct RunOutput {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub status: i32,
}

impl RunOutput {
    pub fn success(&self) -> bool {
        self.status == 0
    }

    /// The `TLERR:` code a script reported, if any.
    pub fn tlerr(&self) -> Option<&str> {
        self.stderr
            .lines()
            .find_map(|l| l.trim().strip_prefix(TLERR))
            .map(str::trim)
    }

    /// Maps a failed script to an error about `what`.
    pub fn error(&self, what: &str) -> VfsError {
        match self.tlerr() {
            Some("ENOENT") => VfsError::NotFound(what.into()),
            Some("EACCES") => VfsError::PermissionDenied(what.into()),
            Some("EEXIST") => VfsError::AlreadyExists(what.into()),
            Some("ENOTDIR") => VfsError::other(format!("Not a folder: {what}")),
            Some("EISDIR") => VfsError::other(format!("Is a folder: {what}")),
            Some("ENOTREG") => VfsError::other(format!("Not a regular file: {what}")),
            Some("SHORT") => VfsError::Connection(format!("Upload of {what} was interrupted")),
            Some(code) => VfsError::other(format!("{what}: {code}")),
            None if self.status == 255 => VfsError::Connection(non_empty(&self.stderr, "Connection lost")),
            None => {
                let text = self.stderr.trim();
                if text.is_empty() {
                    VfsError::other(format!("{what}: command failed with status {}", self.status))
                } else {
                    VfsError::from_tool_output(text)
                }
            }
        }
    }
}

fn non_empty(s: &str, fallback: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        fallback.to_string()
    } else {
        s.to_string()
    }
}

/// A connected server: runs scripts through the master's control socket.
pub struct Remote {
    pub ssh_bin: PathBuf,
    pub control: PathBuf,
    pub host: String,
    pub label: String,
    pub shell: Mutex<LoginShell>,
    pub sudo_user: String,
    sudo: Mutex<Sudo>,
    /// Serializes the sudo password dialog.
    sudo_prompt: Mutex<()>,
    prompter: Arc<dyn Prompter>,
}

impl Remote {
    pub fn new(
        ssh_bin: PathBuf,
        control: PathBuf,
        host: String,
        label: String,
        sudo_user: String,
        prompter: Arc<dyn Prompter>,
    ) -> Self {
        Remote {
            ssh_bin,
            control,
            host,
            label,
            shell: Mutex::new(LoginShell::Posix),
            sudo_user,
            sudo: Mutex::new(Sudo::Unknown),
            sudo_prompt: Mutex::new(()),
            prompter,
        }
    }

    fn ssh(&self) -> Command {
        let mut cmd = Command::new(&self.ssh_bin);
        cmd.arg("-S")
            .arg(&self.control)
            .args([
                "-o",
                "ControlMaster=no",
                "-o",
                "BatchMode=yes",
                "-o",
                "LogLevel=ERROR",
                "-T",
            ])
            .arg(&self.host);
        detach(&mut cmd);
        cmd
    }

    /// Runs a raw command line in the login shell, unwrapped. Only used to
    /// discover which login shell the server uses.
    pub fn run_raw(&self, command: &str) -> VfsResult<RunOutput> {
        let out = self
            .ssh()
            .arg(command)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| VfsError::Connection(e.to_string()))?;
        Ok(RunOutput {
            stdout: out.stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            status: out.status.code().unwrap_or(-1),
        })
    }

    /// Builds the command line for the login shell; returns it with the sudo
    /// password that must be written to stdin first, if any.
    fn command_line(&self, script: &[u8], elevated: bool) -> VfsResult<(Vec<u8>, Option<String>)> {
        let mut inner = format!("LC_ALL=C; export LC_ALL; {MARKER_PRINTF}\n").into_bytes();
        inner.extend_from_slice(script);
        let (outer, password) = if elevated {
            let mode = self.ensure_sudo()?;
            let mut line = b"exec sudo ".to_vec();
            let password = match mode {
                Sudo::NoPassword => {
                    line.extend_from_slice(b"-n ");
                    None
                }
                Sudo::Password(pw) => {
                    // -k: always read the password, so it is never left in
                    // stdin for the command when a cached credential exists.
                    line.extend_from_slice(b"-S -k -p '' ");
                    Some(pw)
                }
                Sudo::Unknown => unreachable!("ensure_sudo resolves the mode"),
            };
            line.extend_from_slice(b"-u ");
            line.extend_from_slice(&quote(self.sudo_user.as_bytes()));
            line.extend_from_slice(b" -- sh -c ");
            line.extend_from_slice(&quote(&inner));
            (line, password)
        } else {
            (inner, None)
        };
        let mut command = b"exec sh -c ".to_vec();
        match *self.shell.lock() {
            LoginShell::Posix => command.extend_from_slice(&quote(&outer)),
            LoginShell::Fish => command.extend_from_slice(&fish_quote(&outer)),
            LoginShell::Exotic => {
                // Only [A-Za-z0-9+/=] travel through the unknown shell.
                let encoded = base64::engine::general_purpose::STANDARD.encode(&outer);
                command.extend_from_slice(b"'eval \"$(echo ");
                command.extend_from_slice(encoded.as_bytes());
                command.extend_from_slice(b" | base64 -d)\"'");
            }
        }
        Ok((command, password))
    }

    fn spawn(
        &self,
        script: &[u8],
        elevated: bool,
        stdin: bool,
    ) -> VfsResult<(Child, Option<String>)> {
        let (line, password) = self.command_line(script, elevated)?;
        let child = self
            .ssh()
            .arg(OsStr::from_bytes(&line))
            .stdin(if stdin || password.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| VfsError::Connection(format!("Cannot run ssh: {e}")))?;
        Ok((child, password))
    }

    pub fn run(&self, script: &[u8], opts: RunOpts, cancel: &CancelToken) -> VfsResult<RunOutput> {
        self.run_streaming(script, opts, cancel, |_| {})
    }

    /// Runs a script; `on_data` sees stdout bytes (after the marker) as
    /// they arrive, e.g. to parse progress lines.
    pub fn run_streaming(
        &self,
        script: &[u8],
        opts: RunOpts,
        cancel: &CancelToken,
        mut on_data: impl FnMut(&[u8]),
    ) -> VfsResult<RunOutput> {
        cancel.check()?;
        let (mut child, password) =
            self.spawn(script, opts.elevated, opts.stdin.is_some() || opts.watchdog)?;
        let pid = child.id() as libc::pid_t;
        let exited = Arc::new(AtomicBool::new(false));

        let stdin_slot: Arc<Mutex<Option<ChildStdin>>> = Arc::new(Mutex::new(None));
        let writer = child.stdin.take().map(|mut stdin| {
            let mut data = Vec::new();
            if let Some(pw) = &password {
                data.extend_from_slice(pw.as_bytes());
                data.push(b'\n');
            }
            data.extend_from_slice(opts.stdin.unwrap_or_default());
            let slot = stdin_slot.clone();
            let keep_open = opts.watchdog;
            std::thread::spawn(move || {
                let result = stdin.write_all(&data);
                if keep_open && result.is_ok() {
                    *slot.lock() = Some(stdin);
                }
                result
            })
        });

        let _guard = {
            let (slot, exited) = (stdin_slot.clone(), exited.clone());
            let watchdog = opts.watchdog;
            cancel.on_cancel(move || {
                // Closing stdin triggers the remote watchdog; the local ssh
                // is killed too in case the connection itself is stuck.
                drop(slot.lock().take());
                let grace = if watchdog { Duration::from_secs(2) } else { Duration::ZERO };
                std::thread::spawn(move || {
                    std::thread::sleep(grace);
                    if !exited.load(Ordering::SeqCst) {
                        unsafe { libc::kill(pid, libc::SIGTERM) };
                    }
                });
            })
        };

        let stderr_reader = child.stderr.take().map(spawn_collector);
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut skipper = MarkerSkipper::default();
        let mut collected = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            if let Some(data) = skipper.feed(&buf[..n])
                && !data.is_empty() {
                    on_data(&data);
                    collected.extend_from_slice(&data);
                }
        }
        let status = child.wait().map_err(|e| VfsError::Connection(e.to_string()))?;
        exited.store(true, Ordering::SeqCst);
        drop(stdin_slot.lock().take());
        if let Some(writer) = writer {
            let _ = writer.join();
        }
        let stderr = stderr_reader
            .map(|h| h.join().unwrap_or_default())
            .unwrap_or_default();
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        let status = status.code().unwrap_or(-1);
        if !skipper.found {
            return Err(self.diagnose(status, &stderr));
        }
        Ok(RunOutput {
            stdout: collected,
            stderr,
            status,
        })
    }

    /// Explains why a script never started (it did not print the marker).
    fn diagnose(&self, status: i32, stderr: &str) -> VfsError {
        let lower = stderr.to_ascii_lowercase();
        let text = non_empty(stderr, &format!("remote shell exited with status {status}"));
        if lower.contains("incorrect password") || lower.contains("sorry, try again") {
            *self.sudo.lock() = Sudo::Unknown;
            VfsError::Auth("Incorrect sudo password".into())
        } else if lower.contains("a password is required") {
            *self.sudo.lock() = Sudo::Unknown;
            VfsError::Auth("sudo needs a password".into())
        } else if lower.contains("sudoers") || lower.contains("not allowed to") || lower.contains("may not run sudo") {
            VfsError::Auth(text)
        } else if lower.contains("must have a tty") || lower.contains("no tty present") {
            VfsError::Unsupported(
                "sudo on this server requires a terminal (Defaults requiretty)".into(),
            )
        } else if status == 255 {
            VfsError::Connection(non_empty(stderr, "Connection lost"))
        } else {
            VfsError::other(text)
        }
    }

    /// Determines how to use sudo, asking for the password if needed.
    fn ensure_sudo(&self) -> VfsResult<Sudo> {
        if let mode @ (Sudo::NoPassword | Sudo::Password(_)) = self.sudo.lock().clone() {
            return Ok(mode);
        }
        let _prompt = self.sudo_prompt.lock();
        if let mode @ (Sudo::NoPassword | Sudo::Password(_)) = self.sudo.lock().clone() {
            return Ok(mode);
        }
        let probe = self.run(b"sudo -k -n true 2>&1 && echo TL_SUDO_OK", RunOpts::default(), &CancelToken::new())?;
        let text = String::from_utf8_lossy(&probe.stdout).into_owned();
        if text.contains("TL_SUDO_OK") {
            *self.sudo.lock() = Sudo::NoPassword;
            return Ok(Sudo::NoPassword);
        }
        let lower = text.to_ascii_lowercase();
        if lower.contains("not found") && lower.contains("sudo") {
            return Err(VfsError::Unsupported("sudo is not installed on the server".into()));
        }
        if !(lower.contains("password") || lower.contains("terminal is required")) {
            return Err(VfsError::Auth(non_empty(&text, "sudo refused")));
        }
        let mut message = format!("[sudo] password for {}", self.label);
        for _attempt in 0..3 {
            let Some(password) = self.prompter.secret("sudo password", &message) else {
                return Err(VfsError::Cancelled);
            };
            let check = self.run(
                b"sudo -S -k -p '' true 2>&1 && echo TL_SUDO_OK",
                RunOpts {
                    stdin: Some(format!("{password}\n").as_bytes()),
                    ..Default::default()
                },
                &CancelToken::new(),
            )?;
            let out = String::from_utf8_lossy(&check.stdout).into_owned();
            if out.contains("TL_SUDO_OK") {
                *self.sudo.lock() = Sudo::Password(password.clone());
                return Ok(Sudo::Password(password));
            }
            let lower = out.to_ascii_lowercase();
            if lower.contains("incorrect") || lower.contains("sorry, try again") {
                message = format!("Sorry, try again.\n[sudo] password for {}", self.label);
                continue;
            }
            return Err(VfsError::Auth(non_empty(&out, "sudo refused")));
        }
        Err(VfsError::Auth("Incorrect sudo password".into()))
    }

    pub fn prepare_sudo(&self) -> VfsResult<()> {
        self.ensure_sudo().map(drop)
    }

    /// Streams stdout of a script (`tar -c`).
    pub fn source(
        self: &Arc<Self>,
        script: &[u8],
        elevated: bool,
        cancel: &CancelToken,
        what: String,
    ) -> VfsResult<RemoteSource> {
        let (mut child, password) = self.spawn(script, elevated, false)?;
        if let (Some(mut stdin), Some(pw)) = (child.stdin.take(), password) {
            let _ = stdin.write_all(format!("{pw}\n").as_bytes());
        }
        let proc = Proc::new(child, cancel);
        let mut stdout = proc.child.lock().stdout.take().expect("piped stdout");
        let stderr = proc.child.lock().stderr.take().map(spawn_collector);
        let mut skipper = MarkerSkipper::default();
        let mut buf = vec![0u8; 16 * 1024];
        let leftover = loop {
            let n = stdout.read(&mut buf).unwrap_or(0);
            if n == 0 {
                let status = proc.wait();
                let stderr = stderr.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
                cancel.check()?;
                return Err(self.diagnose(status, &stderr));
            }
            if let Some(data) = skipper.feed(&buf[..n]) {
                break data;
            }
        };
        Ok(RemoteSource {
            remote: self.clone(),
            proc,
            stdout: Some(stdout),
            leftover,
            pos: 0,
            stderr,
            what,
        })
    }

    /// Feeds stdin of a script (`tar -x`).
    pub fn sink(
        self: &Arc<Self>,
        script: &[u8],
        elevated: bool,
        cancel: &CancelToken,
        what: String,
    ) -> VfsResult<RemoteSink> {
        let (mut child, password) = self.spawn(script, elevated, true)?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        if let Some(pw) = password {
            stdin
                .write_all(format!("{pw}\n").as_bytes())
                .map_err(|e| VfsError::Connection(e.to_string()))?;
        }
        let proc = Proc::new(child, cancel);
        let mut stdout = proc.child.lock().stdout.take().expect("piped stdout");
        let stderr = proc.child.lock().stderr.take().map(spawn_collector);
        // Wait for the marker and the script's `R` (ready): proves sudo
        // accepted us and the destination checks passed before data flows.
        let mut skipper = MarkerSkipper::default();
        let mut after = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = stdout.read(&mut buf).unwrap_or(0);
            if n > 0
                && let Some(data) = skipper.feed(&buf[..n]) {
                    after.extend_from_slice(&data);
                }
            if after.first() == Some(&b'R') {
                break;
            }
            if n == 0 || !after.is_empty() {
                drop(stdin);
                let status = proc.wait();
                let stderr = stderr.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
                cancel.check()?;
                if skipper.found {
                    let out = RunOutput {
                        stdout: Vec::new(),
                        stderr,
                        status,
                    };
                    return Err(out.error(&what));
                }
                return Err(self.diagnose(status, &stderr));
            }
        }
        let drain = std::thread::spawn(move || {
            let _ = io::copy(&mut stdout, &mut io::sink());
        });
        Ok(RemoteSink {
            proc,
            stdin: Some(stdin),
            drain: Some(drain),
            stderr,
            what,
        })
    }
}

/// fish single quotes: only `\\` and `\'` are escapes.
fn fish_quote(s: &[u8]) -> Vec<u8> {
    let mut out = vec![b'\''];
    for &b in s {
        if b == b'\\' || b == b'\'' {
            out.push(b'\\');
        }
        out.push(b);
    }
    out.push(b'\'');
    out
}

fn spawn_collector(mut reader: impl Read + Send + 'static) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut data = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            if data.len() < 256 * 1024 {
                data.extend_from_slice(&buf[..n]);
            }
        }
        String::from_utf8_lossy(&data).into_owned()
    })
}

/// Drops everything before [`MARKER`].
#[derive(Default)]
pub struct MarkerSkipper {
    pending: Vec<u8>,
    pub found: bool,
}

impl MarkerSkipper {
    /// Returns the payload contained in `chunk` once the marker was seen.
    pub fn feed(&mut self, chunk: &[u8]) -> Option<Vec<u8>> {
        if self.found {
            return Some(chunk.to_vec());
        }
        self.pending.extend_from_slice(chunk);
        if let Some(pos) = find(&self.pending, MARKER) {
            self.found = true;
            let rest = self.pending.split_off(pos + MARKER.len());
            self.pending.clear();
            return Some(rest);
        }
        // Keep only a possible partial marker.
        let keep = MARKER.len() - 1;
        if self.pending.len() > keep {
            self.pending.drain(..self.pending.len() - keep);
        }
        None
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A spawned `ssh` that a cancel token can kill.
struct Proc {
    child: Mutex<Child>,
    exited: Arc<AtomicBool>,
    _guard: CancelGuard,
}

impl Proc {
    fn new(child: Child, cancel: &CancelToken) -> Self {
        let pid = child.id() as libc::pid_t;
        let exited = Arc::new(AtomicBool::new(false));
        let flag = exited.clone();
        let guard = cancel.on_cancel(move || {
            if !flag.load(Ordering::SeqCst) {
                unsafe { libc::kill(pid, libc::SIGTERM) };
            }
        });
        Proc {
            child: Mutex::new(child),
            exited,
            _guard: guard,
        }
    }

    fn wait(&self) -> i32 {
        let status = self.child.lock().wait();
        self.exited.store(true, Ordering::SeqCst);
        status.ok().and_then(|s| s.code()).unwrap_or(-1)
    }
}

pub struct RemoteSource {
    remote: Arc<Remote>,
    proc: Proc,
    stdout: Option<ChildStdout>,
    leftover: Vec<u8>,
    pos: usize,
    stderr: Option<JoinHandle<String>>,
    what: String,
}

impl Read for RemoteSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos < self.leftover.len() {
            let n = buf.len().min(self.leftover.len() - self.pos);
            buf[..n].copy_from_slice(&self.leftover[self.pos..self.pos + n]);
            self.pos += n;
            return Ok(n);
        }
        match self.stdout.as_mut() {
            Some(stdout) => stdout.read(buf),
            None => Ok(0),
        }
    }
}

impl TarSource for RemoteSource {
    fn finish(mut self: Box<Self>) -> VfsResult<()> {
        // If the reader stopped early, closing the pipe stops the producer.
        drop(self.stdout.take());
        let status = self.proc.wait();
        let stderr = self
            .stderr
            .take()
            .map(|h| h.join().unwrap_or_default())
            .unwrap_or_default();
        tar_result(status, &stderr, &self.what, &self.remote)
    }
}

pub struct RemoteSink {
    proc: Proc,
    stdin: Option<ChildStdin>,
    drain: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<String>>,
    what: String,
}

impl Write for RemoteSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.stdin.as_mut() {
            Some(stdin) => stdin.write(buf),
            None => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdin.as_mut().map_or(Ok(()), |s| s.flush())
    }
}

impl TarSink for RemoteSink {
    fn finish(mut self: Box<Self>) -> VfsResult<()> {
        drop(self.stdin.take());
        let status = self.proc.wait();
        if let Some(drain) = self.drain.take() {
            let _ = drain.join();
        }
        let stderr = self
            .stderr
            .take()
            .map(|h| h.join().unwrap_or_default())
            .unwrap_or_default();
        if status == 0 {
            return Ok(());
        }
        let out = RunOutput {
            stdout: Vec::new(),
            stderr,
            status,
        };
        Err(out.error(&self.what))
    }
}

fn tar_result(status: i32, stderr: &str, what: &str, _remote: &Remote) -> VfsResult<()> {
    if status == 0 {
        return Ok(());
    }
    // GNU tar exits 1 when a file changed while being read: the archive
    // is still complete.
    if status == 1 && stderr.contains("changed as we read") {
        log::warn!("tar: {stderr}");
        return Ok(());
    }
    let out = RunOutput {
        stdout: Vec::new(),
        stderr: stderr.to_string(),
        status,
    };
    Err(out.error(what))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_skipping() {
        let mut s = MarkerSkipper::default();
        assert_eq!(s.feed(b"motd noise\x1fTL"), None);
        assert_eq!(s.feed(b"X1\x1fpayload").unwrap(), b"payload");
        assert_eq!(s.feed(b"more").unwrap(), b"more");
    }

    #[test]
    fn shells() {
        assert_eq!(LoginShell::from_path("/bin/bash\n"), LoginShell::Posix);
        assert_eq!(LoginShell::from_path("/usr/bin/fish"), LoginShell::Fish);
        assert_eq!(LoginShell::from_path("/bin/tcsh"), LoginShell::Exotic);
        assert_eq!(LoginShell::from_path(""), LoginShell::Posix);
    }

    #[test]
    fn fish_quoting() {
        assert_eq!(fish_quote(br"a'b\c"), br"'a\'b\\c'".to_vec());
    }
}
