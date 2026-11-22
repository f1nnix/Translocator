#!/bin/sh
# Builds the native macOS app and wraps it in target/Translocator.app.
#   macos/bundle.sh            debug, this machine's architecture
#   macos/bundle.sh --release  release, arm64 and x86_64
#
# Builds the Rust core first (build-core.sh, same profile), then the Swift
# package. The bundle is signed ad hoc with a fixed identifier, so macOS
# keeps its privacy grants (Local Network among them) across rebuilds.
set -eu
cd "$(dirname "$0")/.."

profile=debug
for arg in "$@"; do
  case "$arg" in
    --release) profile=release ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

bundle_id=dev.translocator.Translocator
if [ "$profile" = release ]; then
  macos/build-core.sh --release
  swift build --package-path macos -c release --arch arm64 --arch x86_64
  bin=$(swift build --package-path macos -c release --arch arm64 --arch x86_64 --show-bin-path)/Translocator
else
  macos/build-core.sh
  swift build --package-path macos
  bin=$(swift build --package-path macos --show-bin-path)/Translocator
fi

app=target/Translocator.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/Translocator"

# The icon, from the master PNG (as scripts/bundle.sh does).
iconset=target/Translocator.iconset
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
  sips -z "$size" "$size" packaging/translocator-1024.png --out "$iconset/$name.png" > /dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/Translocator.icns"
rm -rf "$iconset"

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
: "${version:=0.0.0}"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>$bundle_id</string>
  <key>CFBundleName</key><string>Translocator</string>
  <key>CFBundleDisplayName</key><string>Translocator</string>
  <key>CFBundleExecutable</key><string>Translocator</string>
  <key>CFBundleIconFile</key><string>Translocator</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>26.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>NSLocalNetworkUsageDescription</key><string>Translocator connects to servers on your local network over SSH.</string>
</dict>
</plist>
PLIST

if [ -n "${MACOS_IDENTITY:-}" ]; then
  codesign --force --sign "$MACOS_IDENTITY" --identifier "$bundle_id" --options runtime --timestamp "$app"
else
  codesign --force --sign - --identifier "$bundle_id" "$app"
fi
codesign --verify --strict "$app"
touch "$app"
echo "$app"
