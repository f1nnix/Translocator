//! The OpenSSH ControlMaster connection.
//!
//! One authenticated `ssh -M -N` process per bookmark; every command then
//! runs as a cheap multiplexed session over its control socket (no new TCP
//! connection, no new authentication).

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tl_core::askpass::AskpassHandle;
use tl_core::{CancelToken, VfsError, VfsResult};

use crate::config::SshTarget;

pub struct Master {
    child: Mutex<Option<Child>>,
    /// Write end of the wrapper's stdin: when this process dies for any
    /// reason the pipe closes and the wrapper takes `ssh` down with it, so
    /// no master connection outlives the app.
    lifeline: Mutex<Option<ChildStdin>>,
    pub control: PathBuf,
    ssh_bin: PathBuf,
    host: String,
}

/// Runs `"$@"` (the ssh master) in the background and kills it when stdin
/// reaches EOF. Exits with ssh's status.
const LIFELINE: &str = r#"exec 3<&0 </dev/null
"$@" &
p=$!
( while read -r _ <&3; do :; done; kill "$p" 2>/dev/null ) >/dev/null 2>&1 &
w=$!
wait "$p"; rc=$?
kill "$w" 2>/dev/null
exit "$rc"
"#;

/// Detaches a child from our terminal (if any) so `ssh` never prompts on a
/// tty; prompts go through askpass instead.
pub fn detach(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

impl Master {
    pub fn start(
        target: &SshTarget,
        control: PathBuf,
        askpass: Option<&AskpassHandle>,
        cancel: &CancelToken,
    ) -> VfsResult<Master> {
        let _ = std::fs::remove_file(&control);
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(LIFELINE)
            .arg("tl-ssh-master")
            .arg(&target.ssh_bin)
            .arg("-M")
            .arg("-N")
            .arg("-S")
            .arg(&control)
            .args([
                "-o",
                "ControlPersist=no",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=4",
                "-o",
                "ConnectTimeout=20",
                "-o",
                "LogLevel=ERROR",
            ])
            .args(target.connect_args())
            .arg(&target.host)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        match askpass {
            Some(askpass) => {
                for (key, value) in askpass.env() {
                    cmd.env(key, value);
                }
            }
            None => {
                cmd.args(["-o", "BatchMode=yes"]);
            }
        }
        detach(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| {
            VfsError::Connection(format!("Cannot start {}: {e}", target.ssh_bin.display()))
        })?;

        let stderr_text = Arc::new(Mutex::new(String::new()));
        if let Some(mut stderr) = child.stderr.take() {
            let sink = stderr_text.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = stderr.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut text = sink.lock();
                    if text.len() < 64 * 1024 {
                        text.push_str(&String::from_utf8_lossy(&buf[..n]));
                    }
                }
            });
        }

        let lifeline = child.stdin.take();
        let pid = child.id() as libc::pid_t;
        let master = Master {
            child: Mutex::new(None),
            lifeline: Mutex::new(lifeline),
            control: control.clone(),
            ssh_bin: target.ssh_bin.clone(),
            host: target.host.clone(),
        };
        loop {
            if cancel.is_cancelled() {
                // The wrapper leads its own process group (see `detach`).
                unsafe { libc::kill(-pid, libc::SIGTERM) };
                let _ = child.wait();
                return Err(VfsError::Cancelled);
            }
            if let Ok(Some(status)) = child.try_wait() {
                // Let the stderr reader drain.
                std::thread::sleep(Duration::from_millis(50));
                let text = stderr_text.lock().trim().to_string();
                return Err(classify_connect_error(&text, status.code()));
            }
            // The socket appears once authentication has succeeded.
            if control.exists() && master.check() {
                break;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        *master.child.lock() = Some(child);
        Ok(master)
    }

    fn control_command(&self, op: &str) -> Command {
        let mut cmd = Command::new(&self.ssh_bin);
        cmd.arg("-S")
            .arg(&self.control)
            .args(["-o", "BatchMode=yes", "-O", op])
            .arg(&self.host)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach(&mut cmd);
        cmd
    }

    pub fn check(&self) -> bool {
        self.control_command("check")
            .status()
            .is_ok_and(|s| s.success())
    }

    pub fn is_alive(&self) -> bool {
        match self.child.lock().as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    pub fn stop(&self) {
        if let Some(mut child) = self.child.lock().take() {
            let _ = self.control_command("exit").status();
            drop(self.lifeline.lock().take());
            unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGTERM) };
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(&self.control);
    }
}

impl Drop for Master {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn classify_connect_error(stderr: &str, code: Option<i32>) -> VfsError {
    let lower = stderr.to_ascii_lowercase();
    let mut msg = if stderr.is_empty() {
        format!("ssh exited with status {}", code.unwrap_or(-1))
    } else {
        stderr.to_string()
    };
    if lower.contains("permission denied")
        || lower.contains("host key verification failed")
        || lower.contains("too many authentication failures")
        || lower.contains("no more authentication methods")
    {
        return VfsError::Auth(msg);
    }
    if let Some(hint) = local_network_hint(&lower) {
        msg.push_str(hint);
    }
    VfsError::Connection(msg)
}

/// macOS asks an application for permission before it may reach the local
/// network and, until that is granted, fails the connection with
/// `EHOSTUNREACH` — which `ssh` reports as "No route to host". The same
/// server answers from a terminal, so the bare message sends you looking at
/// the network instead of at System Settings.
#[cfg(target_os = "macos")]
fn local_network_hint(lower: &str) -> Option<&'static str> {
    let unreachable =
        lower.contains("no route to host") || lower.contains("network is unreachable");
    (unreachable && names_local_host(lower)).then_some(
        "\n\nThe server is on your local network, and macOS only lets an application \
         reach it with permission. Check System Settings \u{2192} Privacy & Security \
         \u{2192} Local Network, and switch Translocator on.",
    )
}

#[cfg(not(target_os = "macos"))]
fn local_network_hint(_lower: &str) -> Option<&'static str> {
    None
}

/// The address in ssh's own "connect to host H port N: ..." line, when it is
/// one of the private ranges macOS gates.
#[cfg(target_os = "macos")]
fn names_local_host(lower: &str) -> bool {
    lower
        .split("connect to host ")
        .skip(1)
        .filter_map(|rest| rest.split_whitespace().next())
        .any(is_local_address)
}

#[cfg(target_os = "macos")]
fn is_local_address(host: &str) -> bool {
    use std::net::IpAddr;
    if host.ends_with(".local") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_private() || ip.is_link_local() || ip.is_loopback(),
        Ok(IpAddr::V6(ip)) => {
            let first = ip.segments()[0];
            ip.is_loopback() || first & 0xffc0 == 0xfe80 || first & 0xfe00 == 0xfc00
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn lifeline_kills_child_when_parent_side_closes() {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(LIFELINE)
            .arg("tl-ssh-master")
            .args(["sleep", "30"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let lifeline = child.stdin.take();
        std::thread::sleep(Duration::from_millis(200));
        assert!(matches!(child.try_wait(), Ok(None)), "still running while the pipe is open");
        let started = Instant::now();
        drop(lifeline);
        let status = child.wait().unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(!status.success());
    }

    #[test]
    fn a_server_on_the_lan_points_at_the_privacy_setting() {
        // macOS refuses an ungranted application the local network, and ssh
        // can only report what connect(2) told it.
        let text = "ssh: connect to host 192.168.2.6 port 2222: No route to host";
        let VfsError::Connection(msg) = classify_connect_error(text, Some(255)) else {
            panic!("a dead connection is not an authentication problem");
        };
        assert!(msg.starts_with(text), "{msg}");
        if cfg!(target_os = "macos") {
            assert!(msg.contains("Local Network"), "{msg}");
        } else {
            assert_eq!(msg, text);
        }
    }

    #[test]
    fn a_server_on_the_internet_gets_no_hint() {
        let text = "ssh: connect to host 93.184.216.34 port 22: No route to host";
        let VfsError::Connection(msg) = classify_connect_error(text, Some(255)) else {
            panic!("a dead connection is not an authentication problem");
        };
        assert_eq!(msg, text);
    }

    #[test]
    fn a_refused_key_is_still_an_authentication_problem() {
        let text = "ssh: Permission denied (publickey,password).";
        assert!(matches!(classify_connect_error(text, Some(255)), VfsError::Auth(_)));
    }

    #[test]
    fn lifeline_passes_exit_status() {
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(LIFELINE)
            .arg("tl-ssh-master")
            .args(["sh", "-c", "exit 7"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let _lifeline = child.stdin.take();
        assert_eq!(child.wait().unwrap().code(), Some(7));
    }
}
