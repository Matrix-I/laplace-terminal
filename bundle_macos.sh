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

echo "==> Building optimized macOS release binary..."
if [ -f "$HOME/.cargo/env" ]; then
    . "$HOME/.cargo/env"
fi
cargo build --release


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

if [ -d "$HOME/Applications" ]; then
    echo "==> Installing to $HOME/Applications/$APP_NAME..."
    rm -rf "$HOME/Applications/$APP_NAME"
    cp -R "$APP_DIR" "$HOME/Applications/"
    codesign --force --deep --sign - "$HOME/Applications/$APP_NAME"
fi

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

echo "==> Creating standalone binary and zip archive..."
cp "target/release/laplace" "$DIST_DIR/laplace-macos"
chmod +x "$DIST_DIR/laplace-macos"
(cd "$DIST_DIR" && rm -f Laplace-macos.zip && zip -q -r Laplace-macos.zip "$APP_NAME")

echo "==> Done! Outputs ready:"
ls -lh "$APP_DIR"
ls -lh "$DMG_PATH"
ls -lh "$DIST_DIR/laplace-macos"
ls -lh "$DIST_DIR/Laplace-macos.zip"

