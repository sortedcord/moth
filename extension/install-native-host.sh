#!/usr/bin/env bash
# Register moth as the native-messaging host for the "Open in moth" extension in every
# Chromium-based browser that has a config directory on this machine.
#
# usage: extension/install-native-host.sh [path/to/moth]     (default: target/release/moth)
#        extension/install-native-host.sh --uninstall
set -euo pipefail

NAME="dev.moth.open"
EXTENSION_ID="mlnmgkmbmkjeelhpapeodnipmnbgaphj"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}"
BROWSERS=(
    "net.imput.helium"
    "google-chrome"
    "google-chrome-beta"
    "google-chrome-unstable"
    "chromium"
    "BraveSoftware/Brave-Browser"
    "vivaldi"
    "microsoft-edge"
)

if [[ "${1:-}" == "--uninstall" ]]; then
    for browser in "${BROWSERS[@]}"; do
        rm -fv "$CONFIG/$browser/NativeMessagingHosts/$NAME.json"
    done
    exit 0
fi

MOTH="$(realpath "${1:-$ROOT/target/release/moth}")"
if [[ ! -x "$MOTH" ]]; then
    echo "moth binary not found at $MOTH (build it with: cargo build --release)" >&2
    exit 1
fi

installed=0
for browser in "${BROWSERS[@]}"; do
    [[ -d "$CONFIG/$browser" ]] || continue
    dir="$CONFIG/$browser/NativeMessagingHosts"
    mkdir -p "$dir"
    cat > "$dir/$NAME.json" <<JSON
{
  "name": "$NAME",
  "description": "moth: open YouTube videos in the native client",
  "path": "$MOTH",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://$EXTENSION_ID/"]
}
JSON
    echo "registered for $browser: $dir/$NAME.json"
    installed=$((installed + 1))
done
if [[ $installed -eq 0 ]]; then
    echo "no Chromium-based browser config found under $CONFIG" >&2
    exit 1
fi
