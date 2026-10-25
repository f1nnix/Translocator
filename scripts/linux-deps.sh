#!/bin/sh
# Installs the system packages Translocator needs on Linux, for the
# distribution it is run on. Used by CI and by local Docker builds, so both
# install exactly the same things.
#
#   scripts/linux-deps.sh build    build-time packages (default)
#   scripts/linux-deps.sh runtime  the shared libraries a built binary needs
#
# Must run as root (or under sudo).
set -eu

mode=${1:-build}

# Build: headers and pkg-config files. fontconfig and freetype are linked
# through pkg-config; wayland, xkbcommon, X11 and Vulkan are dlopened at
# runtime, so only the runtime packages matter for those.
# zip and unzip are not needed by the app, but the archive tests use them to
# exercise the zip paths; without them those tests quietly skip.
apt_build="build-essential pkg-config curl ca-certificates git zip unzip
    libfontconfig-dev libfreetype-dev libwayland-dev libxkbcommon-dev
    libxkbcommon-x11-dev libx11-dev libxcb1-dev libdbus-1-dev"
apt_runtime="libfontconfig1 libfreetype6 libwayland-client0 libwayland-cursor0
    libwayland-egl1 libxkbcommon0 libxkbcommon-x11-0 libx11-6 libxcb1
    libdbus-1-3 libvulkan1 mesa-vulkan-drivers openssh-client xdg-utils fonts-dejavu-core"

dnf_build="gcc gcc-c++ make pkgconf-pkg-config curl ca-certificates git zip unzip
    fontconfig-devel freetype-devel wayland-devel libxkbcommon-devel
    libxkbcommon-x11-devel libX11-devel libxcb-devel dbus-devel"
dnf_runtime="fontconfig freetype libwayland-client libwayland-cursor
    libwayland-egl libxkbcommon libxkbcommon-x11 libX11 libxcb dbus-libs
    vulkan-loader mesa-vulkan-drivers openssh-clients xdg-utils dejavu-sans-fonts"

if command -v apt-get >/dev/null 2>&1; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    case "$mode" in
        build) set -- $apt_build $apt_runtime ;;
        runtime) set -- $apt_runtime ;;
        *) echo "unknown mode: $mode" >&2; exit 2 ;;
    esac
    apt-get install -y --no-install-recommends "$@"
elif command -v dnf >/dev/null 2>&1; then
    case "$mode" in
        build) set -- $dnf_build $dnf_runtime ;;
        runtime) set -- $dnf_runtime ;;
        *) echo "unknown mode: $mode" >&2; exit 2 ;;
    esac
    dnf install -y --setopt=install_weak_deps=False "$@"
else
    echo "no supported package manager (apt-get or dnf) found" >&2
    exit 2
fi
