# Makima Deckery

> Deckery-specific fork of [cyber-sushi/makima](https://github.com/cyber-sushi/makima).

The heart of Deckery — the input remapper. Reads raw evdev events directly from the kernel, applies a TOML config, and emits keyboard/mouse events via uinput. Part of [Plasma Deckery](https://github.com/Plasma-Deckery/deckery).

---

## Setup

Makima Deckery is installed and managed as part of the Deckery suite — no separate setup needed.

→ [Deckery Setup Guide](https://plasma-deckery.github.io/deckery/setup-guide/)

---

## Development

All development happens inside the `deckery` distrobox container (created by `install.sh`, shared with deckery-hud). After making source changes:

```bash
bash redeploy.sh          # build + restart service in one step
```

Running tests and building manually inside the container:

```bash
distrobox enter deckery
cargo build --release
cargo test
```

Or in one line without entering the container:

```bash
distrobox enter deckery -- cargo test
```

---

## What's different from upstream

- **Bug fixes** — D-Pad remapping, x11rb Wayland crash, evdev reconnect on device error (all submitted as upstream PRs)
- **Event-driven window focus** — KWin D-Bus script replaces `kdotool` subprocess spawning; no polling, no latency
- **Per-app configs with inheritance** — app overrides only declare what differs; base config is merged at runtime
- **Binding attributes** — `label` and `no_pause` per binding
- **State export** → `/tmp/makima-state.json` — all state needed for a real-time button preview HUD: active bindings, modifier context, currently held buttons, last executed action, and analog sensor values (sticks, trackpads, IMU)
- **Trackpad MT translation** — both trackpads emulated as standard system touchpad devices, activating existing trackpad gesture recognition tools
- **Lizard Mode suppression** — periodic hidraw heartbeat keeps the `hid-steam` kernel driver's built-in mouse/scroll fallback disabled without Steam running; configurable via `SUPPRESS_LIZARD_MODE`
- **Pause / Resume IPC** — runtime control via Unix socket at `/tmp/makima-control.sock`
- **Steam Deck keycodes** — `BTN_GRIPL/R/L2/R2` for the back paddles via patched `evdev` crate
- **Unit test suite** — 150 tests covering resolver, state export, analog helpers, config parsing, trackpad routing, and haptic report encoding

→ [Full documentation](https://plasma-deckery.github.io/deckery/projects/makima-deckery/)

---

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

The Steam Deck trackpads are capable input surfaces, but Steam Input's default handling is invisible to gesture tools — they expect standard Linux multi-touch devices. By reading the trackpads' raw hidraw reports and translating them into proper MT events on virtual uinput devices, makima makes both pads (and, optionally, a combined two-finger gesture surface) visible to tools like `libinput-gestures` or `fusuma`. This is the prerequisite for defining custom gestures per pad (swipe zones, tap areas, circular scroll, pinch-zoom) without having to implement gesture recognition inside makima itself.

Trackpad handling is split across three layers, each independently testable and swappable:

```
pad_hidraw.rs        — raw producer: parses 64-byte hidraw reports from the
                        Steam Deck controller into PadFrame{x, y, touching, click}
                        per pad. No knowledge of modes or gestures.
trackpad_router.rs   — Core routing: always mirrors raw position/touch/click into
                        state.json regardless of mode, and decides which channel
                        is active — left individual, right individual, or (if
                        enabled) the combined two-finger gesture channel once both
                        pads are touching simultaneously.
mt_trackpad.rs        — handler(s): turn one channel's frame stream into an
                        emulated MT device, including click-edge detection and
                        haptic policy. A future trackball or multi-zone handler
                        would be a sibling module here, without touching the
                        two layers above.
```

With `mode = "mt-trackpad"` under `[trackpad.left]`/`[trackpad.right]` in the config, makima exposes each pad as its own standard uinput touchpad device — `Deckery Left Trackpad` / `Deckery Right Trackpad`. Setting `combined_gesture_device = true` additionally exposes a third two-slot MT device, `Deckery Combined Trackpad`, active only while both pads are touching at once (e.g. for pinch-zoom) — individual pads seamlessly resume their own device the instant one finger lifts.

```toml
[trackpad.left]
mode = "mt-trackpad"   # creates "Deckery Left Trackpad" virtual MT device

[trackpad.right]
mode = "mt-trackpad"   # creates "Deckery Right Trackpad" virtual MT device

[trackpad]
combined_gesture_device = true   # also creates "Deckery Combined Trackpad" for two-finger gestures
# mode = "disabled" # default per pad — no virtual device, but position is still tracked in state.json
```

The legacy `[settings] LPAD = "trackpad"` / `RPAD = "trackpad"` syntax is still accepted as a fallback for `mode = "mt-trackpad"`.

Position is Y-corrected to libinput convention (hardware reports up as negative; the virtual device flips this) and split into left/right halves of a shared X axis on the combined device so a pinch gesture tracks correctly across both slots. Trackpad position, touch state, and press state are always tracked and exported to `state.json` regardless of the mode setting — the HUD can visualize trackpad input even when `"disabled"`.

Haptic feedback is configurable per pad: press and release edges are independent events with separate pulse shapes, and distance-gated movement haptics are also supported. See the [trackpad configuration docs](https://plasma-deckery.github.io/deckery/projects/makima-deckery/trackpad/) for the full config reference.

> **Tip:** if you enable `combined_gesture_device` and use quick two-hand gestures (e.g. pinch-zoom), disable "Tap to click" on the individual `Deckery Left/Right Trackpad` devices in your desktop's touchpad settings. Touching down with one pad slightly before the other briefly routes through that pad's individual channel before gesture mode activates; the router's forced clean-lift on gesture entry looks like a fast tap-and-release to libinput, which tap-to-click would otherwise turn into a spurious click.

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

→ [Full state.json reference](https://plasma-deckery.github.io/deckery/reference/state-json/)

---

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

→ [Full bindings reference](https://plasma-deckery.github.io/deckery/projects/makima-deckery/bindings/)

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
