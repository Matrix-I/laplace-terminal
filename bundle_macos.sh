#!/bin/bash
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$DIR"

APP_NAME="Laplace.app"
DIST_DIR="$DIR/dist"
APP_DIR="$DIST_DIR/$APP_NAME"
CONTENTS="$APP_DIR/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES="$CONTENTS/Resources"

if [ ! -f "target/release/laplace" ]; then
    echo "==> Building optimized macOS release binary..."
    . "$HOME/.cargo/env"
    cargo build --release
fi

echo "==> Packaging $APP_NAME..."
rm -rf "$APP_DIR"
mkdir -p "$MACOS" "$RESOURCES"

# Copy binary
cp "target/release/laplace" "$MACOS/laplace"
chmod +x "$MACOS/laplace"

# Copy Info.plist and Icon
cp "resources/Info.plist" "$CONTENTS/Info.plist"
cp "resources/Laplace.icns" "$RESOURCES/Laplace.icns"

# Sign ad-hoc so macOS Gatekeeper allows immediate execution without quarantine warnings
codesign --force --deep --sign - "$APP_DIR"

echo "==> $APP_NAME created at $APP_DIR"

# Also package a convenient drag-and-drop DMG for distribution
DMG_NAME="Laplace.dmg"
DMG_PATH="$DIST_DIR/$DMG_NAME"
DMG_STAGING="$DIST_DIR/dmg_staging"

echo "==> Creating installable disk image ($DMG_NAME)..."
rm -rf "$DMG_STAGING" "$DMG_PATH"
mkdir -p "$DMG_STAGING"

cp -R "$APP_DIR" "$DMG_STAGING/"
ln -s /Applications "$DMG_STAGING/Applications"

hdiutil create -volname "Laplace" \
    -srcfolder "$DMG_STAGING" \
    -ov -format UDZO \
    "$DMG_PATH" > /dev/null

rm -rf "$DMG_STAGING"

echo "==> Done! Outputs ready:"
ls -lh "$APP_DIR"
ls -lh "$DMG_PATH"
