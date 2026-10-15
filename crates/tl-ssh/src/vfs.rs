//! [`Vfs`] on top of remote shell commands.

use std::sync::Arc;

use tl_core::shell::Script;
use tl_core::util::recursive_dir_mode;
use tl_core::{
    Capabilities, Entry, ExecOutput, MergePolicy, OpCtx, OwnerHint, Progress, Resolution,
    ScanTotals, SpaceInfo, TarSink, TarSource, TransferItem, VPath, Vfs, VfsError, VfsResult,
    WalkEntry,
};

use crate::config::SshTarget;
use crate::listing::{self, Dialect};
use crate::master::Master;
use crate::remote::{Remote, RunOpts, RunOutput};
use crate::scripts;

/// Scripts stay below this size: Linux caps one argument at 128 KiB.
const MAX_SCRIPT: usize = 96 * 1024;

/// What the probe found out about the server.
#[derive(Clone, Debug, Default)]
pub struct RemoteInfo {
    pub user: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
    pub os: String,
    pub hostname: String,
    pub dialect: Dialect,
    pub gnu_cp: bool,
    /// `tar` understands `-p` (GNU tar, bsdtar; not BusyBox).
    pub tar_p: bool,
    pub tools: Vec<String>,
    pub shell: String,
}

impl RemoteInfo {
    pub fn parse(out: &[u8], shell: String) -> Result<Self, VfsError> {
        let text = String::from_utf8_lossy(out);
        let mut info = RemoteInfo {
            shell,
            ..Default::default()
        };
        let (mut find_printf, mut stat_c, mut stat_f) = (false, false, false);
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "user" => info.user = value.to_string(),
                "uid" => info.uid = value.parse().unwrap_or(0),
                "gid" => info.gid = value.parse().unwrap_or(0),
                "home" => info.home = value.to_string(),
                "os" => info.os = value.to_string(),
                "host" => info.hostname = value.to_string(),
                "find_printf" => find_printf = true,
                "stat_c" => stat_c = true,
                "stat_f" => stat_f = true,
                "gnu_cp" => info.gnu_cp = true,
                "tar" => {
                    info.tar_p = value.contains("GNU tar") || value.contains("bsdtar")
                }
                "has" => info.tools.push(value.to_string()),
                _ => {}
            }
        }
        info.dialect = if find_printf {
            Dialect::Gnu
        } else if stat_c {
            Dialect::StatC
        } else if stat_f {
            Dialect::StatF
        } else {
            return Err(VfsError::Unsupported(
                "the server has neither GNU find nor a usable stat command".into(),
            ));
        };
        Ok(info)
    }

    pub fn has(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t == tool)
    }
}

pub struct SshVfs {
    pub(crate) id: String,
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) remote: Arc<Remote>,
    pub(crate) master: Master,
    pub(crate) info: RemoteInfo,
    pub(crate) target: SshTarget,
}

fn policy_word(policy: MergePolicy) -> &'static str {
    match policy {
        MergePolicy::Overwrite => "overwrite",
        MergePolicy::OverwriteOlder => "older",
        MergePolicy::SkipExisting => "skip",
    }
}

/// Splits paths so each command stays below [`MAX_SCRIPT`].
fn chunks(paths: &[VPath]) -> Vec<&[VPath]> {
    let mut out = Vec::new();
    let (mut start, mut size) = (0, 0);
    for (i, p) in paths.iter().enumerate() {
        let len = p.as_bytes().len() + 8;
        if i > start && size + len > MAX_SCRIPT {
            out.push(&paths[start..i]);
            start = i;
            size = 0;
        }
        size += len;
    }
    if start < paths.len() {
        out.push(&paths[start..]);
    }
    out
}

impl SshVfs {
    pub fn info(&self) -> &RemoteInfo {
        &self.info
    }

    fn run(&self, script: &Script, ctx: &OpCtx) -> VfsResult<RunOutput> {
        self.remote.run(
            script.as_bytes(),
            RunOpts {
                elevated: ctx.elevated,
                ..Default::default()
            },
            &ctx.cancel,
        )
    }

    fn run_checked(&self, script: &Script, ctx: &OpCtx, what: &str) -> VfsResult<RunOutput> {
        let out = self.run(script, ctx)?;
        if out.success() { Ok(out) } else { Err(out.error(what)) }
    }

    /// A cancellable long-running job (copy, delete, ...).
    fn run_job(
        &self,
        body: &[u8],
        ctx: &OpCtx,
        on_data: impl FnMut(&[u8]),
    ) -> VfsResult<RunOutput> {
        self.remote.run_streaming(
            &scripts::with_watchdog(body),
            RunOpts {
                elevated: ctx.elevated,
                watchdog: true,
                ..Default::default()
            },
            &ctx.cancel,
            on_data,
        )
    }

    fn transfer_within(
        &self,
        items: &[TransferItem],
        ctx: &OpCtx,
        progress: &Progress,
        mode: &str,
    ) -> VfsResult<()> {
        // GNU `cp -v` prints one line per copied node: exact progress.
        let verbose = mode == "copy" && self.info.gnu_cp;
        if verbose {
            let paths: Vec<VPath> = items.iter().map(|i| i.src.clone()).collect();
            match self.scan(&paths, ctx) {
                Ok(t) => progress.set_totals(0, t.files + t.dirs),
                Err(VfsError::Cancelled) => return Err(VfsError::Cancelled),
                Err(_) => progress.set_indeterminate(true),
            }
        } else {
            progress.set_totals(0, items.len() as u64);
        }
        let owner = (ctx.elevated && ctx.owner_hint == OwnerHint::LoginUser)
            .then(|| format!("{}:{}", self.info.uid, self.info.gid));

        let mut start = 0;
        while start < items.len() {
            let mut body = Script::new();
            body.raw(scripts::MERGE_FUNCTIONS);
            body.raw(if verbose { "TL_CP=-av\n" } else { "TL_CP=-a\n" });
            if let Some(owner) = &owner {
                body.raw("TL_OWNER=").arg(owner).raw("\n");
            }
            let mut end = start;
            while end < items.len() && (end == start || body.len() < MAX_SCRIPT) {
                let item = &items[end];
                body.raw(&format!("printf '\\036{end}\\n'\n"));
                match item.resolution {
                    Resolution::Create => body.raw("tl_xfer ").raw(mode),
                    Resolution::Replace => body.raw("tl_put ").raw(mode),
                    Resolution::Merge(p) => body.raw("tl_merge ").raw(mode).raw(" ").raw(policy_word(p)),
                };
                body.sp_arg(item.src.as_bytes())
                    .sp_arg(item.dst.as_bytes())
                    .raw(" || exit 1\n");
                end += 1;
            }
            let mut line = Vec::new();
            let out = self.run_job(body.as_bytes(), ctx, |data| {
                for &b in data {
                    if b != b'\n' {
                        line.push(b);
                        continue;
                    }
                    if let Some(idx) = line.strip_prefix(b"\x1e") {
                        let idx: usize = std::str::from_utf8(idx)
                            .ok()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0);
                        if let Some(item) = items.get(idx) {
                            progress.set_current(item.src.display());
                        }
                        if !verbose {
                            progress.set_done(0, idx as u64);
                        }
                    } else if verbose {
                        progress.add_items(1);
                    }
                    line.clear();
                }
            })?;
            if !out.success() {
                return Err(out.error(&items[start].src.display()));
            }
            start = end;
        }
        if !verbose {
            progress.set_done(0, items.len() as u64);
        }
        Ok(())
    }
}

impl Vfs for SshVfs {
    fn id(&self) -> &str {
        &self.id
    }

    fn plugin_id(&self) -> &str {
        crate::PLUGIN_ID
    }

    fn label(&self) -> String {
        self.label.clone()
    }

    fn location_key(&self) -> &str {
        &self.key
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            elevation: self.info.has("sudo"),
            trash: false,
            exec: true,
            chmod: true,
            chown: true,
            symlink: true,
            case_insensitive: self.info.os == "Darwin",
        }
    }

    fn is_connected(&self) -> bool {
        self.master.is_alive()
    }

    fn home(&self) -> VPath {
        if self.info.home.is_empty() {
            VPath::root()
        } else {
            VPath::parse(&self.info.home)
        }
    }

    fn list(&self, dir: &VPath, ctx: &OpCtx) -> VfsResult<Vec<Entry>> {
        let out = self.run_checked(&listing::list_script(self.info.dialect, dir), ctx, &dir.display())?;
        Ok(listing::parse_list(self.info.dialect, &out.stdout))
    }

    fn stat(&self, path: &VPath, ctx: &OpCtx) -> VfsResult<Option<Entry>> {
        let out = self.run(&listing::stat_script(self.info.dialect, path), ctx)?;
        match out.status {
            0 => Ok(listing::parse_stat(self.info.dialect, path, &out.stdout)),
            3 => Ok(None),
            _ => Err(out.error(&path.display())),
        }
    }

    fn walk(&self, root: &VPath, ctx: &OpCtx) -> VfsResult<Vec<WalkEntry>> {
        let out = self.run_checked(&listing::walk_script(self.info.dialect, root), ctx, &root.display())?;
        Ok(listing::parse_walk(self.info.dialect, &out.stdout))
    }

    fn scan(&self, paths: &[VPath], ctx: &OpCtx) -> VfsResult<ScanTotals> {
        let mut totals = ScanTotals::default();
        for chunk in chunks(paths) {
            let out = self.run(&listing::scan_script(self.info.dialect, chunk), ctx)?;
            let t = listing::parse_scan(&out.stdout);
            totals.bytes += t.bytes;
            totals.files += t.files;
            totals.dirs += t.dirs;
        }
        Ok(totals)
    }

    fn mkdir(&self, path: &VPath, ctx: &OpCtx) -> VfsResult<()> {
        let mut s = Script::new();
        s.raw("mkdir --").sp_arg(path.as_bytes());
        self.run_checked(&s, ctx, &path.display()).map(drop)
    }

    fn create_file(&self, path: &VPath, ctx: &OpCtx) -> VfsResult<()> {
        let mut s = Script::new();
        s.raw("set -C\n: >").sp_arg(path.as_bytes());
        self.run_checked(&s, ctx, &path.display()).map(drop)
    }

    fn rename(&self, from: &VPath, to: &VPath, ctx: &OpCtx) -> VfsResult<()> {
        let mut s = Script::new();
        s.raw("t=").arg(to.as_bytes()).raw("\n");
        s.raw("if [ -e \"$t\" ] || [ -L \"$t\" ]; then echo TLERR:EEXIST >&2; exit 2; fi\n");
        s.raw("mv --").sp_arg(from.as_bytes()).raw(" \"$t\"\n");
        self.run_checked(&s, ctx, &to.display()).map(drop)
    }

    fn delete(&self, paths: &[VPath], ctx: &OpCtx, progress: &Progress) -> VfsResult<()> {
        if paths.iter().any(VPath::is_root) {
            return Err(VfsError::other("Refusing to delete /"));
        }
        progress.set_totals(0, paths.len() as u64);
        for chunk in chunks(paths) {
            progress.set_current(chunk[0].display());
            let mut body = Script::new();
            body.raw("rm -rf --");
            for p in chunk {
                body.sp_arg(p.as_bytes());
            }
            let out = self.run_job(body.as_bytes(), ctx, |_| {})?;
            if !out.success() {
                return Err(out.error(&chunk[0].display()));
            }
            progress.add_items(chunk.len() as u64);
        }
        Ok(())
    }

    fn remove_empty_dirs(&self, dirs: &[VPath], ctx: &OpCtx) -> VfsResult<()> {
        for chunk in chunks(dirs) {
            let mut s = Script::new();
            s.raw("rmdir --");
            for d in chunk {
                s.sp_arg(d.as_bytes());
            }
            s.raw(" 2>/dev/null\nexit 0\n");
            self.run(&s, ctx)?;
        }
        Ok(())
    }

    fn copy_within(&self, items: &[TransferItem], ctx: &OpCtx, progress: &Progress) -> VfsResult<()> {
        self.transfer_within(items, ctx, progress, "copy")
    }

    fn move_within(&self, items: &[TransferItem], ctx: &OpCtx, progress: &Progress) -> VfsResult<()> {
        self.transfer_within(items, ctx, progress, "move")
    }

    fn chmod(&self, paths: &[VPath], mode: u32, recursive: bool, ctx: &OpCtx) -> VfsResult<()> {
        for chunk in chunks(paths) {
            let mut body = Script::new();
            if recursive {
                body.raw("find");
                for p in chunk {
                    body.sp_arg(p.as_bytes());
                }
                // No `--` after the mode: BSD chmod would take it for a file.
                // Paths are absolute, so they never look like options.
                body.raw(&format!(" ! -type l ! -type d -exec chmod {mode:04o} {{}} + && find"));
                for p in chunk {
                    body.sp_arg(p.as_bytes());
                }
                body.raw(&format!(
                    " -type d -exec chmod {:04o} {{}} +\n",
                    recursive_dir_mode(mode)
                ));
            } else {
                body.raw(&format!("chmod {mode:04o}"));
                for p in chunk {
                    body.sp_arg(p.as_bytes());
                }
            }
            let out = self.run_job(body.as_bytes(), ctx, |_| {})?;
            if !out.success() {
                return Err(out.error(&chunk[0].display()));
            }
        }
        Ok(())
    }

    fn chown(
        &self,
        paths: &[VPath],
        owner: Option<&str>,
        group: Option<&str>,
        recursive: bool,
        ctx: &OpCtx,
    ) -> VfsResult<()> {
        let owner = owner.filter(|o| !o.is_empty());
        let group = group.filter(|g| !g.is_empty());
        let (cmd, spec) = match (owner, group) {
            (Some(o), Some(g)) => ("chown", format!("{o}:{g}")),
            (Some(o), None) => ("chown", o.to_string()),
            (None, Some(g)) => ("chgrp", g.to_string()),
            (None, None) => return Ok(()),
        };
        for chunk in chunks(paths) {
            let mut body = Script::new();
            body.raw(cmd);
            if recursive {
                body.raw(" -R");
            }
            body.raw(" --").sp_arg(spec.as_bytes());
            for p in chunk {
                body.sp_arg(p.as_bytes());
            }
            let out = self.run_job(body.as_bytes(), ctx, |_| {})?;
            if !out.success() {
                return Err(out.error(&chunk[0].display()));
            }
        }
        Ok(())
    }

    fn symlink(&self, target: &[u8], link: &VPath, ctx: &OpCtx) -> VfsResult<()> {
        let mut s = Script::new();
        s.raw("ln -s --").sp_arg(target).sp_arg(link.as_bytes());
        self.run_checked(&s, ctx, &link.display()).map(drop)
    }

    fn read_file(&self, path: &VPath, limit: u64, ctx: &OpCtx) -> VfsResult<Vec<u8>> {
        let mut s = Script::new();
        s.raw("p=").arg(path.as_bytes()).raw("\n");
        s.raw(concat!(
            "if [ ! -e \"$p\" ]; then echo TLERR:ENOENT >&2; exit 2; fi\n",
            "if [ -d \"$p\" ]; then echo TLERR:EISDIR >&2; exit 2; fi\n",
            "if [ ! -r \"$p\" ]; then echo TLERR:EACCES >&2; exit 2; fi\n",
        ));
        if limit == u64::MAX {
            s.raw("exec cat -- \"$p\"\n");
        } else {
            s.raw(&format!("exec head -c {limit} -- \"$p\"\n"));
        }
        Ok(self.run_checked(&s, ctx, &path.display())?.stdout)
    }

    fn write_file(&self, path: &VPath, data: &[u8], ctx: &OpCtx) -> VfsResult<()> {
        // Upload to a temp file first: if the connection drops halfway, the
        // target is untouched. Then rewrite the target in place, keeping its
        // owner, mode and hard links.
        let mut s = Script::new();
        s.raw("p=").arg(path.as_bytes()).raw("\n");
        s.raw(concat!(
            "if [ -e \"$p\" ] && [ ! -f \"$p\" ]; then echo TLERR:ENOTREG >&2; exit 2; fi\n",
            "t=$(umask 077; mktemp \"${TMPDIR:-/tmp}/tl.XXXXXX\" 2>/dev/null) || ",
            "{ t=\"${TMPDIR:-/tmp}/tl.$$\"; (umask 077; : > \"$t\") || exit 2; }\n",
            "cat > \"$t\" || { rm -f \"$t\"; exit 2; }\n",
        ));
        s.raw(&format!(
            "if [ $(( $(wc -c < \"$t\") )) -ne {} ]; then rm -f \"$t\"; echo TLERR:SHORT >&2; exit 2; fi\n",
            data.len()
        ));
        s.raw("cat \"$t\" > \"$p\"; rc=$?\nrm -f \"$t\"\nexit $rc\n");
        let out = self.remote.run(
            s.as_bytes(),
            RunOpts {
                elevated: ctx.elevated,
                stdin: Some(data),
                watchdog: false,
            },
            &ctx.cancel,
        )?;
        if out.success() { Ok(()) } else { Err(out.error(&path.display())) }
    }

    fn export_tar(&self, base: &VPath, names: &[Vec<u8>], ctx: &OpCtx) -> VfsResult<Box<dyn TarSource>> {
        let mut s = Script::new();
        scripts::require_dir(&mut s, base.as_bytes());
        s.raw("exec tar -cf -");
        for name in names {
            // `./` keeps names starting with `-` from looking like options.
            let mut rel = b"./".to_vec();
            rel.extend_from_slice(name);
            s.sp_arg(&rel);
        }
        s.raw("\n");
        let source = self
            .remote
            .source(s.as_bytes(), ctx.elevated, &ctx.cancel, base.display())?;
        Ok(Box::new(source))
    }

    fn import_tar(&self, dir: &VPath, ctx: &OpCtx) -> VfsResult<Box<dyn TarSink>> {
        let mut s = Script::new();
        scripts::require_dir(&mut s, dir.as_bytes());
        s.raw("if [ ! -w \"$p\" ]; then echo TLERR:EACCES >&2; exit 2; fi\nprintf R\n");
        s.raw(if self.info.tar_p {
            "exec tar -x -p -o -f -\n"
        } else {
            "exec tar -x -o -f -\n"
        });
        let sink = self
            .remote
            .sink(s.as_bytes(), ctx.elevated, &ctx.cancel, dir.display())?;
        Ok(Box::new(sink))
    }

    fn free_space(&self, path: &VPath, ctx: &OpCtx) -> VfsResult<Option<SpaceInfo>> {
        let mut s = Script::new();
        s.raw("df -Pk").sp_arg(path.as_bytes()).raw(" 2>/dev/null | tail -n 1\n");
        let out = self.run(&s, ctx)?;
        Ok(parse_df(&String::from_utf8_lossy(&out.stdout)))
    }

    fn find(
        &self,
        root: &VPath,
        masks: &[String],
        ctx: &OpCtx,
        found: &mut dyn FnMut(VPath, tl_core::EntryKind) -> bool,
    ) -> VfsResult<()> {
        // Runs `find` on the server; only matches travel back.
        let mut body = Script::new();
        scripts::require_dir(&mut body, root.as_bytes());
        body.raw("find . \\(");
        for (i, mask) in masks.iter().enumerate() {
            if i > 0 {
                body.raw(" -o");
            }
            body.raw(" -iname").sp_arg(mask.as_bytes());
        }
        body.raw(" \\) \\( -type d -exec printf 'd %s\\n' {} + -o -exec printf 'f %s\\n' {} + \\) 2>/dev/null | head -n 5000\nexit 0\n");
        let mut line = Vec::new();
        let mut stopped = false;
        let out = self.run_job(body.as_bytes(), ctx, |data| {
            for &b in data {
                if b != b'\n' {
                    line.push(b);
                    continue;
                }
                if !stopped && line.len() > 2 {
                    let kind = if line[0] == b'd' { tl_core::EntryKind::Dir } else { tl_core::EntryKind::File };
                    let rel = &line[2..];
                    let rel = rel.strip_prefix(b"./").unwrap_or(rel);
                    stopped = !found(root.join(rel), kind);
                }
                line.clear();
            }
        })?;
        if out.success() { Ok(()) } else { Err(out.error(&root.display())) }
    }

    fn exec(&self, cwd: &VPath, command: &str, ctx: &OpCtx) -> VfsResult<ExecOutput> {
        let mut body = Script::new();
        body.raw("p=").arg(cwd.as_bytes()).raw("\n");
        body.raw("cd -- \"$p\" || exit 125\nunset LC_ALL\n\"${SHELL:-/bin/sh}\" -c ")
            .arg(command.as_bytes())
            .raw("\n");
        let out = self.run_job(body.as_bytes(), ctx, |_| {})?;
        Ok(ExecOutput {
            stdout: out.stdout,
            stderr: out.stderr.into_bytes(),
            status: out.status,
        })
    }

    fn prepare_elevation(&self, _ctx: &OpCtx) -> VfsResult<()> {
        self.remote.prepare_sudo()
    }

    fn terminal_command(&self, cwd: &VPath, elevated: bool) -> Option<Vec<String>> {
        let mut argv = vec![self.target.ssh_bin.display().to_string(), "-t".into()];
        argv.extend(
            self.target
                .connect_args()
                .into_iter()
                .map(|a| a.to_string_lossy().into_owned()),
        );
        argv.push(self.target.host.clone());
        let mut remote = Script::new();
        remote.raw("cd ").arg(cwd.as_bytes());
        remote.raw(if elevated {
            " && exec sudo -s"
        } else {
            " && exec \"$SHELL\" -l"
        });
        argv.push(String::from_utf8_lossy(remote.as_bytes()).into_owned());
        Some(argv)
    }

    fn server_info(&self) -> Option<String> {
        let shell = self.info.shell.rsplit('/').next().unwrap_or("sh");
        Some(format!(
            "{} {} · {} · {}",
            self.info.os,
            self.info.hostname,
            self.info.dialect.name(),
            if shell.is_empty() { "sh" } else { shell }
        ))
    }

    fn disconnect(&self) {
        self.master.stop();
    }
}

/// Parses the last line of `df -Pk` output.
fn parse_df(line: &str) -> Option<SpaceInfo> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let cap = fields.iter().position(|f| f.ends_with('%'))?;
    if cap < 3 {
        return None;
    }
    let total: u64 = fields[cap - 3].parse().ok()?;
    let available: u64 = fields[cap - 1].parse().ok()?;
    Some(SpaceInfo {
        total: total * 1024,
        available: available * 1024,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn df_lines() {
        let info = parse_df("/dev/sda1 41152736 12345678 26703896 32% /").unwrap();
        assert_eq!(info.total, 41152736 * 1024);
        assert_eq!(info.available, 26703896 * 1024);
        let info = parse_df("map auto_home 0 0 0 100% /System/Volumes/Data/home").unwrap();
        assert_eq!(info.total, 0);
        assert!(parse_df("garbage").is_none());
    }

    #[test]
    fn chunking() {
        let paths: Vec<VPath> = (0..10).map(|i| VPath::parse(&format!("/p{i}"))).collect();
        assert_eq!(chunks(&paths).len(), 1);
        let long = VPath::parse(&format!("/{}", "x".repeat(60 * 1024)));
        let paths = vec![long.clone(), long.clone(), long];
        assert_eq!(chunks(&paths).len(), 3);
    }
}
