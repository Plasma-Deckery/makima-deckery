#!/bin/bash
# install.sh — one-time setup + initial build for makima-deckery.
# For subsequent code changes, use redeploy.sh instead.
#
# Prerequisites (manual, one-time):
#   sudo usermod -aG input $USER   # grants access to /dev/input/* and /dev/uinput
#   (log out and back in for the group to take effect)

set -e
REPO="$(dirname "$(readlink -f "$0")")"
PACKAGES="rust"

echo "Repo: $REPO"

# ── 1. Distrobox container + packages ────────────────────────────────────────
distrobox assemble create --file "$REPO/distrobox.ini"
if ! distrobox enter deckery -- bash -c "\$HOME/.cargo/bin/cargo --version >/dev/null 2>&1"; then
    distrobox enter deckery -- sudo pacman -S --needed --noconfirm $PACKAGES
fi

# ── 2. Systemd user service (no sudo) ────────────────────────────────────────
SERVICE_DIR="$HOME/.config/systemd/user"
mkdir -p "$SERVICE_DIR"
GENERATED="$REPO/systemd/makima.service.template"
INSTALLED="$SERVICE_DIR/makima.service"
if ! diff -q "$GENERATED" "$INSTALLED" 2>/dev/null; then
    echo "Installing systemd user service..."
    cp "$GENERATED" "$INSTALLED"
    systemctl --user daemon-reload
fi
systemctl --user enable makima.service

# ── 3. Build + deploy ────────────────────────────────────────────────────────
bash "$REPO/redeploy.sh"
