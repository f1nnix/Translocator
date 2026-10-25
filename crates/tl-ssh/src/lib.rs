//! SSH provider: a file system emulated with plain shell commands sent
//! through the system's OpenSSH client.
//!
//! Nothing here speaks SFTP. A ControlMaster connection is authenticated
//! once (keys, agent, `~/.ssh/config`, passwords via the app's askpass), and
//! every operation is a short POSIX `sh` script — `find` to list, `cp -a` /
//! `mv` to copy and move on the server, `tar` to stream to and from other
//! locations — optionally wrapped in `sudo`.

mod archive;
mod config;
mod listing;
mod master;
mod remote;
mod scripts;
mod vfs;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use tl_core::plugin::FieldKind;
use tl_core::{
    Bookmark, CancelToken, FieldSpec, PluginEnv, ProviderPlugin, Vfs, VfsError, VfsResult,
};

pub use config::SshTarget;
pub use listing::Dialect;
pub use vfs::{RemoteInfo, SshVfs};

use master::Master;
use remote::{LoginShell, Remote, RunOpts};

pub const PLUGIN_ID: &str = "ssh";

pub struct SshPlugin;

impl ProviderPlugin for SshPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn name(&self) -> &'static str {
        "SSH (shell commands)"
    }

    fn description(&self) -> &'static str {
        "Works through your OpenSSH client and ~/.ssh/config. Files are managed with \
         ordinary commands on the server (find, cp, mv, tar), so copies between two \
         folders on the same server never leave it, and sudo works like in a terminal."
    }

    fn fields(&self) -> Vec<FieldSpec> {
        vec![
            FieldSpec::text("host", "Host")
                .placeholder("server.example.com or a ~/.ssh/config alias")
                .help("user@host:port is accepted too.")
                .required(),
            FieldSpec::text("user", "User").placeholder("from ssh config"),
            FieldSpec::text("port", "Port")
                .kind(FieldKind::Number)
                .placeholder("22"),
            FieldSpec::text("identity", "Identity file")
                .kind(FieldKind::LocalPath)
                .placeholder("~/.ssh/id_ed25519"),
            FieldSpec::text("path", "Start folder").placeholder("~ (home)"),
            FieldSpec::text("sudo", "Elevate with sudo when opened")
                .kind(FieldKind::Toggle)
                .help("Panes on this server start in sudo mode. You can toggle it per pane."),
            FieldSpec::text("sudo_user", "sudo as user")
                .placeholder("root")
                .default_value("root")
                .advanced(),
            FieldSpec::text("extra", "Extra ssh options")
                .placeholder("-J bastion -o Compression=yes")
                .advanced(),
            FieldSpec::text("ssh_bin", "ssh executable")
                .kind(FieldKind::LocalPath)
                .placeholder("/usr/bin/ssh")
                .advanced(),
        ]
    }

    fn validate(&self, config: &tl_core::BookmarkConfig) -> Result<(), String> {
        let mut bookmark = Bookmark::new("", PLUGIN_ID);
        bookmark.config = config.clone();
        SshTarget::from_bookmark(&bookmark).map(drop)
    }

    fn summary(&self, bookmark: &Bookmark) -> String {
        match SshTarget::from_bookmark(bookmark) {
            Ok(target) => {
                let mut s = target.label();
                if let Some(port) = target.port {
                    s.push_str(&format!(":{port}"));
                }
                s
            }
            Err(err) => err,
        }
    }

    fn connect(
        &self,
        bookmark: &Bookmark,
        env: &PluginEnv,
        cancel: &CancelToken,
    ) -> VfsResult<Arc<dyn Vfs>> {
        Ok(connect(bookmark, env, cancel)?)
    }
}

pub fn connect(bookmark: &Bookmark, env: &PluginEnv, cancel: &CancelToken) -> VfsResult<Arc<SshVfs>> {
    let target = SshTarget::from_bookmark(bookmark).map_err(VfsError::other)?;
    let resolved = config::resolve(&target);
    let key = match &resolved {
        Some(r) => r.location_key(),
        None => format!("ssh://{}:{}", target.label(), target.port.unwrap_or(22)),
    };
    let label = match &resolved {
        Some(r) if !r.user.is_empty() => format!("{}@{}", r.user, target.host),
        _ => target.label(),
    };

    let control = control_path(&env.runtime_dir);
    let master = Master::start(&target, control.clone(), env.askpass.as_ref(), cancel)?;
    let sudo_user = match bookmark.get("sudo_user") {
        "" => "root".to_string(),
        user => user.to_string(),
    };
    let remote = Arc::new(Remote::new(
        target.ssh_bin.clone(),
        control,
        target.host.clone(),
        label.clone(),
        sudo_user,
        env.prompter.clone(),
    ));

    // The login shell decides how scripts must be quoted.
    let shell_out = remote.run_raw("printenv SHELL")?;
    let shell = String::from_utf8_lossy(&shell_out.stdout)
        .lines()
        .rfind(|l| l.starts_with('/'))
        .unwrap_or("")
        .trim()
        .to_string();
    *remote.shell.lock() = LoginShell::from_path(&shell);

    let probe = remote.run(scripts::PROBE.as_bytes(), RunOpts::default(), cancel)?;
    let info = RemoteInfo::parse(&probe.stdout, shell)?;
    log::info!("connected to {label} ({key}): {info:?}");

    Ok(Arc::new(SshVfs {
        id: format!("ssh:{}", bookmark.id),
        key,
        label,
        remote,
        master,
        info,
        target,
    }))
}

/// A control socket path short enough for `sun_path` (104 bytes on macOS),
/// leaving room for the random suffix ssh appends while binding.
fn control_path(runtime_dir: &Path) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let name = format!("{}-{}.sock", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
    let candidate = runtime_dir.join(&name);
    if candidate.as_os_str().len() + 20 < 100 {
        candidate
    } else {
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/tmp/tl-{uid}-{name}"))
    }
}
