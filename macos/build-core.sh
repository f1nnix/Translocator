#!/bin/sh
# Builds the Rust core (crates/tl-ffi) for the macOS app:
#   macos/build-core.sh            debug
#   macos/build-core.sh --release  release
#
# 1. the static library for arm64 and x86_64, joined with lipo;
# 2. the Swift bindings, generated from that library by uniffi's own
#    generator (crates/tl-ffi/src/bin/uniffi-bindgen-swift.rs), so they
#    always match it: Sources/Translocator/Core/TLCore.swift plus the C
#    header and
#    module map of TLCoreFFI;
# 3. build/TLCoreFFI.xcframework, the binary target Package.swift links.
#
# Then `swift build` (or macos/bundle.sh) builds the app.
set -eu
cd "$(dirname "$0")/.."

profile=debug
release_flag=
for arg in "$@"; do
  case "$arg" in
    --release) profile=release; release_flag=--release ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

# The app's minimum: objects built for an older system would make the
# linker warn about every one of them.
export MACOSX_DEPLOYMENT_TARGET=26.0

build=macos/build
mkdir -p "$build"

# Both targets need a rustup toolchain: a rust from a package manager (which
# may come first on PATH) has only its host's standard library.
if command -v rustup > /dev/null; then
  toolchain=${RUSTUP_TOOLCHAIN:-stable}
  rustup target add --toolchain "$toolchain" aarch64-apple-darwin x86_64-apple-darwin > /dev/null 2>&1 || true
  # Not `rustup run`: some rustup builds leave PATH alone, and the other
  # rust would still be found first.
  bin=$(dirname "$(rustup which --toolchain "$toolchain" cargo)")
  PATH="$bin:$PATH"
  export PATH
fi
# shellcheck disable=SC2086  # $release_flag is one optional word
cargo build -p tl-ffi $release_flag --target aarch64-apple-darwin --target x86_64-apple-darwin

lib="$build/libtl_ffi.a"
lipo -create -output "$lib" \
  "target/aarch64-apple-darwin/$profile/libtl_ffi.a" \
  "target/x86_64-apple-darwin/$profile/libtl_ffi.a"

# Bindings from the library's own objects: uniffi reads the interface out of
# the tl_ffi members, and handing it only those (not the whole archive with
# every dependency) takes seconds instead of many minutes.
meta="$build/meta"
rm -rf "$meta"
mkdir -p "$meta"
arm="$PWD/target/aarch64-apple-darwin/$profile/libtl_ffi.a"
(
  cd "$meta"
  ar t "$arm" | grep '^tl_ffi-' | xargs ar x "$arm"
  ar rcs libtl_ffi_meta.a tl_ffi-*.o
)
bindings="$build/bindings"
rm -rf "$bindings"
# A plain (not "framework") module map: the xcframework holds a library.
cargo run -q -p tl-ffi --target aarch64-apple-darwin --features cli --bin uniffi-bindgen-swift -- \
  "$meta/libtl_ffi_meta.a" "$bindings" \
  --swift-sources --headers --modulemap \
  --module-name TLCoreFFI --modulemap-filename module.modulemap
rm -rf "$meta"

# Compiled into the app's own target: Xcode 26's swift-build cannot link a
# library target over a binary one (`library not found for -ltl_ffi`). The
# folder holds only this generated file, so a fresh checkout has none.
mkdir -p macos/Sources/Translocator/Core
cp "$bindings/TLCore.swift" macos/Sources/Translocator/Core/TLCore.swift

headers="$build/include"
rm -rf "$headers"
mkdir -p "$headers"
cp "$bindings/TLCoreFFI.h" "$bindings/module.modulemap" "$headers/"

rm -rf "$build/TLCoreFFI.xcframework"
xcodebuild -create-xcframework -library "$lib" -headers "$headers" \
  -output "$build/TLCoreFFI.xcframework" > /dev/null
echo "$build/TLCoreFFI.xcframework ($profile)"
