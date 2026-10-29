#!/bin/bash
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$DIR"

APP_NAME="Litty.app"
DIST_DIR="$DIR/dist"
APP_DIR="$DIST_DIR/$APP_NAME"
CONTENTS="$APP_DIR/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES="$CONTENTS/Resources"

echo "==> Building optimized macOS release binary..."
if [ -z "${SDKROOT:-}" ]; then
    export SDKROOT="$(xcrun --show-sdk-path 2>/dev/null || true)"
fi
if [ -f "$HOME/.cargo/env" ]; then
    . "$HOME/.cargo/env"
fi
cargo build --release


echo "==> Packaging $APP_NAME..."
rm -rf "$APP_DIR"
mkdir -p "$MACOS" "$RESOURCES"

# Copy binary
cp "target/release/litty" "$MACOS/litty"
chmod +x "$MACOS/litty"
ln -sf "litty" "$MACOS/laplace"

# Copy Info.plist and Icon
cp "resources/Info.plist" "$CONTENTS/Info.plist"
cp "resources/Litty.icns" "$RESOURCES/Litty.icns"

# Sign ad-hoc so macOS Gatekeeper allows immediate execution without quarantine warnings
codesign --force --deep --sign - "$APP_DIR"
touch "$APP_DIR"

echo "==> $APP_NAME created at $APP_DIR"

if [ -d "$HOME/Applications" ]; then
    echo "==> Installing to $HOME/Applications/$APP_NAME..."
    rm -rf "$HOME/Applications/$APP_NAME" 2>/dev/null || true
    cp -R "$APP_DIR" "$HOME/Applications/" 2>/dev/null || true
    codesign --force --deep --sign - "$HOME/Applications/$APP_NAME" 2>/dev/null || true
    touch "$HOME/Applications/$APP_NAME" 2>/dev/null || true
fi

# Also package a convenient drag-and-drop DMG for distribution
DMG_NAME="Litty.dmg"
DMG_PATH="$DIST_DIR/$DMG_NAME"
DMG_STAGING="$DIST_DIR/dmg_staging"

echo "==> Creating installable disk image ($DMG_NAME)..."
rm -rf "$DMG_STAGING" "$DMG_PATH"
mkdir -p "$DMG_STAGING"

cp -R "$APP_DIR" "$DMG_STAGING/"
ln -s /Applications "$DMG_STAGING/Applications"

hdiutil create -volname "Litty" \
    -srcfolder "$DMG_STAGING" \
    -ov -format UDZO \
    "$DMG_PATH" > /dev/null 2>&1 || true

rm -rf "$DMG_STAGING"

echo "==> Creating standalone binary and zip archive..."
cp "target/release/litty" "$DIST_DIR/litty-macos"
chmod +x "$DIST_DIR/litty-macos"
(cd "$DIST_DIR" && rm -f Litty-macos.zip && zip -q -r Litty-macos.zip "$APP_NAME")

# Architecture-suffixed copies (Rust `std::env::consts::ARCH` naming) for the self-updater.
ARCH="$(uname -m)"
[ "$ARCH" = "arm64" ] && ARCH="aarch64"
cp "$DIST_DIR/litty-macos" "$DIST_DIR/litty-macos-$ARCH"
cp "$DIST_DIR/Litty-macos.zip" "$DIST_DIR/Litty-macos-$ARCH.zip"

echo "==> Done! Outputs ready:"
ls -lh "$APP_DIR"
ls -lh "$DMG_PATH"
ls -lh "$DIST_DIR/litty-macos"
ls -lh "$DIST_DIR/Litty-macos.zip"
