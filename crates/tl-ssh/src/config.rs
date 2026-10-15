//! Bookmark settings → `ssh` command-line arguments.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tl_core::Bookmark;

/// Where and how to connect. Everything not set here comes from
/// `~/.ssh/config`, exactly as with a plain `ssh` in a terminal.
#[derive(Clone, Debug)]
pub struct SshTarget {
    /// Host name, IP or `~/.ssh/config` alias.
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity: Option<PathBuf>,
    /// Extra options passed verbatim (`-J bastion -o Compression=yes`).
    pub extra: Vec<String>,
    pub ssh_bin: PathBuf,
}

impl SshTarget {
    pub fn from_bookmark(bookmark: &Bookmark) -> Result<Self, String> {
        let raw = bookmark.get("host");
        if raw.is_empty() {
            return Err("Host is required".into());
        }
        let (mut user, host, mut port) = parse_destination(raw)?;
        if !bookmark.get("user").is_empty() {
            user = Some(bookmark.get("user").to_string());
        }
        if !bookmark.get("port").is_empty() {
            port = Some(
                bookmark
                    .get("port")
                    .parse()
                    .map_err(|_| format!("Invalid port “{}”", bookmark.get("port")))?,
            );
        }
        let identity = Some(bookmark.get("identity"))
            .filter(|s| !s.is_empty())
            .map(expand_tilde);
        let extra = split_args(bookmark.get("extra"))?;
        let ssh_bin = Some(bookmark.get("ssh_bin"))
            .filter(|s| !s.is_empty())
            .map(expand_tilde)
            .unwrap_or_else(find_ssh);
        Ok(SshTarget {
            host,
            user,
            port,
            identity,
            extra,
            ssh_bin,
        })
    }

    /// Options for the master connection and interactive sessions.
    pub fn connect_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = Vec::new();
        if let Some(user) = &self.user {
            args.push("-l".into());
            args.push(user.into());
        }
        if let Some(port) = self.port {
            args.push("-p".into());
            args.push(port.to_string().into());
        }
        if let Some(identity) = &self.identity {
            args.push("-i".into());
            args.push(identity.into());
        }
        args.extend(self.extra.iter().map(OsString::from));
        args
    }

    pub fn label(&self) -> String {
        match &self.user {
            Some(user) => format!("{user}@{}", self.host),
            None => self.host.clone(),
        }
    }
}

/// Effective settings according to `ssh -G`, which applies `~/.ssh/config`.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    pub user: String,
    pub hostname: String,
    pub port: u16,
    /// ProxyJump / ProxyCommand: the same address behind different jump
    /// hosts may be different machines.
    pub via: Option<String>,
}

impl Resolved {
    /// Identity of the file namespace this connection sees.
    pub fn location_key(&self) -> String {
        let mut key = format!("ssh://{}@{}:{}", self.user, self.hostname, self.port);
        if let Some(via) = &self.via {
            key.push_str("?via=");
            key.push_str(via);
        }
        key
    }
}

pub fn resolve(target: &SshTarget) -> Option<Resolved> {
    let output = Command::new(&target.ssh_bin)
        .arg("-G")
        .args(target.connect_args())
        .arg(&target.host)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut resolved = Resolved::default();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        match key {
            "user" => resolved.user = value.to_string(),
            "hostname" => resolved.hostname = value.to_string(),
            "port" => resolved.port = value.parse().unwrap_or(22),
            "proxyjump" | "proxycommand" if value != "none" => {
                resolved.via = Some(value.to_string())
            }
            _ => {}
        }
    }
    (!resolved.hostname.is_empty()).then_some(resolved)
}

/// Accepts `host`, `user@host`, `host:port`, `user@host:port` and
/// `[v6addr]:port`.
pub fn parse_destination(raw: &str) -> Result<(Option<String>, String, Option<u16>), String> {
    let raw = raw.trim();
    let raw = raw.strip_prefix("ssh://").unwrap_or(raw);
    let (user, rest) = match raw.rsplit_once('@') {
        Some((user, rest)) => (Some(user.to_string()), rest),
        None => (None, raw),
    };
    let (host, port) = if let Some(inner) = rest.strip_prefix('[') {
        let (host, after) = inner
            .split_once(']')
            .ok_or_else(|| format!("Invalid address “{raw}”"))?;
        let port = after.strip_prefix(':').map(str::parse::<u16>).transpose();
        (host.to_string(), port.map_err(|_| format!("Invalid port in “{raw}”"))?)
    } else if rest.matches(':').count() == 1 {
        let (host, port) = rest.split_once(':').unwrap();
        let port = port
            .parse::<u16>()
            .map_err(|_| format!("Invalid port in “{raw}”"))?;
        (host.to_string(), Some(port))
    } else {
        (rest.to_string(), None)
    };
    if host.is_empty() {
        return Err(format!("Invalid host “{raw}”"));
    }
    Ok((user, host, port))
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    PathBuf::from(path)
}

/// Prefers Apple's `/usr/bin/ssh` on macOS: it understands `UseKeychain`,
/// which many `~/.ssh/config` files contain.
pub fn find_ssh() -> PathBuf {
    let system = Path::new("/usr/bin/ssh");
    if cfg!(target_os = "macos") && system.exists() {
        return system.into();
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("ssh");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from("ssh")
}

/// Splits a command-line-like string, honouring quotes and backslashes.
pub fn split_args(s: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => return Err("Unterminated ' in ssh options".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c) => current.push(c),
                            None => return Err("Dangling \\ in ssh options".into()),
                        },
                        Some(c) => current.push(c),
                        None => return Err("Unterminated \" in ssh options".into()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(c) => current.push(c),
                    None => return Err("Dangling \\ in ssh options".into()),
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    args.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        args.push(current);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations() {
        assert_eq!(parse_destination("web").unwrap(), (None, "web".into(), None));
        assert_eq!(
            parse_destination("deploy@web:2222").unwrap(),
            (Some("deploy".into()), "web".into(), Some(2222))
        );
        assert_eq!(
            parse_destination("[::1]:22").unwrap(),
            (None, "::1".into(), Some(22))
        );
        assert_eq!(parse_destination("fe80::1").unwrap(), (None, "fe80::1".into(), None));
        assert!(parse_destination("web:abc").is_err());
    }

    #[test]
    fn args() {
        assert_eq!(
            split_args(r#"-J bastion -o "ProxyCommand=nc %h 22" 'a b'"#).unwrap(),
            vec!["-J", "bastion", "-o", "ProxyCommand=nc %h 22", "a b"]
        );
        assert!(split_args("'open").is_err());
        assert!(split_args("").unwrap().is_empty());
    }
}
