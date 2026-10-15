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
mkdir -p "$app/Contents/MacOS"
cp "target/$profile/translocator" "$app/Contents/MacOS/translocator"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>dev.translocator.Translocator</string>
  <key>CFBundleName</key><string>Translocator</string>
  <key>CFBundleDisplayName</key><string>Translocator</string>
  <key>CFBundleExecutable</key><string>translocator</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST
echo "$app"
