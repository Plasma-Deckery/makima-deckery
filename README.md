# Makima Deckery

> Deckery-specific fork of [cyber-sushi/makima](https://github.com/cyber-sushi/makima).

The heart of Deckery — the input remapper. Reads raw evdev events directly from the kernel, applies a TOML config, and emits keyboard/mouse events via uinput. Part of [Plasma Deckery](https://github.com/Plasma-Deckery/deckery).

---

## What's different from upstream

- **Bug fixes** — D-Pad remapping, x11rb Wayland crash, evdev reconnect on device error (all submitted as upstream PRs)
- **Event-driven window focus** — KWin D-Bus script replaces `kdotool` subprocess spawning; no polling, no latency
- **Per-app configs with inheritance** — app overrides only declare what differs; base config is merged at runtime
- **Binding attributes** — `label` and `no_pause` per binding
- **State export** → `/tmp/makima-state.json` — live button map for the HUD overlay
- **Trackpad MT translation** — both trackpads emulated as standard system touchpad devices
- **Pause / Resume IPC** — runtime control via Unix socket at `/tmp/makima-control.sock`
- **Steam Deck keycodes** — `BTN_GRIPL/R/L2/R2` for the back paddles via patched `evdev` crate
- **Unit test suite** — 69 tests covering resolver, state export, analog helpers, and config parsing

→ [Full documentation](https://plasma-deckery.github.io/deckery/projects/makima-deckery/)

---

## Installation

Makima Deckery is installed and managed as part of the Deckery suite — no separate setup needed.

→ [Deckery Setup Guide](https://plasma-deckery.github.io/deckery/setup-guide/)
