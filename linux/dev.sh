#!/bin/sh
# Linux development in Docker, for the GTK app and the rest of the workspace.
#
#   linux/dev.sh image             build the image (the other commands do it
#                                  when it is missing)
#   linux/dev.sh build [args]      cargo build -p translocator-gtk [args]
#   linux/dev.sh test [args]       cargo test for every crate that builds on
#                                  Linux (the GPUI shell only with TL_GPUI=1;
#                                  `-p crate` tests just that one)
#   linux/dev.sh clippy [args]     cargo clippy -p translocator-gtk
#   linux/dev.sh run [script]      starts the app under Xvfb with D-Bus,
#                                  runs `script` (a shell file, see below)
#                                  and takes screenshots into $TL_SHOTS
#   linux/dev.sh shell             a shell in the container
#
# The worktree is mounted read-write at /src. The cargo registry and the
# target dir are named Docker volumes (tl-gtk-cargo, tl-gtk-target), never
# the worktree, so the Mac's disk only holds the sources.
#
# `run`: the app starts in a fresh home with sample folders, the window is
# 1400x880. Without a script, one screenshot `main.png` is taken after
# $TL_WAIT seconds (default 6). A script is sourced inside the container
# after the app is up; it can use xdotool and these helpers:
#   shot NAME        writes $TL_SHOTS/NAME.png
#   key KEYS...      xdotool key (the window gets the X focus first)
#   text TEXT        xdotool type, likewise
#   click X Y [BUTTON], drag X1 Y1 X2 Y2
#   pause SECONDS
# $TL_SETTINGS, if set, is written as the settings file first (JSON, e.g.
# '{"appearance":{"mode":"dark","dark":"Nord"}}').
# $TL_SHOTS defaults to target/gtk-shots in the worktree (git-ignored).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
image=${TL_IMAGE:-translocator-gtk-dev}
shots=${TL_SHOTS:-$root/target/gtk-shots}

ensure_image() {
    if [ "${1:-}" = force ] || ! docker image inspect "$image" >/dev/null 2>&1; then
        docker build -t "$image" -f "$here/Dockerfile" "$root"
    fi
}

# docker run with the source, the volumes and a lean build profile: no
# incremental artifacts and line tables only, which keeps the target volume
# a fraction of a full debug build.
in_docker() {
    tty=""
    [ -t 0 ] && [ -t 1 ] && tty="-t"
    docker run --rm -i $tty \
        -v "$root:/src" \
        -v tl-gtk-cargo:/usr/local/cargo/registry \
        -v tl-gtk-target:/target \
        -e CARGO_TARGET_DIR=/target \
        -e CARGO_INCREMENTAL=0 \
        -e CARGO_PROFILE_DEV_DEBUG=line-tables-only \
        -e CARGO_PROFILE_TEST_DEBUG=line-tables-only \
        -e CARGO_TERM_COLOR=always \
        -e RUST_BACKTRACE=1 \
        "$@"
}

# Every workspace crate but the GPUI shell, whose Linux build needs a GPU
# stack and is CI's business.
packages="-p tl-model -p tl-core -p tl-local -p tl-ssh -p tl-s3 -p tl-webdav
    -p tl-app -p tl-runtime -p tl-ffi -p tl-themes -p translocator-gtk"

cmd=${1:-build}
[ $# -gt 0 ] && shift
case "$cmd" in
    image)
        ensure_image force
        ;;
    build)
        ensure_image
        in_docker "$image" cargo build -p translocator-gtk "$@"
        ;;
    test)
        ensure_image
        extra=""
        [ "${TL_GPUI:-0}" = 1 ] && extra="-p translocator"
        # Packages named on the command line replace the list.
        case " $* " in *" -p "*|*" --package "*) packages="" extra="" ;; esac
        # shellcheck disable=SC2086
        in_docker "$image" cargo test $packages $extra "$@"
        ;;
    clippy)
        ensure_image
        in_docker "$image" cargo clippy -p translocator-gtk --all-targets "$@" -- -D warnings
        ;;
    run)
        ensure_image
        mkdir -p "$shots"
        script=/dev/null
        if [ $# -gt 0 ]; then
            script_dir=$(cd "$(dirname "$1")" && pwd)
            mount_script="-v $script_dir/$(basename "$1"):/run/tl-script.sh:ro"
            script=/run/tl-script.sh
        else
            mount_script=""
        fi
        in_docker $mount_script -v "$shots:/shots" -e TL_WAIT="${TL_WAIT:-6}" -e TL_SETTINGS="${TL_SETTINGS:-}" \
            -e TL_SCRIPT="$script" "$image" sh /src/linux/run-headless.sh
        echo "screenshots in $shots"
        ;;
    shell)
        ensure_image
        in_docker "$image" bash "$@"
        ;;
    *)
        echo "usage: linux/dev.sh image|build|test|clippy|run [script]|shell" >&2
        exit 2
        ;;
esac
