# makima-deckery

> **Deckery-specific fork of [cyber-sushi/makima](https://github.com/cyber-sushi/makima).**
> For installation, configuration and general usage see the [upstream README](https://github.com/cyber-sushi/makima#readme).

This fork is maintained as part of the [Plasma Deckery](https://github.com/Plasma-Deckery) project — a Steam-independent input stack for the Steam Deck in desktop mode.

---

## What's different from upstream

### Bug fixes (submitted as upstream PRs)

| Fix | PR | Why |
|---|---|---|
| `BTN_DPAD_*` keys silently ignored in config | [#57](https://github.com/cyber-sushi/makima/pull/57) | D-Pad buttons were classified as axes, making them impossible to remap |
| `x11rb::connect()` panic on Wayland after suspend | [#58](https://github.com/cyber-sushi/makima/pull/58) | Caused the worker thread to die silently; service appeared active but processed no events |
| Evdev fd reconnect on device read error | — | When the evdev stream returns an I/O error (e.g. USB hotplug), makima now reinitializes automatically via `tokio::select!` instead of silently stopping |

---

### State export → `/tmp/makima-state.json`

On every config or modifier change, makima writes a fully-resolved state snapshot to `/tmp/makima-state.json`. This allows the Deckery HUD overlay to display live button mappings without re-implementing any of makima's lookup logic.

```json
{
  "context": {
    "config_stack": ["Steam Deck"],
    "layout": 0,
    "paused": false,
    "held_modifiers": ["BTN_TL"],
    "active_buttons": ["BTN_TL", "BTN_EAST"],
    "active_outputs": ["KEY_C", "KEY_LEFTCTRL"]
  },
  "bindings": {
    "BTN_SOUTH": { "action": ["KEY_ENTER"], "origin": "Steam Deck" },
    "BTN_TL-BTN_EAST": { "action": ["KEY_C", "KEY_LEFTCTRL"], "origin": "Steam Deck" }
  },
  "modifier_active": {
    "BTN_EAST": { "action": ["KEY_C", "KEY_LEFTCTRL"], "origin": "Steam Deck" }
  },
  "last_action": {
    "type": "keys",
    "value": ["KEY_ENTER"],
    "ts": 1748383200.123
  }
}
```

- **`bindings`** — all remaps from the active config; plain buttons as `"BTN_FOO"`, combos as `"MOD-BTN_FOO"`
- **`modifier_active`** — empty when no modifier held; while a modifier is pressed, contains every trigger reachable via that combo, keyed by trigger button
- **`held_modifiers`** — modifier buttons currently physically held
- **`active_buttons`** — all input buttons currently held (for button highlighting in the HUD)
- **`active_outputs`** — evdev keys currently being emitted (derived from held buttons + modifier context)
- **`config_stack`** — inheritance chain for the active config; currently always one element (`[config.name]`); will grow once the planned `EXTENDS` feature lands
- **`last_action`** — the most recent discrete user action with a Unix timestamp (for HUD fade-out)

---

### Pause / Resume IPC

makima-deckery exposes a Unix socket at `/tmp/makima.sock` for pause/resume control:

```bash
echo "pause"  | nc -U /tmp/makima.sock   # suspend all remapping
echo "resume" | nc -U /tmp/makima.sock   # re-enable remapping
```

When paused, all input passes through unmodified. The `paused` flag is reflected in `/tmp/makima-state.json`. The primary use case is **HUD dry-run mode**: the overlay can show the full binding map without any remapping actually taking effect — useful for exploring layouts without triggering actions.

---

## Setup

### Systemd services

Two user services are required. Both are tracked in [steamdeck-dotfiles](https://github.com/Plasma-Deckery/steamdeck-dotfiles).

**`makima.service`** runs the remapper. Environment variables must be hardcoded because the service starts before the desktop session environment is fully inherited:

```ini
[Service]
ExecStart=/home/<user>/.local/bin/makima
Environment=WAYLAND_DISPLAY=wayland-0
Environment=DISPLAY=:0
Environment=XDG_SESSION_TYPE=wayland
Environment=XDG_CURRENT_DESKTOP=KDE
```

**`makima-resume-watcher.service`** watches for the `PrepareForSleep(false)` DBus signal and restarts makima after suspend. This is required because the Steam Deck kernel silently freezes evdev file descriptors on suspend without returning an error — makima cannot detect this on its own and will stop processing input until restarted.

```bash
systemctl --user enable --now makima.service
systemctl --user enable --now makima-resume-watcher.service
```

### Building

makima-deckery depends on a patched `evdev` crate that adds `BTN_GRIPL/R/L2/R2` keycodes for the Steam Deck's back paddles. The upstream binary does not include this.

The build runs inside a [distrobox](https://github.com/containers/distrobox) container (`deckery`) with the Rust toolchain and patched dependencies pre-installed. See [steamdeck-dotfiles](https://github.com/Plasma-Deckery/steamdeck-dotfiles) for container setup.

---

## Relationship to upstream

Bug fixes are submitted to upstream as PRs. Features specific to the Deckery HUD architecture (state export, IPC) are maintained here; an upstream proposal may follow once the HUD design stabilises.
