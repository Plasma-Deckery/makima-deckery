#!/bin/bash
# install.sh — one-time setup + initial build for makima-deckery.
# For subsequent code changes, use redeploy.sh instead.
#
# Prerequisites (manual, one-time):
#   sudo usermod -aG input $USER   # grants access to /dev/input/* and /dev/uinput
#   (log out and back in for the group to take effect)

set -e
REPO="$(dirname "$(readlink -f "$0")")"
BUILD_PACKAGES="rust pkgconf gcc systemd-libs rpm-tools make"

echo "Repo: $REPO"

# ── 1. Distrobox container + packages ────────────────────────────────────────
distrobox create --name deckery --image archlinux:latest || true
distrobox enter deckery -- sudo pacman -S --needed --noconfirm $BUILD_PACKAGES

# ── 2. Systemd user services (no sudo) ───────────────────────────────────────
SERVICE_DIR="$HOME/.config/systemd/user"
mkdir -p "$SERVICE_DIR"
for svc in makima.service; do
    GENERATED="$REPO/systemd/$svc"
    INSTALLED="$SERVICE_DIR/$svc"
    if ! diff -q "$GENERATED" "$INSTALLED" 2>/dev/null; then
        echo "Installing systemd user service: $svc"
        cp "$GENERATED" "$INSTALLED"
    fi
done
systemctl --user daemon-reload
systemctl --user enable makima.service

# Remove the old external resume-watcher (script + unit) from any prior
# install: makima now watches logind's PrepareForSleep in-process (see
# resume_watcher.rs), so the external systemctl-restart-on-resume mechanism
# is obsolete. Leaving it installed would double-trigger reinit on resume
# alongside the in-process watcher.
OLD_UNIT="$SERVICE_DIR/makima-resume-watcher.service"
if [ -f "$OLD_UNIT" ]; then
    echo "Removing obsolete makima-resume-watcher.service..."
    systemctl --user disable --now makima-resume-watcher.service 2>/dev/null || true
    rm -f "$OLD_UNIT"
    systemctl --user daemon-reload
fi
OLD_SCRIPT="$HOME/.local/bin/makima-resume-watcher"
if [ -f "$OLD_SCRIPT" ]; then
    echo "Removing obsolete makima-resume-watcher script..."
    rm -f "$OLD_SCRIPT"
fi
OLD_BIN="$HOME/.local/bin/makima"
if [ -f "$OLD_BIN" ]; then
    echo "Removing obsolete makima binary (renamed to makima-deckery)..."
    rm -f "$OLD_BIN"
fi

# ── 3. Build + deploy ────────────────────────────────────────────────────────
bash "$REPO/redeploy.sh"
