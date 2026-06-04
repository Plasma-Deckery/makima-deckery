# makima-deckery

> **Deckery-specific fork of [cyber-sushi/makima](https://github.com/cyber-sushi/makima).**
> For installation, configuration and general usage see the [upstream README](https://github.com/cyber-sushi/makima#readme).

This fork is maintained as part of the [Plasma Deckery](https://github.com/Plasma-Deckery) project — a Steam-independent input stack for the Steam Deck in desktop mode. The scope of makima is extended here to meet the requirements of a handheld device: hardware-specific input translation, live UI integration, and analog sensor export.

---

## What's different from upstream

Quick overview — details in the sections below:

- **Bug fixes** — D-Pad remapping, x11rb Wayland crash, evdev reconnect on device error (all submitted as upstream PRs)
- **kdotool replaced** — window focus is detected event-driven via a KWin D-Bus script instead of spawning a subprocess on every button press; significantly more efficient
- **Per-app configs with inheritance** — app overrides only declare what differs; everything else is merged from the base config at runtime
- **Binding attributes** — extended configuration options per binding: human-readable display names and execution control flags
- **State export** → `/tmp/makima-state.json` — all state needed for a real-time button preview HUD: active bindings, modifier context, currently held buttons, last executed action, and analog sensor values (sticks, trackpads, IMU)
- **Trackpad MT translation** — both trackpads are emulated as standard system touchpad devices, activating existing trackpad gesture recognition tools on the left and right pad
- **Lizard Mode suppression** — periodic hidraw heartbeat keeps the `hid-steam` kernel driver's built-in mouse/scroll fallback disabled without Steam running; configurable via `SUPPRESS_LIZARD_MODE`
- **Pause / Resume IPC** — the service can be paused and resumed at any time via a Unix socket; useful for HUD dry-run mode where the overlay previews bindings without any remapping taking effect
- **Steam Deck keycodes** — `BTN_GRIPL`, `BTN_GRIPR`, `BTN_GRIPL2`, `BTN_GRIPR2` for the back paddles (patched `evdev` crate)
- **Unit test suite** — 69 tests covering resolver, state export, analog helpers, and config parsing

---

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

### Trackpad emulation as system touchpads

The Steam Deck trackpads are capable input surfaces, but the raw `ABS_HAT` events they produce are invisible to gesture tools — they expect standard Linux multi-touch devices. By translating the trackpad data into proper MT events and exposing virtual uinput devices, makima makes both pads visible to tools like `libinput-gestures` or `fusuma`. This is the prerequisite for defining custom gestures per pad (swipe zones, tap areas, circular scroll) without having to implement gesture recognition inside makima itself.

With `LPAD = "trackpad"` or `RPAD = "trackpad"` in the config, makima translates the raw Steam Deck trackpad axes into proper Linux multi-touch events and exposes them as standard uinput touchpad devices — `Deckery Left Trackpad` and `Deckery Right Trackpad`.

The Steam Deck kernel driver (`hid-steam`) delivers trackpad data as absolute axes on the gamepad device:

```
ABS_HAT0X / ABS_HAT0Y  →  left trackpad position  (−32767 … +32767)
ABS_HAT1X / ABS_HAT1Y  →  right trackpad position
BTN_THUMB              →  left trackpad physical click
BTN_THUMB2             →  right trackpad physical click
```

makima translates these to `ABS_MT_POSITION_X/Y` + `BTN_TOUCH` + `BTN_TOOL_FINGER` frames on the virtual device, with Y-axis corrected to libinput convention (hardware reports up as negative; the virtual device flips this). Once the virtual device exists, gesture tools like `libinput-gestures` or `fusuma` can read it and map swipes, taps, and zones to arbitrary actions — independently configurable per pad.

```toml
[settings]
LPAD = "trackpad"   # creates "Deckery Left Trackpad" virtual MT device
RPAD = "trackpad"   # creates "Deckery Right Trackpad" virtual MT device
# LPAD = "disabled" # default — no virtual device, but position is still tracked in state.json
```

Trackpad position, touch state, and press state are always tracked and exported to `state.json` regardless of the mode setting — the HUD can visualize trackpad input even when `"disabled"`.

---

### Lizard Mode suppression

The `hid-steam` kernel driver keeps a built-in mouse/scroll fallback ("Lizard Mode") active unless a userspace client suppresses it by sending HID feature reports periodically. Steam handles this while it is running — without it, the trackpads emit mouse events directly via the kernel driver, bypassing makima entirely.

Makima-deckery takes over this role via a configurable hidraw heartbeat. On startup, it opens the raw controller hidraw device (the `.0005` interface, not the emulated keyboard/mouse nodes) and sends suppression reports every 4 s. The heartbeat is a built-in safety mechanism: if makima crashes or exits, the file descriptor is closed, and Lizard Mode re-activates automatically within ~8 s.

Control which aspects are suppressed via `SUPPRESS_LIZARD_MODE` in the `[settings]` section of your base config:

```toml
[settings]
SUPPRESS_LIZARD_MODE = "buttons,mouse"   # suppress both (recommended)
SUPPRESS_LIZARD_MODE = "buttons"          # only clear keyboard/button mappings
SUPPRESS_LIZARD_MODE = "mouse"            # only disable trackpad mouse/scroll emulation
SUPPRESS_LIZARD_MODE = "false"            # disabled (default when setting is absent)
```

| Value | Effect |
|---|---|
| `"buttons"` | Sends `ID_CLEAR_DIGITAL_MAPPINGS` (0x81) — prevents the kernel driver from emitting arrow keys, Enter, Esc via d-pad and face buttons |
| `"mouse"` | Sends `ID_SET_SETTINGS_VALUES` (0x87) with `TRACKPAD_NONE` — prevents the kernel driver from emitting mouse and scroll events from the trackpads |
| `"buttons,mouse"` | Both of the above (recommended for full Steam independence) |
| `"false"` / absent | Disabled — Lizard Mode is not suppressed |

When the setting is absent, Lizard Mode is **not** suppressed. Makima gracefully skips the heartbeat task on non-Steam-Deck hardware (no Valve hidraw device found).

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

makima-deckery exposes a Unix socket at `/tmp/makima-control.sock` for runtime control:

```bash
echo "pause"               | socat - UNIX-CONNECT:/tmp/makima-control.sock
echo "resume"              | socat - UNIX-CONNECT:/tmp/makima-control.sock
echo "analog-state-export on"  | socat - UNIX-CONNECT:/tmp/makima-control.sock
echo "analog-state-export off" | socat - UNIX-CONNECT:/tmp/makima-control.sock
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
