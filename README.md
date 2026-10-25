# Translocator

[![Linux](https://github.com/f1nnix/Translocator/actions/workflows/linux.yml/badge.svg)](https://github.com/f1nnix/Translocator/actions/workflows/linux.yml)

A classic dual-pane file manager for macOS and Linux, written in Rust on
[gpui](https://gpui.rs) and [gpui-component](https://gpui-kit.com). Remote
locations are plugins; the first one, **SSH**, does not use SFTP: it drives a
plain shell on the server through your own OpenSSH client, so working with a
server feels exactly like working in a terminal there — `cp`, `mv`, `chmod`,
`sudo` and all.

## Building

On **macOS**, this wraps the binary in an app bundle:

```bash
scripts/bundle.sh --release   # → target/release/Translocator.app
```

On **Linux**, install the system libraries once (the script knows apt and
dnf), then build normally:

```bash
sudo scripts/linux-deps.sh build
```

```bash
cargo build --release -p translocator
```

## Packages and CI

`.github/workflows/linux.yml` builds every push on Ubuntu 24.04 and Fedora,
runs the test suite on both, and uploads a `.deb` and an `.rpm` as run
artifacts; each job also installs its own package, so a missing dependency
fails the build. Pushing a `v*` tag attaches both packages to a GitHub
release. macOS is not in CI yet: releasing it needs signing and notarization
keys.

To build the packages by hand, on the matching distribution:

```bash
cargo deb -p translocator            # → target/debian/*.deb
```

```bash
cargo generate-rpm -p crates/translocator   # → target/generate-rpm/*.rpm
```

## Layout

| Crate | What it holds |
|---|---|
| `crates/tl-core` | Provider-neutral `Vfs` trait, plugin registry, bookmarks, askpass bridge, transfer engine, directory compare and sync, multi-rename, archives as folders |
| `crates/tl-local` | Local file system (`std::fs`, APFS clones, Trash), archives via the `tar`, `flate2` and `zip` crates |
| `crates/tl-ssh` | SSH plugin: ControlMaster, command protocol, sudo, listings, tar streams, archives with the server's `tar` / `unzip` / `zip` |
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

## Compare, synchronize, multi-rename

* **Compare directories** (⇧F2) marks what differs in both panes: entries
  that exist on one side only and, of two files with the same name, the
  newer one (both, when the times match but the sizes do not). Times less
  than two seconds apart count as equal (FAT). Folders on both sides are not
  descended into. Copy the marked items with F5.
* **Synchronize directories** (⌘⌥S) compares the two panes' folders,
  subfolders included, and lists what would be copied each way: the newer
  file wins, what exists on one side only is copied over (a missing folder as
  a whole), and items that differ without a newer side wait for you. Click an
  item's arrow to change its direction or leave it alone; Both ways / Left →
  right / Right → left resets the suggestions. Nothing is ever deleted.
  Synchronize compares again first and stops if anything it would copy
  changed since the preview; the copies then go through the transfer engine,
  so between two folders on one server they are server-side `cp -a`.
* **Multi-rename** (⌃M) renames the marked files from a name mask and an
  extension mask — `[N]` name, `[E]` extension, `[N2-5]` characters,
  `[C]` counter (start, step, digits), `[P]` folder, `[YMD]` / `[hms]` and
  their parts for the modification time — then find & replace (plain, or
  regex with `$1`) and case conversion, with a live preview. Clashes are
  flagged before anything is renamed; chains and swaps (`a → b`, `b → a`) go
  through temporary names, and if one rename fails the ones already done are
  undone. On servers the whole batch is one command.

## Archives

tar, tar.gz / tgz and zip files open like folders (Enter or ⌃PgDn), and `..`
at the archive's root leads back out. Copying out of an archive extracts,
copying into one adds (replacing members of the same name); F7, ⇧F4, F8 and
the editor work inside archives too. ⌥F9 unpacks the selected archives into a
folder of the other pane; ⌥F5 packs the selection into a new or existing
archive there, the extension picking the format.

* **On this Mac** archives are read and written in-process. A change writes a
  new archive next to the old one and renames it into place.
* **On servers** the server's tools do the work. Listings parse `tar -tv`
  (GNU tar, bsdtar or BusyBox) or read just the zip's central directory with
  `tail -c` (`unzip -l` mangles non-ASCII names). Extracting unpacks the
  chosen members with `tar -x` / `unzip -o` into a temporary folder on the
  server and streams them back as tar. A change unpacks the archive there,
  applies it, packs again with `tar -c` / `zip` and swaps the result in,
  keeping the archive's owner and mode. Changing zip archives needs `zip` on
  the server.

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
| ⇧F2 / ⌘⇧C | Compare directories: mark what differs |
| ⌘⌥S | Synchronize directories |
| ⌃M / ⌘⇧M | Multi-rename the marked files |
| Enter, ⌃PgDn | Open a tar, tar.gz or zip file as a folder |
| ⌥F9 / ⌘⌥E | Extract the selected archives to the other pane |
| ⌥F5 / ⌘⌥P | Pack the selection into an archive in the other pane |
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
scenario against a fake `ssh` that executes commands locally, and a fake `sudo`
that wants a password: with the system's BSD tools and, when Homebrew's GNU
tools are installed, with GNU coreutils over BSD find (the `stat -c` listings
BusyBox servers get) and with GNU coreutils plus findutils.

To run them over real SSH against Linux servers as well, start two throwaway
Docker containers (sshd on 127.0.0.1 only, sudo behind a password, your
`~/.ssh` untouched):

```bash
scripts/ssh-test-servers.sh up
```

```bash
scripts/ssh-test-servers.sh test
```

```bash
scripts/ssh-test-servers.sh down
```

Any other server that sees this machine's file system at the same paths works
too; see `real_server()` in the tests for the `TL_SSH_TEST_*` variables.

Verified servers:

| Server | Tools | Login shell | Listings | sudo |
|---|---|---|---|---|
| Debian 13, OpenSSH 10.0 | GNU coreutils 9.7, findutils 4.10, tar 1.35 | bash | `find -printf` | 1.9.16, password |
| Alpine 3.22, OpenSSH 10.0 | BusyBox 1.37 (ash, find, stat, tar) | ash | `stat -c` | 1.9.17, password |
| macOS (local sshd) | BSD tools, optionally Homebrew's GNU tools | zsh, sh | `stat -f`, `find -printf` | fake only |

Not verified yet: Ubuntu 25.10 and later (uutils coreutils, sudo-rs), RHEL
and its relatives, zsh or fish as the login shell on Linux, sudo without a
password on Linux, and sudo to a user other than root.

`crates/tl-ssh/tests/archives.rs` checks archives on a server through the
provider alone, so it runs against any server, shared files or not:
`TL_SSH_ARCHIVE_TEST_HOST` (with `_PORT`, `_KEY`, `_EXTRA`), or the
`TL_SSH_TEST_*` server above. It has passed with GNU tar and Info-ZIP
(Debian), BusyBox tar and unzip (Alpine) and bsdtar with Info-ZIP (macOS).
The app's own tests (`crates/translocator/src/ui_tests.rs`) drive the real
workspace in a headless window: compare, multi-rename, sync, and opening,
extracting, packing and adding to archives.

## Not yet

An SFTP and an S3 plugin, Windows support, a mirror mode for sync
(deleting what the other side lacks), more archive formats (tar.bz2, tar.xz,
7z), renaming inside archives, and archives inside archives. Extracting an
archive into another folder on the same server streams through this Mac.
Names containing newlines are listed correctly only on servers with GNU find.
