# Translocator

A classic dual-pane file manager for macOS, written in Rust on
[gpui](https://gpui.rs) and [gpui-component](https://gpui-kit.com). Remote
locations are plugins; the first one, **SSH**, does not use SFTP: it drives a
plain shell on the server through your own OpenSSH client, so working with a
server feels exactly like working in a terminal there — `cp`, `mv`, `chmod`,
`sudo` and all.

```bash
scripts/bundle.sh            # debug build → target/debug/Translocator.app
```

```bash
scripts/bundle.sh --release  # release build → target/release/Translocator.app
```

## Layout

| Crate | What it holds |
|---|---|
| `crates/tl-core` | Provider-neutral `Vfs` trait, plugin registry, bookmarks, askpass bridge, transfer engine |
| `crates/tl-local` | Local file system (`std::fs`, APFS clones, Trash) |
| `crates/tl-ssh` | SSH plugin: ControlMaster, command protocol, sudo, listings, tar streams |
| `crates/translocator` | The gpui application |

## How copies and moves are routed

Every pane shows a location: a `Vfs` (file system) plus a directory. Each
`Vfs` reports a *location key*; for SSH it is the effective
`user@hostname:port` from `ssh -G` (plus the jump host, if any).

* **Same key** (two folders on the same server, as the same user): the
  provider does the work itself. On SSH that is one `cp -a` / `mv` running
  on the server — no data passes through this Mac. The dialog says so before
  you confirm.
* **Different keys** (local ↔ server, server A ↔ server B): the source emits a
  tar stream (`tar -cf -` on the server, or in-process locally), the engine
  rewrites it on the fly (renames, merge filters) and feeds the destination's
  unpacker (`tar -x` on the server). No temporary files; byte-exact progress.
  Server-to-server transfers stream through this Mac.
* Small files that **overwrite** an existing file are rewritten in place
  (`cat` into the target), so owner, mode and hard links survive — uploading
  `/etc/nginx/nginx.conf` keeps it `root:root 0644`.

Name conflicts are resolved per top-level item: Replace / Replace If Older /
Skip / Keep Both, and for folders Merge (replace, replace older, or keep
existing), with "apply to all". On servers, merges run as a portable shell
function on the server (`tl_merge` in `crates/tl-ssh/src/scripts.rs`); a move
that keeps some files back leaves exactly those files in the source.

## The SSH command protocol

* **Connection**: one `ssh -M -N` ControlMaster per bookmark, started with
  your `~/.ssh/config`, keys and agent. Every operation is a multiplexed
  `ssh -S <socket> host <command>` — no reconnects, no re-authentication.
  The master runs under a small lifeline wrapper, so it dies with the app.
* **Prompts**: `SSH_ASKPASS` points at the app itself. Passwords, key
  passphrases, 2FA codes and unknown host keys show up as dialogs.
* **Scripts**: each operation is a short POSIX `sh` script, run as
  `exec sh -c '…'` so bash, zsh, fish and csh login shells all work. Scripts
  print a marker before their output, so noise from `.bashrc` is discarded —
  even binary tar streams stay clean.
* **Dialects**: on connect a probe detects GNU find (`-printf`, exact
  NUL-separated listings), BusyBox / GNU `stat -c`, or BSD `stat -f`.
* **sudo**: per pane (lock button, ⌘⇧S, or a bookmark default). `sudo -n` is
  used when NOPASSWD is configured; otherwise the password is asked once per
  connection, kept in memory only and fed through `sudo -S` (never on a
  command line). A "Permission denied" anywhere offers a retry with sudo.
  When an elevated copy writes into a pane that is not elevated, the new
  files are handed to your login user.
* **Cancellation**: long jobs run under a watchdog that kills the whole
  remote process group when the session's stdin closes — Cancel, a dropped
  connection, or quitting never leaves a `cp` running on the server.
* **Editor saves**: the new content is uploaded to a temp file and verified
  by size before the target is rewritten in place, so a dropped connection
  can never truncate the file.

## Keys

| Key | Action |
|---|---|
| ↑ ↓ PgUp PgDn Home End | Move cursor |
| Space / Insert, ⇧↑ ⇧↓ | Mark / unmark |
| ⌘A, ⌘⇧A, ⌘⌥A, `+`, `-`, `*` | Mark all, unmark all, invert, mark/unmark by mask, invert |
| Enter, ⌫ | Open, enclosing folder |
| Tab | Switch pane |
| Type letters | Jump to name (type-ahead) |
| F2 / ⇧F6 | Rename |
| F3 / F4 / ⇧F4 | View / edit / new file (built-in editor) |
| F5 / F6 | Copy / move to the other pane |
| F7 / ⌘⇧N | New folder (`a/b/c` creates nested folders) |
| F8 / ⌘⌫ | Move to Trash (permanent delete on servers, with a clear warning) |
| ⇧F8 / ⌘⌥⌫ | Delete permanently |
| ⌘I | Properties: permissions (chmod), owner and group (chown) |
| ⌥Enter | Calculate folder size |
| ⌘F / ⌥F7 | Find files (`find` runs on the server) |
| ⌘K, ⌥F1 / ⌥F2 | Locations: this Mac, volumes, bookmarks |
| ⌘⇧G / ⌘L | Go to folder |
| ⌘[ ⌘] | Back / forward |
| ⌘T, ⌘W, ⌃Tab | New tab, close tab, next tab |
| ⌘U, ⌘= | Swap panes, same folder in the other pane |
| ⌘C ⌘X ⌘V | Copy / cut / paste files (works across servers) |
| ⌘⌥C | Copy path (`user@host:/path` for remote files) |
| ⌘⇧S | Toggle sudo for the pane |
| ⌘E | Command line (runs in the pane's folder, on the server for SSH panes) |
| ⌘⇧T | Open Terminal here (`ssh -t host 'cd …'` for SSH panes) |
| ⌘B, ⌘D | Manage bookmarks, bookmark this folder |
| ⌘J, ⌘⇧J | Jobs, console output |
| ⌘⇧. | Show hidden files |

## Plugins

A connection type implements `ProviderPlugin` (`crates/tl-core/src/plugin.rs`):
it declares its bookmark fields (the bookmark editor renders them
generically), validates them and turns a bookmark into a `Vfs`. Plugins are
compiled in and registered in `crates/translocator/src/app_state.rs`. To add
SFTP or S3, implement `Vfs` for it and register the plugin — copies to and
from every other location work through tar streams automatically, and
same-location copies use whatever server-side operation the provider offers.

## Tests

```bash
cargo test --workspace
```

The SSH plugin's end-to-end tests (`crates/tl-ssh/tests/fake_ssh.rs`) run every
scenario against a fake `ssh` that executes commands locally, once with the
system's BSD tools and once with Homebrew's GNU tools when installed. Point
them at a real server that sees this machine's file system (for example an
`sshd` on localhost) to run them over real SSH as well:

```bash
TL_SSH_TEST_HOST=127.0.0.1 TL_SSH_TEST_PORT=2222 TL_SSH_TEST_KEY=~/.ssh/test_key cargo test -p tl-ssh
```

## Not yet

Archives as folders, directory compare/sync, multi-rename, an SFTP and an S3
plugin, and Linux/Windows packaging. Names containing newlines are listed
correctly only on servers with GNU find.
