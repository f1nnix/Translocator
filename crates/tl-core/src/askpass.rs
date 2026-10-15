//! `SSH_ASKPASS` bridge.
//!
//! The app points `ssh` at its own executable as the askpass program. When
//! `ssh` needs a password, passphrase or host key confirmation it starts that
//! executable, which (see [`run_client`]) forwards the prompt over a private
//! Unix socket to the running app. The app shows a dialog and writes the
//! answer back. This keeps authentication fully inside OpenSSH: keys, agents,
//! `~/.ssh/config`, ProxyJump and 2FA prompts all work unchanged.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::prompt::Prompter;

/// Environment variable carrying the socket path to the askpass child.
pub const ENV_SOCKET: &str = "TRANSLOCATOR_ASKPASS_SOCKET";

/// What a child process needs to reach the askpass server.
#[derive(Clone, Debug)]
pub struct AskpassHandle {
    pub exe: PathBuf,
    pub socket: PathBuf,
}

impl AskpassHandle {
    /// Environment to set on a spawned `ssh`.
    pub fn env(&self) -> Vec<(&'static str, OsString)> {
        vec![
            ("SSH_ASKPASS", self.exe.clone().into_os_string()),
            ("SSH_ASKPASS_REQUIRE", "force".into()),
            // OpenSSH older than 8.4 only uses askpass when DISPLAY is set.
            (
                "DISPLAY",
                std::env::var_os("DISPLAY").unwrap_or_else(|| ":0".into()),
            ),
            (ENV_SOCKET, self.socket.clone().into_os_string()),
        ]
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptKind {
    /// Password, passphrase or one-time code; the answer is printed.
    Secret,
    /// A question answered by typing "yes"/"no" (host key confirmation).
    YesNo,
    /// `SSH_ASKPASS_PROMPT=confirm`: answered by the exit status.
    Confirm,
    /// `SSH_ASKPASS_PROMPT=none`: informational ("touch your key").
    Info,
}

impl PromptKind {
    pub fn classify(prompt: &str, askpass_prompt_env: Option<&str>) -> Self {
        match askpass_prompt_env {
            Some("confirm") => PromptKind::Confirm,
            Some("none") => PromptKind::Info,
            _ if prompt.contains("(yes/no") => PromptKind::YesNo,
            _ => PromptKind::Secret,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Request {
    prompt: String,
    kind: PromptKind,
}

#[derive(Serialize, Deserialize)]
struct Response {
    /// `None` means the user cancelled.
    answer: Option<String>,
}

/// Listens for askpass children and answers them through a [`Prompter`].
pub struct AskpassServer {
    handle: AskpassHandle,
}

impl AskpassServer {
    pub fn start(
        socket: PathBuf,
        exe: PathBuf,
        prompter: Arc<dyn Prompter>,
    ) -> std::io::Result<Self> {
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        }
        std::thread::Builder::new()
            .name("askpass-server".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let prompter = prompter.clone();
                    let _ = std::thread::Builder::new()
                        .name("askpass-request".into())
                        .spawn(move || {
                            if let Err(err) = serve(stream, &*prompter) {
                                log::warn!("askpass request failed: {err}");
                            }
                        });
                }
            })?;
        Ok(AskpassServer {
            handle: AskpassHandle { exe, socket },
        })
    }

    pub fn handle(&self) -> AskpassHandle {
        self.handle.clone()
    }
}

impl Drop for AskpassServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.handle.socket);
    }
}

fn serve(stream: UnixStream, prompter: &dyn Prompter) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let request: Request = serde_json::from_str(&line).map_err(std::io::Error::other)?;
    let prompt = request.prompt.trim().to_string();
    let answer = match request.kind {
        PromptKind::Secret => prompter.secret(secret_title(&prompt), &prompt),
        PromptKind::YesNo => Some(
            if prompter.confirm("Unknown host key", &prompt) {
                "yes"
            } else {
                "no"
            }
            .to_string(),
        ),
        PromptKind::Confirm => prompter.confirm("SSH", &prompt).then(String::new),
        PromptKind::Info => Some(String::new()),
    };
    let mut stream = stream;
    serde_json::to_writer(&mut stream, &Response { answer }).map_err(std::io::Error::other)?;
    stream.write_all(b"\n")
}

fn secret_title(prompt: &str) -> &'static str {
    let lower = prompt.to_ascii_lowercase();
    if lower.contains("passphrase") {
        "Key passphrase"
    } else if lower.contains("password") {
        "SSH password"
    } else {
        "SSH authentication"
    }
}

/// Entry point when this executable runs as `SSH_ASKPASS`; returns the
/// process exit code. `ssh` passes the prompt as the only argument.
pub fn run_client(prompt: &str) -> i32 {
    let Some(socket) = std::env::var_os(ENV_SOCKET) else {
        return 1;
    };
    let env_kind = std::env::var("SSH_ASKPASS_PROMPT").ok();
    let kind = PromptKind::classify(prompt, env_kind.as_deref());
    let result = (|| -> std::io::Result<Option<String>> {
        let mut stream = UnixStream::connect(&socket)?;
        let request = Request {
            prompt: prompt.to_string(),
            kind,
        };
        serde_json::to_writer(&mut stream, &request).map_err(std::io::Error::other)?;
        stream.write_all(b"\n")?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        let response: Response = serde_json::from_str(&line).map_err(std::io::Error::other)?;
        Ok(response.answer)
    })();
    match result {
        Ok(Some(answer)) => {
            if matches!(kind, PromptKind::Secret | PromptKind::YesNo) {
                println!("{answer}");
            }
            0
        }
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed;
    impl Prompter for Fixed {
        fn secret(&self, _: &str, message: &str) -> Option<String> {
            Some(format!("pw-for:{message}"))
        }
        fn confirm(&self, _: &str, _: &str) -> bool {
            true
        }
    }

    #[test]
    fn classifies_prompts() {
        assert_eq!(
            PromptKind::classify("Are you sure you want to continue connecting (yes/no/[fingerprint])? ", None),
            PromptKind::YesNo
        );
        assert_eq!(PromptKind::classify("x", Some("confirm")), PromptKind::Confirm);
        assert_eq!(PromptKind::classify("me@h's password: ", None), PromptKind::Secret);
    }

    #[test]
    fn server_answers() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("a.sock");
        let _server = AskpassServer::start(socket.clone(), "/bin/false".into(), Arc::new(Fixed)).unwrap();
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream
            .write_all(b"{\"prompt\":\"pw?\",\"kind\":\"secret\"}\n")
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        assert!(line.contains("pw-for:pw?"), "{line}");
    }
}
