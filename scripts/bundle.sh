#!/bin/sh
# Builds Translocator and wraps it in a macOS .app bundle.
#   scripts/bundle.sh            debug build  -> target/debug/Translocator.app
#   scripts/bundle.sh --release  release build -> target/release/Translocator.app
set -eu
cd "$(dirname "$0")/.."

profile=debug
if [ "${1:-}" = "--release" ]; then
  profile=release
  cargo build -p translocator --release
else
  cargo build -p translocator
fi

app="target/$profile/Translocator.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "target/$profile/translocator" "$app/Contents/MacOS/translocator"

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
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

# Finder caches an app's icon by bundle mtime; bump it so a rebuilt bundle
# does not keep showing the old (or missing) one.
touch "$app"
echo "$app"
