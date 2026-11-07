# Internals

How the pieces fit, for anyone changing them. The [README](../README.md)
covers what the app does.

## Layout

| Crate | What it holds |
|---|---|
| `crates/tl-core` | Provider-neutral `Vfs` trait, plugin registry, bookmarks, askpass bridge, transfer engine, directory compare and sync, multi-rename, archives as folders |
| `crates/tl-local` | Local file system (`std::fs`, APFS clones, Trash), archives via the `tar`, `flate2` and `zip` crates |
| `crates/tl-ssh` | SSH plugin: ControlMaster, command protocol, sudo, listings, tar streams, archives with the server's `tar` / `unzip` / `zip` |
| `crates/tl-s3` | S3 plugin: the AWS SDK behind the blocking `Vfs`, bucket list as the root, server-side copies, tar streaming both ways |
| `crates/tl-webdav` | WebDAV plugin: PROPFIND listings, server-side `COPY` and `MOVE`, tar streaming both ways |
| `crates/translocator` | The gpui application |

## Plugins

A connection type implements `ProviderPlugin`
(`crates/tl-core/src/plugin.rs`): it declares its bookmark fields (the
bookmark editor renders them generically), validates them and turns a
bookmark into a `Vfs`. Plugins are compiled in and registered in
`crates/translocator/src/app_state.rs`. To add another — SFTP, say —
implement `Vfs` for it and register the plugin: copies to and from every
other location work through tar streams automatically, and same-location
copies use whatever server-side operation the provider offers.

The `Vfs` trait is blocking and called on worker threads. Paths are
`VPath`, absolute POSIX paths kept as raw bytes, because a remote name is
not guaranteed to be UTF-8.

## How copies and moves are routed

Every pane shows a location: a `Vfs` plus a directory. Each `Vfs` reports a
*location key*; for SSH it is the effective `user@hostname:port` from
`ssh -G` (plus the jump host, if any), for S3 the credentials, endpoint and
region, and for WebDAV the user and base URL.

* **Same key** (two folders on one server, or two buckets of one account):
  the provider does the work itself. On SSH that is one `cp -a` / `mv` on
  the server, on S3 a `CopyObject`, on WebDAV one `COPY` or `MOVE` request —
  no data passes through this machine. The dialog says so before you
  confirm.
* **Different keys** (local ↔ server, server A ↔ server B, anything ↔ S3):
  the source emits a tar stream (`tar -cf -` on the server, built in memory
  for S3 and WebDAV, in-process locally), the engine rewrites it on the fly
  (renames, merge filters) and feeds the destination's unpacker (`tar -x` on
  the server). No temporary files; byte-exact progress. Server-to-server
  transfers stream through this machine.
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

## Archives

tar, tar.gz / tgz and zip files open like folders, and `..` at the
archive's root leads back out. Copying out of an archive extracts, copying
into one adds (replacing members of the same name); F7, ⇧F4, F8 and the
editor work inside archives too. The extension picks the format when
packing, and an existing archive of that name gets the files added to it.

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

## Compare, synchronize, multi-rename

* **Compare directories** marks what differs in both panes: entries that
  exist on one side only and, of two files with the same name, the newer one
  (both, when the times match but the sizes do not). Times less than two
  seconds apart count as equal (FAT). Folders on both sides are not descended
  into.
* **Synchronize directories** compares the two panes' folders, subfolders
  included, and lists what would be copied each way: the newer file wins,
  what exists on one side only is copied over (a missing folder as a whole),
  and items that differ without a newer side wait for you. Nothing is ever
  deleted. Synchronize compares again first and stops if anything it would
  copy changed since the preview; the copies then go through the transfer
  engine, so between two folders on one server they are server-side `cp -a`.
* **Multi-rename** renames the marked files from a name mask and an
  extension mask — `[N]` name, `[E]` extension, `[N2-5]` characters,
  `[C]` counter (start, step, digits), `[P]` folder, `[YMD]` / `[hms]` and
  their parts for the modification time — then find & replace (plain, or
  regex with `$1`) and case conversion, with a live preview. Clashes are
  flagged before anything is renamed; chains and swaps (`a → b`, `b → a`) go
  through temporary names, and if one rename fails the ones already done are
  undone. On servers the whole batch is one command.

## Filtering, marks and saved folders

Typing in a pane filters its list; the filter belongs to the tab, and
stepping into a folder clears it. The whole name is matched, extension
included; a query holding `*` or `?` is a wildcard matched end to end, with
`;` separating several.

A filter is a view, not a selection. `selection()` intersects the marks with
the *listed* rows, so an operation can never touch a file the filter is
hiding, while `rebuild_rows` prunes marks against the whole listing rather
than the visible rows, so narrowing and widening the filter keeps them.
`row_bg`, `marked_fg` and `accent` in `crates/translocator/src/pane.rs` hold
the highlight colours, with a test that keeps the active pane's cursor
distinguishable from the other one's in both themes.

Saved folders are keyed on the same location key that decides whether a copy
runs server-side, so one host keeps one row whichever bookmark opened it.
They live in `bookmarks.json` beside the connection bookmarks; a path that is
not valid UTF-8 is stored as the bytes it really is.

## S3

* **Folders are key prefixes.** An empty one is held open by a zero-byte
  object whose key ends in `/`, which is what `mkdir` creates and what the
  pane hides again when it lists.
* **Copying inside one account never leaves the service.** Two buckets
  reached with the same credentials, endpoint and region share a location, so
  F5 and F6 become `CopyObject` — including between different buckets. A move
  is that copy followed by a delete, because S3 has no rename.
* **To and from anywhere else it streams**, as a tar built and taken apart in
  memory, the same path SSH uses. Files past 64 MB go up as a multipart
  upload, so memory stays flat whatever the size.
* **No permissions, links, trash or shell.** Objects are listed as `0644` and
  folders as `0755` because a row of dashes reads as broken; chmod, chown and
  symlinks are refused, F8 deletes for good, and the command line is off.
* **Symbolic links cannot be stored**, so a copy that contains one fails and
  names it instead of dropping it silently — which matters most for a move,
  where the source would otherwise be deleted.

Anything other than Amazon needs an endpoint, and path-style addressing then
turns itself on. A key typed into a bookmark is stored in `bookmarks.json`,
which is `chmod 600`. The TLS backend is rustls over ring rather than the
default aws-lc, which would need cmake and a long C build on every CI
machine.

## WebDAV

Nextcloud and ownCloud put the files below a path of their own, so the
bookmark URL is the whole thing:
`https://cloud.example.com/remote.php/dav/files/<user>`. A bare host is
assumed to be `https://`, credentials written into the URL are used and then
dropped from it, and a self-signed certificate needs *Trust any certificate*
on that bookmark; otherwise the system's trust store decides, so a corporate
CA already installed works.

* **Copying inside one server never leaves it.** F5 and F6 become one `COPY`
  or `MOVE` request with `Depth: infinity`, whatever the size of the tree, and
  a rename is a `MOVE` that refuses to overwrite. A merge is the exception: it
  needs a decision per file, so those go one request each — still on the
  server.
* **To and from anywhere else it streams**, as a tar built from `GET`s and
  taken apart into `PUT`s. Files up to 8 MB go up from a buffer; past that
  the body is streamed, so memory stays flat whatever the size.
* **Listings are one request.** `PROPFIND` with `Depth: infinity` walks a
  whole tree at once; Apache refuses that by default, and the connection
  notices, says so in the log and walks folder by folder from then on.
* **No permissions, links, trash or shell**, and symbolic links cannot be
  stored, so a copy that contains one fails and names it.
* Free space comes from RFC 4331 (`quota-available-bytes`) when the server
  answers it, and the pane leaves it out when it does not.

The HTTP client is `reqwest::blocking`, not `ureq`: WebDAV needs PROPFIND,
MKCOL, COPY and MOVE, and ureq only sends the methods RFC 9110 lists. It is
rustls over ring, as the S3 provider is.

## Tests

```bash
cargo test --workspace
```

The S3 and WebDAV plugins have end-to-end tests that need a server. Each
has a script that starts a throwaway one in Docker and prints the
environment the tests look for; without that environment they skip
themselves, so the line above works on a machine with neither.

```bash
eval "$(scripts/s3-test-server.sh)"        # MinIO
eval "$(scripts/webdav-test-server.sh)"    # rclone's WebDAV server
cargo test -p tl-s3 -p tl-webdav
scripts/s3-test-server.sh --stop
scripts/webdav-test-server.sh --stop
```

CI runs both, so every push exercises real listings, server-side copies and
moves, merges, streamed transfers in both directions and a file large enough
to go up in several pieces. Any other WebDAV server works too:
`TL_WEBDAV_TEST_URL`, `TL_WEBDAV_TEST_USER` and `TL_WEBDAV_TEST_PASSWORD`.

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
scripts/ssh-test-servers.sh test
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

`crates/translocator/src/ui_tests.rs` drives the real workspace in a headless
window: compare, multi-rename, sync, archives, the filter, the saved-folder
row and the highlight colours. The test window is activated on open, because
gpui reports an empty focus path for an inactive window and no pane would
ever learn it is the active one.

## Packages and CI

`.github/workflows/linux.yml` builds every push on Ubuntu 24.04 and Fedora,
runs the test suite on both, and uploads a `.deb` and an `.rpm` as run
artifacts; each job also installs its own package, so a missing dependency
fails the build. Pushing a `v*` tag attaches both packages to a GitHub
release. macOS is not in CI yet: releasing it needs signing and notarization
keys.

To build the packages by hand, on the matching distribution:

```bash
cargo deb -p translocator                    # → target/debian/*.deb
cargo generate-rpm -p crates/translocator    # → target/generate-rpm/*.rpm
```

A debug build of the workspace is ~26 GB with full debug info, and a runner
has about 14 GB free, so CI sets `CARGO_INCREMENTAL=0` and drops debug info
(`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`), which brings it
to about 2 GB.
