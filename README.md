# Translocator

[![Linux](https://github.com/f1nnix/Translocator/actions/workflows/linux.yml/badge.svg)](https://github.com/f1nnix/Translocator/actions/workflows/linux.yml)

A dual-pane file manager for macOS and Linux, in Rust on [gpui](https://gpui.rs) and [gpui-component](https://gpui-kit.com). Local disks, SSH servers, S3 and WebDAV in the same two panes, with the keys you already know.

## Features

- **Providers** — local disks, SSH servers, S3 accounts and WebDAV shares, any of them in either pane. SSH is the interesting one: copies that run on the server, and sudo. It is [below](#ssh-provider).
- **Advanced file operations** — copy and move with per-item conflict resolution, queued in the background. Compare and synchronize two folders, multi-rename from masks, compress and extract, open archives as folders, view and edit a file that lives on a server.
- **Bookmarks per host** — every server, every account and this machine keeps its own row of folders under the pane. ⌘D saves the one you are in, ⌘1 … ⌘9 jump.
- **Everything from the keyboard** — type to filter the list, mark with Space, find files, check permissions and sizes, open a command line or a terminal in the folder you are looking at. The [keys](#keys) are the ones you already know.

## SSH provider

### Why

A remote pane is usually SFTP, which can only move bytes: copying a folder from one directory to another on the same server means pulling it down and pushing it back up, `sudo` is out of reach, and nothing the machine already has can help. This plugin runs ordinary shell commands over one multiplexed OpenSSH connection instead, so a remote pane behaves like a terminal on that machine — the server does the work, with your own ssh setup and nothing installed on the far end.

### How it works

- **Copies run on the server** — two folders on one host is one `cp -a` or `mv` there, however large; nothing crosses your network. Between two hosts it is a tar stream through this machine, with no temporary files.
- **sudo, per pane** — the lock button or ⌘⇧S. `sudo -n` where NOPASSWD is set, otherwise the password is asked once per connection, kept in memory and fed through `sudo -S`, never on a command line. A "Permission denied" anywhere offers the retry as root. Overwriting `/etc/nginx/nginx.conf` leaves it `root:root 0644`.
- **Your own ssh** — `~/.ssh/config`, keys, agent, jump hosts. Passwords, passphrases, 2FA codes and unknown host keys arrive as dialogs. One `ControlMaster` per bookmark, so no operation re-authenticates.
- **Any POSIX shell, any tools** — commands are short `sh` scripts behind an output marker, so a chatty `.bashrc` and a fish login shell are both fine, and GNU, BusyBox and BSD tools are detected and used as they are. An Alpine box works like a Debian one. No SFTP subsystem required.
- **Nothing is left running** — long jobs sit under a watchdog that kills the remote process group when you cancel, quit, or lose the connection.

Listings, `find`, chmod, chown, archives and multi-rename all work the same way: the server's own tools, one command per operation.

## Keys

| Key | Action |
|---|---|
| ↑ ↓ PgUp PgDn Home End | Move cursor |
| Space / Insert, ⇧↑ ⇧↓ | Mark / unmark |
| ⌘A, ⌘⇧A, ⌘⌥A | Mark all (what the filter shows), unmark all, invert |
| `+`, `-` | Mark / unmark by mask |
| Type letters | Filter the list to the names that match |
| ⌫ | One character off the filter, or the enclosing folder |
| Esc | Clear the filter |
| Enter | Open (a tar, tar.gz or zip opens as a folder) |
| Tab | Switch pane |
| F2 / ⇧F6 | Rename |
| F3 / F4 / ⇧F4 | View / edit / new file |
| F5 / F6 | Copy / move to the other pane |
| F7 / ⌘⇧N | New folder (`a/b/c` creates nested folders) |
| F8 / ⌘⌫ | Move to Trash (permanent on servers, with a warning) |
| ⇧F8 / ⌘⌥⌫ | Delete permanently |
| ⌘I | Properties: permissions, owner and group |
| ⌥Enter | Calculate folder size |
| ⌘F / ⌥F7 | Find files |
| ⇧F2 / ⌘⇧C | Compare directories |
| ⌘⌥S | Synchronize directories |
| ⌃M / ⌘⇧M | Multi-rename the marked files |
| ⇧⌥F5 / ⇧⌥F9 | Compress / decompress in this folder |
| ⌥F5 / ⌥F9 | Pack / extract into the other pane |
| ⌘K, ⌥F1 / ⌥F2 | Locations: saved folders, this Mac, volumes, bookmarks |
| ⌘D, ⌘1 … ⌘9 | Save this folder on the row; go to the n-th |
| ⌘⇧G / ⌘L | Go to folder |
| ⌘[ ⌘] | Back / forward |
| ⌘T, ⌘W, ⌃Tab | New tab, close tab, next tab |
| ⌘U, ⌘= | Swap panes, same folder in the other pane |
| ⌘C ⌘X ⌘V | Copy / cut / paste files (across servers too) |
| ⌘⌥C | Copy path |
| ⌘⇧S | Toggle sudo for the pane |
| ⌘E | Command line in the pane's folder |
| ⌘⇧T | Open Terminal here |
| ⌘B | Manage bookmarks |
| ⌘J, ⌘⇧J | Jobs, console output |
| ⌘⇧. | Show hidden files |

## Install

On **Linux**, take the `.deb` or `.rpm` from the [releases](https://github.com/f1nnix/Translocator/releases).

To build it yourself:

```bash
sudo scripts/linux-deps.sh build        # Linux, once; knows apt and dnf
cargo build --release -p translocator
```

```bash
scripts/bundle.sh --release             # macOS → target/release/Translocator.app
```

macOS builds are not published yet: that needs signing and notarization keys.

## License

MIT. See [LICENSE](LICENSE).
