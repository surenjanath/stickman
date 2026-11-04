#!/bin/sh
# Builds Stickman.app and a zip of it, ready to attach to a GitHub release.
#
#   packaging/bundle.sh 0.1.0
#
# Output: dist/Stickman.app and dist/Stickman-<version>-macos-arm64.zip
# The app is ad-hoc signed (no Apple Developer ID), so macOS asks the user to
# confirm the first launch; see the release notes.
set -eu

VERSION="${1:?usage: packaging/bundle.sh <version>}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIST="$ROOT/dist"
APP="$DIST/Stickman.app"
TARGET=aarch64-apple-darwin

cd "$ROOT"
cargo build --release --target "$TARGET"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/$TARGET/release/stickman" "$APP/Contents/MacOS/stickman"

# App icon: every size macOS wants, from the 1024 px master.
ICONSET="$DIST/AppIcon.iconset"
rm -rf "$ICONSET"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" packaging/icon-1024.png --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" packaging/icon-1024.png --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "$ICONSET"

# LSUIElement: no Dock icon (it lives in the menu bar).
cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Stickman</string>
    <key>CFBundleDisplayName</key><string>Stickman</string>
    <key>CFBundleIdentifier</key><string>com.surenjanath.stickman</string>
    <key>CFBundleExecutable</key><string>stickman</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>13.0</string>
    <key>LSUIElement</key><true/>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSHumanReadableCopyright</key><string>GPL-3.0. github.com/surenjanath/stickman</string>
</dict>
</plist>
EOF

codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"

ZIP="$DIST/Stickman-$VERSION-macos-arm64.zip"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"
echo "built $ZIP"
