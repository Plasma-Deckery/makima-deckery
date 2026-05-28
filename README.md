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
    "config_stack": ["Steam Deck", "Steam Deck::konsole"],
    "layout": 0,
    "paused": false
  },
  "bindings": {
    "BTN_SOUTH": { "action": ["KEY_ENTER"], "origin": "base" },
    "BTN_EAST":  { "action": ["KEY_C", "KEY_LEFTCTRL"], "origin": "app:konsole" }
  },
  "modifier_active": {
    "BTN_TL-BTN_GRIPR2": { "action": ["KEY_LEFTCTRL", "KEY_PAGEDOWN"], "origin": "base" }
  },
  "last_action": {
    "type": "keys",
    "value": ["KEY_ENTER"],
    "ts": 1748383200.123
  }
}
```

- **`bindings`** — all buttons with their current action, fully resolved (per-app overrides already merged)
- **`modifier_active`** — empty when no modifier is held; populated with the active combos while a modifier key is pressed
- **`origin`** — where each binding comes from: `"base"` or `"app:<classname>"`
- **`last_action`** — the most recent discrete user action with a Unix timestamp (for HUD fade-out)

---

### Pause / Resume IPC

makima-deckery exposes a Unix socket at `/tmp/makima.sock` for pause/resume control:

```bash
echo "pause"  | nc -U /tmp/makima.sock   # suspend all remapping
echo "resume" | nc -U /tmp/makima.sock   # re-enable remapping
```

When paused, all input passes through unmodified. The `paused` flag is reflected in `/tmp/makima-state.json`. Useful for temporarily bypassing remapping (e.g. when a game takes over input).

---

## Relationship to upstream

Bug fixes are submitted to upstream as PRs. Features specific to the Deckery HUD architecture (state export, IPC) are maintained here; an upstream proposal may follow once the HUD design stabilises.
