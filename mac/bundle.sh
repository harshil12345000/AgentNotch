#!/bin/bash
# Builds Notch.app (universal: Apple silicon and Intel) and zips it.
# Usage: ./bundle.sh        (run from the mac/ folder, on macOS)
set -euo pipefail
cd "$(dirname "$0")"

swift build -c release --arch arm64 --arch x86_64
BIN=".build/apple/Products/Release/Notch"

APP="dist/Notch.app"
rm -rf dist && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/Notch"
cp Resources/Info.plist "$APP/Contents/Info.plist"
cp Resources/DMSans.ttf "$APP/Contents/Resources/DMSans.ttf"

# app icon from the 256 px PNG
ICONSET="dist/AppIcon.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256; do
  sips -z $s $s Resources/icon.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
done
sips -z 32 32 Resources/icon.png --out "$ICONSET/icon_16x16@2x.png" >/dev/null
sips -z 64 64 Resources/icon.png --out "$ICONSET/icon_32x32@2x.png" >/dev/null
sips -z 256 256 Resources/icon.png --out "$ICONSET/icon_128x128@2x.png" >/dev/null
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "$ICONSET"

# Ad-hoc signature: enough to run on Apple silicon. It is not a Developer ID signature,
# so Gatekeeper still asks the first time (right-click the app, then Open).
codesign --force --deep --sign - "$APP"

(cd dist && ditto -c -k --keepParent Notch.app Notch-mac.zip)
echo "built $APP and dist/Notch-mac.zip"
lipo -archs "$APP/Contents/MacOS/Notch"
