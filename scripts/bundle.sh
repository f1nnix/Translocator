#!/bin/sh
# Builds Translocator and wraps it in a macOS .app bundle.
#   scripts/bundle.sh                        debug   -> target/debug/Translocator.app
#   scripts/bundle.sh --release              release -> target/release/Translocator.app
#   scripts/bundle.sh --release --universal  one binary for both architectures
#
# The bundle is signed ad hoc, which is all a build for this machine needs.
# Set MACOS_IDENTITY to a "Developer ID Application" identity (the macOS
# workflow does) to sign it for distribution instead, with the hardened
# runtime and a timestamp from Apple that notarization insists on.
set -eu
cd "$(dirname "$0")/.."

profile=debug
universal=no
for arg in "$@"; do
  case "$arg" in
    --release) profile=release ;;
    --universal) universal=yes ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

release_flag=
if [ "$profile" = release ]; then
  release_flag=--release
fi

app="target/$profile/Translocator.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

if [ "$universal" = yes ]; then
  # A rustup toolchain needs the second target installed; a rust from a
  # package manager has whatever it shipped with, and cargo will say so.
  if command -v rustup > /dev/null; then
    rustup target add aarch64-apple-darwin x86_64-apple-darwin
  fi
  # shellcheck disable=SC2086  # $release_flag is one optional word
  cargo build -p translocator $release_flag \
    --target aarch64-apple-darwin --target x86_64-apple-darwin
  lipo -create -output "$app/Contents/MacOS/translocator" \
    "target/aarch64-apple-darwin/$profile/translocator" \
    "target/x86_64-apple-darwin/$profile/translocator"
else
  # shellcheck disable=SC2086
  cargo build -p translocator $release_flag
  cp "target/$profile/translocator" "$app/Contents/MacOS/translocator"
fi

# The Dock and Finder read the icon from the bundle, so build an .icns out of
# the master PNG. sips and iconutil both ship with macOS.
iconset="target/$profile/Translocator.iconset"
rm -rf "$iconset"
mkdir -p "$iconset"
for size_name in \
  "16 icon_16x16" "32 icon_16x16@2x" \
  "32 icon_32x32" "64 icon_32x32@2x" \
  "128 icon_128x128" "256 icon_128x128@2x" \
  "256 icon_256x256" "512 icon_256x256@2x" \
  "512 icon_512x512" "1024 icon_512x512@2x"
do
  size=${size_name%% *}
  name=${size_name#* }
  sips -z "$size" "$size" packaging/translocator-1024.png --out "$iconset/$name.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/Translocator.icns"
rm -rf "$iconset"

# One version for the whole workspace, so the bundle says what cargo built.
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
: "${version:=0.0.0}"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>dev.translocator.Translocator</string>
  <key>CFBundleName</key><string>Translocator</string>
  <key>CFBundleDisplayName</key><string>Translocator</string>
  <key>CFBundleExecutable</key><string>translocator</string>
  <key>CFBundleIconFile</key><string>Translocator</string>
  <key>NSLocalNetworkUsageDescription</key><string>Translocator connects to servers on your local network over SSH.</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

# The linker signs the binary ad hoc with an identifier taken from its own
# bytes, so every rebuild is a different application to macOS and starts again
# with no privacy grants -- Local Network among them, without which connecting
# to a LAN server fails with "No route to host". Signing the finished bundle
# gives it one identifier and binds Info.plist to the signature.
if [ -n "${MACOS_IDENTITY:-}" ]; then
  codesign --force --sign "$MACOS_IDENTITY" --identifier dev.translocator.Translocator \
    --options runtime --timestamp "$app"
else
  codesign --force --sign - --identifier dev.translocator.Translocator "$app"
fi
codesign --verify --strict "$app"

# Finder caches an app's icon by bundle mtime; bump it so a rebuilt bundle
# does not keep showing the old (or missing) one.
touch "$app"
echo "$app"
