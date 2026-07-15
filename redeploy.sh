#!/bin/bash
# redeploy.sh — build and restart makima-deckery after code changes.
# No sudo required.

set -e
REPO="$(dirname "$(readlink -f "$0")")"

echo "Building makima-deckery..."
distrobox enter deckery -- bash -c "cd '$REPO' && cargo build --release"

systemctl --user stop makima.service
mkdir -p "$HOME/.local/bin"
cp "$REPO/target/release/makima-deckery" "$HOME/.local/bin/makima-deckery"
echo "Installed binary to ~/.local/bin/makima-deckery"

systemctl --user start makima.service
echo "Done — makima-deckery is running."
