# Makima Deckery

> Deckery-specific fork of [cyber-sushi/makima](https://github.com/cyber-sushi/makima).

The heart of Deckery — the input remapper. Reads raw evdev events directly from the kernel, applies a TOML config, and emits keyboard/mouse events via uinput. Part of [Plasma Deckery](https://github.com/Plasma-Deckery/deckery).

---

## Setup

Makima Deckery is installed and managed as part of the Deckery suite.

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
- **Gaming Mode** — Steam games are auto-detected on window focus and activate Gaming Mode, disabling all remapping so Deckery and in-game input don't collide
- **Haptic feedback** — configurable haptic pulses on the Steam Deck's trackpad actuators, triggered on button events, trackpad touch/click, and Gaming Mode transitions
- **State export** → `/tmp/makima-state.json` — all state needed for a real-time button preview HUD: active bindings, modifier context, currently held buttons, last executed action, and analog sensor values (sticks, trackpads, IMU)
- **Trackpad MT translation** — both trackpads emulated as standard system touchpad devices, activating existing trackpad gesture recognition tools
- **Lizard Mode suppression** — periodic hidraw heartbeat keeps the `hid-steam` kernel driver's built-in mouse/scroll fallback disabled without Steam running; configurable via `SUPPRESS_LIZARD_MODE`
- **Pause / Resume IPC** — runtime control via Unix socket at `/tmp/makima-control.sock`
- **Steam Deck keycodes** — `BTN_GRIPL/R/L2/R2` for the back paddles via patched `evdev` crate
- **Unit test suite** — 150 tests covering resolver, state export, analog helpers, config parsing, trackpad routing, and haptic report encoding

→ [Full documentation](https://plasma-deckery.github.io/deckery/projects/makima-deckery/)

---

### Binding attributes

Bindings support an extended inline-table syntax alongside the simple array form:

```toml
[remap]
BTN_TL-BTN_NORTH = { keys = ["KEY_LEFTCTRL", "KEY_C"], label = "Copy (Ctrl+C)" }

[commands]
BTN_THUMBL = { run = ["deckery-hud-toggle"], no_pause = true, label = "Toggle HUD" }
```

→ [Full bindings reference](https://plasma-deckery.github.io/deckery/projects/makima-deckery/bindings/)

---

### Application-aware configuration

On window focus change, makima automatically loads the matching per-app config. App overrides only declare what differs from the base config — everything else is inherited.

```toml
# Steam Deck::org.mozilla.firefox.toml
[remap]
BTN_TL-BTN_DPAD_LEFT  = ["KEY_LEFTALT", "KEY_LEFT"]   # L1+← → Back
BTN_TL-BTN_DPAD_RIGHT = ["KEY_LEFTALT", "KEY_RIGHT"]  # L1+→ → Forward
BTN_TL-BTN_DPAD_UP    = ["KEY_LEFTCTRL", "KEY_R"]     # L1+↑ → Reload

[settings]
CUSTOM_MODIFIERS = "BTN_TL-BTN_MODE"
```

---

### Trackpad emulation as system touchpads

Steam Deck trackpads are similar to laptop trackpads — even including haptic feedback coils. As part of the Steam Deck controller, they are invisible to the standard Linux input stack. Normally Steam Input is required to read from them and emulate mouse movements. Deckery reads the raw data streams and emulates both pads as standard Linux multitouch devices.

→ [Trackpad architecture reference](https://plasma-deckery.github.io/deckery/reference/trackpad-architecture/)

```toml
[trackpad.left]
mode = "mt-trackpad"   # creates "Deckery Left Trackpad" virtual MT device

[trackpad.right]
mode = "mt-trackpad"   # creates "Deckery Right Trackpad" virtual MT device

[trackpad]
combined_gesture_device = true   # also creates "Deckery Combined Trackpad" for two-finger gestures
# mode = "disabled" # default per pad — no virtual device, but position is still tracked in state.json
```

Setting `combined_gesture_device = true` enables both trackpads simultaneously for pinch-zoom and scroll — individual pads seamlessly resume their own device the instant one finger lifts.

Haptic feedback is configurable per pad: press and release edges are independent events with separate pulse shapes, and distance-gated movement haptics are also supported. See the [trackpad configuration docs](https://plasma-deckery.github.io/deckery/projects/makima-deckery/trackpad/) for the full config reference.

> **Tip:** if you enable `combined_gesture_device` and use quick two-hand gestures (e.g. pinch-zoom), disable "Tap to click" on the individual `Deckery Left/Right Trackpad` devices in your desktop's touchpad settings. Touching down with one pad slightly before the other briefly routes through that pad's individual channel before gesture mode activates; the router's forced clean-lift on gesture entry looks like a fast tap-and-release to libinput, which tap-to-click would otherwise turn into a spurious click.

---

### Lizard Mode suppression

The `hid-steam` kernel driver keeps a built-in mouse/scroll fallback ("Lizard Mode") active unless suppressed. Without it, trackpads emit mouse events directly via the kernel driver, bypassing makima entirely.

Makima-deckery suppresses it via a periodic hidraw heartbeat. If makima exits, the file descriptor closes and Lizard Mode re-activates within ~8 s.

→ [Full Lizard Mode reference](https://plasma-deckery.github.io/deckery/reference/lizard-mode/)

---

### State export → `/tmp/makima-state.json`

On every input event, makima writes a fully-resolved state snapshot to `/tmp/makima-state.json`. This allows the Deckery HUD overlay to display live button mappings without re-implementing any of makima's lookup logic. In paused mode, it enables a live preview of button combinations without any actions firing.

→ [Full state.json reference](https://plasma-deckery.github.io/deckery/reference/state-json/)
