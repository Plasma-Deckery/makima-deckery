#!/bin/bash
# redeploy.sh — build and restart makima after code changes.
# No sudo required.

set -e
REPO="$(dirname "$(readlink -f "$0")")"

echo "Building makima..."
distrobox enter deckery -- bash -c "cd '$REPO' && cargo build --release"

systemctl --user stop makima.service
mkdir -p "$HOME/.local/bin"
cp "$REPO/target/release/makima" "$HOME/.local/bin/makima"
echo "Installed binary to ~/.local/bin/makima"

systemctl --user start makima.service
echo "Done — makima is running."
