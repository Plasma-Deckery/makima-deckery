#!/bin/bash
# install.sh — one-time setup + initial build for makima-deckery.
# For subsequent code changes, use redeploy.sh instead.
#
# Prerequisites (manual, one-time):
#   sudo usermod -aG input $USER   # grants access to /dev/input/* and /dev/uinput
#   (log out and back in for the group to take effect)

set -e
REPO="$(dirname "$(readlink -f "$0")")"
BUILD_PACKAGES="rust pkgconf gcc systemd-libs"

echo "Repo: $REPO"

# ── 1. Distrobox container + packages ────────────────────────────────────────
distrobox create --name deckery --image archlinux:latest || true
distrobox enter deckery -- sudo pacman -S --needed --noconfirm $BUILD_PACKAGES

# ── 2. Systemd user services (no sudo) ───────────────────────────────────────
SERVICE_DIR="$HOME/.config/systemd/user"
mkdir -p "$SERVICE_DIR"
for svc in makima.service makima-resume-watcher.service; do
    GENERATED="$REPO/systemd/$svc"
    INSTALLED="$SERVICE_DIR/$svc"
    if ! diff -q "$GENERATED" "$INSTALLED" 2>/dev/null; then
        echo "Installing systemd user service: $svc"
        cp "$GENERATED" "$INSTALLED"
    fi
done
systemctl --user daemon-reload
systemctl --user enable makima.service
systemctl --user enable makima-resume-watcher.service

# Install resume-watcher script
BIN_DIR="$HOME/.local/bin"
mkdir -p "$BIN_DIR"
cp "$REPO/scripts/makima-resume-watcher" "$BIN_DIR/makima-resume-watcher"
chmod +x "$BIN_DIR/makima-resume-watcher"

# ── 3. Build + deploy ────────────────────────────────────────────────────────
bash "$REPO/redeploy.sh"
