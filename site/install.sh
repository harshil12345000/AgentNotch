#!/bin/sh
# Installs AgentNotch for macOS into ~/Applications and starts it.
#   curl -fsSL https://agentnotch-six.vercel.app/install.sh | sh
# Downloading this way means macOS does not quarantine the app, so it opens without the
# "unidentified developer" prompt. The source is at github.com/harshil12345000/AgentNotch.
set -e

if [ "$(uname)" != "Darwin" ]; then
  echo "This installer is for macOS. On Windows, download AgentNotch.exe from the website." >&2
  exit 1
fi

URL="https://github.com/harshil12345000/AgentNotch/releases/latest/download/AgentNotch-mac.zip"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "Downloading AgentNotch..."
curl -fsSL "$URL" -o "$TMP/AgentNotch-mac.zip"

mkdir -p "$HOME/Applications"
pkill -x AgentNotch 2>/dev/null || true
pkill -x Notch 2>/dev/null || true
rm -rf "$HOME/Applications/AgentNotch.app" "$HOME/Applications/Notch.app"
ditto -x -k "$TMP/AgentNotch-mac.zip" "$TMP/x"
ditto "$TMP/x/AgentNotch.app" "$HOME/Applications/AgentNotch.app"
xattr -dr com.apple.quarantine "$HOME/Applications/AgentNotch.app" 2>/dev/null || true

open "$HOME/Applications/AgentNotch.app"
echo "AgentNotch is installed in ~/Applications and running at the top of your screen."
