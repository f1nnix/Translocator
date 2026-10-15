//! Shell snippets shared by the remote operations. Everything here is plain
//! POSIX `sh` plus the tools every server has (`find`, `cp`, `mv`, `tar`,
//! `stat`), so the same code drives GNU/Linux, BusyBox and BSD/macOS hosts.

use tl_core::shell::Script;

/// Assigns `p` and fails with a `TLERR:` code unless it is a readable,
/// searchable directory.
pub fn require_dir(script: &mut Script, path: &[u8]) {
    script.raw("p=").arg(path).raw("\n");
    script.raw(concat!(
        "if [ ! -e \"$p\" ] && [ ! -L \"$p\" ]; then echo TLERR:ENOENT >&2; exit 2; fi\n",
        "if [ ! -d \"$p\" ]; then echo TLERR:ENOTDIR >&2; exit 2; fi\n",
        "if [ ! -x \"$p\" ] || [ ! -r \"$p\" ]; then echo TLERR:EACCES >&2; exit 2; fi\n",
        "cd -- \"$p\" || { echo TLERR:EACCES >&2; exit 2; }\n",
    ));
}

/// Runs `body` so that EOF on the session's stdin kills it.
///
/// The client keeps stdin open while the job runs and closes it to cancel;
/// the watchdog then signals the whole process group, which also stops
/// `cp`/`rm` children. A dropped connection has the same effect, so no
/// orphaned jobs keep running on the server.
pub fn with_watchdog(body: &[u8]) -> Vec<u8> {
    let mut out = b"exec 3<&0 </dev/null\ntl_main() {\n".to_vec();
    out.extend_from_slice(body);
    out.extend_from_slice(
        b"\n}\n\
          tl_main & tl_pid=$!\n\
          ( while read -r tl_x <&3; do :; done; kill -TERM 0 ) >/dev/null 2>&1 &\n\
          tl_wd=$!\n\
          wait $tl_pid; tl_rc=$?\n\
          kill $tl_wd 2>/dev/null\n\
          exit $tl_rc\n",
    );
    out
}

/// Copy/move primitives used by server-side transfers.
///
/// * `tl_xfer MODE SRC DST` — copy (`cp $TL_CP`) or move (`mv`) to a free name.
/// * `tl_put MODE SRC DST` — replace whatever is at DST.
/// * `tl_merge MODE POLICY SRC DST` — merge directory SRC into directory
///   DST. POLICY is `overwrite`, `older` (only replace older files) or
///   `skip` (keep existing). For moves, kept-back files stay in SRC.
///
/// Functions share globals (POSIX has no `local`), which is safe because
/// nothing reads them after the recursive call returns.
pub const MERGE_FUNCTIONS: &str = r#"
tl_own() { if [ -n "$TL_OWNER" ]; then chown -R -- "$TL_OWNER" "$1"; fi; }
tl_xfer() { if [ "$1" = move ]; then mv -- "$2" "$3"; else cp $TL_CP -- "$2" "$3" && tl_own "$3"; fi; }
tl_put() { rm -rf -- "$3" && tl_xfer "$1" "$2" "$3"; }
tl_merge() {
  for tl_f in "$3"/* "$3"/.[!.]* "$3"/..?*; do
    [ -e "$tl_f" ] || [ -L "$tl_f" ] || continue
    tl_d="$4/${tl_f##*/}"
    if [ -d "$tl_f" ] && [ ! -L "$tl_f" ] && [ -d "$tl_d" ] && [ ! -L "$tl_d" ]; then
      tl_merge "$1" "$2" "$tl_f" "$tl_d" || return 1
    elif [ -e "$tl_d" ] || [ -L "$tl_d" ]; then
      case "$2" in
        overwrite) tl_put "$1" "$tl_f" "$tl_d" || return 1 ;;
        older) if [ "$tl_f" -nt "$tl_d" ]; then tl_put "$1" "$tl_f" "$tl_d" || return 1; fi ;;
      esac
    else
      tl_xfer "$1" "$tl_f" "$tl_d" || return 1
    fi
  done
  if [ "$1" = move ]; then rmdir -- "$3" 2>/dev/null; fi
  return 0
}
"#;

/// Facts about the server, gathered once after connecting.
pub const PROBE: &str = r#"
printf 'user=%s\n' "$(id -un 2>/dev/null)"
printf 'uid=%s\n' "$(id -u 2>/dev/null)"
printf 'gid=%s\n' "$(id -g 2>/dev/null)"
printf 'home=%s\n' "$HOME"
printf 'os=%s\n' "$(uname -s 2>/dev/null)"
printf 'host=%s\n' "$(uname -n 2>/dev/null)"
if find / -maxdepth 0 -printf '' >/dev/null 2>&1; then echo find_printf=1; fi
if stat -c %s / >/dev/null 2>&1; then echo stat_c=1; elif stat -f %z / >/dev/null 2>&1; then echo stat_f=1; fi
if cp --version 2>/dev/null | grep -q GNU; then echo gnu_cp=1; fi
printf 'tar=%s\n' "$(tar --version 2>/dev/null | head -n 1)"
for c in sudo tar base64 rsync du df xargs readlink sha256sum shasum awk mktemp; do
  if command -v "$c" >/dev/null 2>&1; then echo "has=$c"; fi
done
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    /// Runs like sshd does: in a session of its own, so the watchdog's
    /// `kill 0` cannot reach the test runner.
    fn isolated(script: &[u8]) -> Command {
        use std::os::unix::ffi::OsStrExt;
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(std::ffi::OsStr::from_bytes(script));
        crate::master::detach(&mut cmd);
        cmd
    }

    fn sh(script: &[u8]) -> (i32, String, String) {
        let out = isolated(script).stdin(Stdio::null()).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    #[test]
    fn merge_functions_work_in_local_sh() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src/sub")).unwrap();
        std::fs::create_dir_all(root.join("dst/sub")).unwrap();
        std::fs::write(root.join("src/sub/a.txt"), "new").unwrap();
        std::fs::write(root.join("src/.hidden"), "h").unwrap();
        std::fs::write(root.join("src/sub/b.txt"), "b").unwrap();
        std::fs::write(root.join("dst/sub/a.txt"), "old").unwrap();

        let mut script = Script::new();
        script.raw(MERGE_FUNCTIONS).raw("TL_CP=-pR\ncd ").arg(root.as_os_str().as_encoded_bytes());
        script.raw("\ntl_merge copy skip src dst\n");
        let (code, _, err) = sh(script.as_bytes());
        assert_eq!(code, 0, "{err}");
        assert_eq!(std::fs::read_to_string(root.join("dst/sub/a.txt")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(root.join("dst/sub/b.txt")).unwrap(), "b");
        assert_eq!(std::fs::read_to_string(root.join("dst/.hidden")).unwrap(), "h");

        let mut script = Script::new();
        script.raw(MERGE_FUNCTIONS).raw("cd ").arg(root.as_os_str().as_encoded_bytes());
        script.raw("\ntl_merge move skip src dst\n");
        let (code, _, err) = sh(script.as_bytes());
        assert_eq!(code, 0, "{err}");
        // The conflicting file stays behind in the source.
        assert!(root.join("src/sub/a.txt").exists());

        let mut script = Script::new();
        script.raw(MERGE_FUNCTIONS).raw("cd ").arg(root.as_os_str().as_encoded_bytes());
        script.raw("\ntl_merge move overwrite src dst\n");
        let (code, _, err) = sh(script.as_bytes());
        assert_eq!(code, 0, "{err}");
        assert_eq!(std::fs::read_to_string(root.join("dst/sub/a.txt")).unwrap(), "new");
        assert!(!root.join("src").exists());
    }

    #[test]
    fn watchdog_passes_exit_status() {
        let mut child = isolated(&with_watchdog(b"echo hi; exit 3"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        // Keep stdin open like the client does; closing it would cancel.
        let _stdin = child.stdin.take();
        let mut out = String::new();
        std::io::Read::read_to_string(child.stdout.as_mut().unwrap(), &mut out).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(3));
        assert_eq!(out, "hi\n");
    }

    #[test]
    fn watchdog_kills_on_stdin_eof() {
        use std::io::Write;
        let script = with_watchdog(b"sleep 30; echo finished");
        let mut child = isolated(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let started = std::time::Instant::now();
        drop(stdin);
        let out = child.wait_with_output().unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(!String::from_utf8_lossy(&out.stdout).contains("finished"));
    }
}
