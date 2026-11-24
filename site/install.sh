#!/bin/sh
# Installs Notch for macOS into ~/Applications and starts it.
#   curl -fsSL https://agentnotch-six.vercel.app/install.sh | sh
# Downloading this way means macOS does not quarantine the app, so it opens without the
# "unidentified developer" prompt. The source is at github.com/harshil12345000/AgentNotch.
set -e

if [ "$(uname)" != "Darwin" ]; then
  echo "This installer is for macOS. On Windows, download notch.exe from the website." >&2
  exit 1
fi

URL="https://github.com/harshil12345000/AgentNotch/releases/latest/download/Notch-mac.zip"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "Downloading Notch..."
curl -fsSL "$URL" -o "$TMP/Notch-mac.zip"

mkdir -p "$HOME/Applications"
pkill -x Notch 2>/dev/null || true
rm -rf "$HOME/Applications/Notch.app"
ditto -x -k "$TMP/Notch-mac.zip" "$TMP/x"
ditto "$TMP/x/Notch.app" "$HOME/Applications/Notch.app"
xattr -dr com.apple.quarantine "$HOME/Applications/Notch.app" 2>/dev/null || true

open "$HOME/Applications/Notch.app"
echo "Notch is installed in ~/Applications and running at the top of your screen."
