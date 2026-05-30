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

### Per-app configs with event-driven window focus

App-specific config files (`{device}::{window-class}.toml`) only need to declare bindings that differ from the base config — everything else is inherited at runtime via `merge_base()`. No duplication required.

Window focus changes are detected event-driven via a KWin D-Bus script (`kwin_watcher`), which registers `org.makima.watcher` on the session bus and receives a callback from `workspace.windowActivated` on every focus change. This replaces the previous approach of spawning a `kdotool` subprocess on every button press to query the active window — eliminating a significant source of CPU load and latency. No polling, no subprocess spawning, no external tool dependency.

Config files only need to contain their overrides:

```toml
# Steam Deck::org.mozilla.firefox.toml
[remap]
BTN_TL-BTN_DPAD_LEFT  = ["KEY_LEFTALT", "KEY_LEFT"]   # L1+← → Back
BTN_TL-BTN_DPAD_RIGHT = ["KEY_LEFTALT", "KEY_RIGHT"]  # L1+→ → Forward
BTN_TL-BTN_DPAD_UP    = ["KEY_LEFTCTRL", "KEY_R"]     # L1+↑ → Reload

[settings]
CUSTOM_MODIFIERS = "BTN_TL-BTN_MODE"
GRAB_DEVICE = "false"
```

---

### State export → `/tmp/makima-state.json`

On every config or modifier change, makima writes a fully-resolved state snapshot to `/tmp/makima-state.json`. This allows the Deckery HUD overlay to display live button mappings without re-implementing any of makima's lookup logic.

```json
{
  "context": {
    "config_stack": ["Steam Deck", "org.mozilla.firefox"],
    "layout": 0,
    "paused": false,
    "held_modifiers": ["BTN_TL"],
    "active_buttons": ["BTN_TL", "BTN_DPAD_LEFT"],
    "active_outputs": ["KEY_LEFT", "KEY_LEFTALT"]
  },
  "bindings": {
    "BTN_SOUTH":          { "action": ["KEY_ENTER"],                    "kind": "remap",   "label": null,       "origin": "Steam Deck" },
    "BTN_TL-BTN_DPAD_LEFT": { "action": ["KEY_LEFTALT", "KEY_LEFT"],   "kind": "remap",   "label": null,       "origin": "org.mozilla.firefox" },
    "BTN_THUMBL":         { "action": ["deckery-hud-toggle"],           "kind": "command", "label": "Toggle HUD", "no_pause": true, "origin": "Steam Deck" }
  },
  "modifier_active": {
    "BTN_DPAD_LEFT": { "action": ["KEY_LEFTALT", "KEY_LEFT"], "kind": "remap",   "label": null,            "origin": "org.mozilla.firefox" },
    "BTN_DPAD_UP":   { "action": ["Previous Desktop"],        "kind": "command", "label": "Previous Desktop", "origin": "Steam Deck" }
  },
  "last_action": {
    "type": "command",
    "value": "deckery-hud-toggle",
    "ts": 1748383200.123,
    "label": "Toggle HUD"
  }
}
```

- **`bindings`** — all remaps and commands from the active config; plain buttons as `"BTN_FOO"`, combos as `"MOD1-MOD2-BTN_FOO"`
- **`modifier_active`** — empty when no modifier held; when a modifier is pressed, contains every trigger reachable via the **exact** current modifier set, keyed by trigger button; uses exact matching so it never suggests a binding that would fall through to a less-specific combo at runtime
- **`held_modifiers`** — modifier buttons currently physically held
- **`active_buttons`** — all input buttons currently held (for button highlighting in the HUD)
- **`active_outputs`** — evdev keys currently being emitted (derived from held buttons + modifier context)
- **`config_stack`** — active config layer chain: one entry (`["Steam Deck"]`) for the base config, two entries (`["Steam Deck", "org.mozilla.firefox"]`) when an app-specific config is active; the second entry is the window class without the base prefix
- **`origin`** — which config layer a binding comes from: the base config name for inherited bindings, the app-specific part for overrides; lets the HUD visually distinguish base bindings from per-app additions
- **`label`** — optional human-readable display name set via `label = "…"` in the config; present on `bindings`, `modifier_active`, and `last_action` entries
- **`kind`** — binding type: `"remap"`, `"command"`, or `"movement"`
- **`no_pause`** — `true` if the binding bypasses the global pause state (command bindings only)
- **`last_action`** — the most recent discrete user action with a Unix timestamp (for HUD fade-out); carries `label` if set on the binding

### Binding attributes

Bindings support an extended inline-table syntax alongside the simple array form:

```toml
[remap]
BTN_TL-BTN_NORTH = { keys = ["KEY_LEFTCTRL", "KEY_C"], label = "Copy (Ctrl+C)" }

[commands]
BTN_THUMBL = { run = ["deckery-hud-toggle"], no_pause = true, label = "Toggle HUD" }
```

| Attribute | Type | Applies to | Description |
|---|---|---|---|
| `keys` / `run` | array | remap / command | The action (replaces the bare array) |
| `label` | string | both | Human-readable name exported to the HUD |
| `no_pause` | bool | command | Execute even when makima is paused |

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
