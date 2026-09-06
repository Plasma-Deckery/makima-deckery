#!/bin/bash
# install.sh — one-time setup + initial build for makima-deckery.
# For subsequent code changes, use redeploy.sh instead.

set -e
REPO="$(dirname "$(readlink -f "$0")")"
BUILD_PACKAGES="rust pkgconf gcc systemd-libs rpm-tools make copr-cli"

echo "Repo: $REPO"

# ── 1. udev rule ─────────────────────────────────────────────────────────────
#
# Grants the active login session access to /dev/uinput via TAG+="uaccess"
# (logind-scoped, narrower than adding the user to the input group).
# Idempotent: sudo is only called when the installed rule differs from the
# source, so updates never prompt for a password.

_UDEV_SRC="$REPO/udev/50-makima.rules"
_UDEV_DST="/etc/udev/rules.d/50-makima.rules"

if ! diff -q "$_UDEV_SRC" "$_UDEV_DST" 2>/dev/null; then
    echo "Installing udev rule (requires sudo once)..."
    sudo install -Dm644 "$_UDEV_SRC" "$_UDEV_DST"
    sudo modprobe uinput 2>/dev/null || true
    sudo udevadm control --reload-rules
    sudo udevadm trigger --subsystem-match=misc
    sudo udevadm settle
else
    echo "udev rule already up to date — no sudo needed"
fi

# ── 2. Distrobox container + packages ────────────────────────────────────────
distrobox create --name deckery --image archlinux:latest || true
distrobox enter deckery -- sudo pacman -S --needed --noconfirm $BUILD_PACKAGES

# ── 3. Systemd user services (no sudo) ───────────────────────────────────────
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

# ── 4. qdbus compatibility shim ──────────────────────────────────────────────
#
# CachyOS and other Qt6-only distros ship qdbus6 but not qdbus. The deckery
# configs use `run = ["qdbus ..."]` for KDE actions (nextDesktop, invokeShortcut
# etc.). Create a user-level symlink so the existing configs work unchanged.
_QDBUS_BIN="$HOME/.local/bin/qdbus"
if ! command -v qdbus &>/dev/null && command -v qdbus6 &>/dev/null; then
    echo "qdbus not found but qdbus6 is available — creating compatibility symlink"
    mkdir -p "$HOME/.local/bin"
    ln -sf "$(command -v qdbus6)" "$_QDBUS_BIN"
fi

# ── 5. Build + deploy ────────────────────────────────────────────────────────
bash "$REPO/redeploy.sh"
