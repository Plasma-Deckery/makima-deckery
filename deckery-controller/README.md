# deckery-controller

Steam Deck controller library for `makima-deckery` and `deckery-auth`.

Encapsulates everything specific to the Steam Deck as a physical device: evdev event streaming (suspend/resume transparent), hidraw I/O, Lizard Mode suppression, click-pressure thresholds, haptic playback, and the cooperative grab yield protocol.

Higher-level concerns — input mapping, virtual devices, config routing — belong in the consuming binary.

---

## Module layout

```
lib.rs              SteamDeckController, ControllerSession, public API
hidraw.rs           PadFrame reader + unified writer (owns the hidraw fd)
haptic.rs           HapticChain API + async player task
lizard_mode.rs      Lizard Mode suppression heartbeat helpers
resume_watcher.rs   logind PrepareForSleep D-Bus watcher
grab_coordinator.rs D-Bus signals for cooperative grab coordination
yield_protocol.rs   open_grabbed, GrabbedHandle RAII
```

---

## Usage

### Path A — device path already known (makima)

```rust
let controller = SteamDeckController::from_evdev(
    Path::new("/dev/input/event7"),
    /*yieldable=*/ true,
);
let session = controller.start(
    /*grab=*/ false,
    device_error_notify.clone(),
    Some(lizard_cfg),
).await?;

// Use session fields:
// session.event_rx             — ControllerEvent channel
// session.pad_rx               — PadFrame channel (trackpad position)
// session.haptic_tx            — HapticRequest channel
// session.lizard_mode.set(cfg) — live Lizard Mode update
// session.click_pressure       — click-pressure threshold setter
// session.grab_handle          — RAII grab guard (None when grab=false)
```

### Path B — find device by name (deckery-auth, standalone tools)

```rust
let controller = SteamDeckController::find(/*yieldable=*/ false)
    .ok_or("no Steam Deck controller found")?;

let session = controller.start(
    /*grab=*/ true,
    Arc::new(Notify::new()),
    Some(lizard_cfg),
).await?;
```

---

## ControllerSession fields

| Field | Type | Notes |
|---|---|---|
| `event_rx` | `Receiver<ControllerEvent>` | Main event stream. Keep receiving; the task reconnects on suspend/resume transparently. |
| `pad_rx` | `Option<Receiver<PadFrame>>` | Raw hidraw trackpad frames. `None` if no hidraw sibling was found via sysfs. |
| `haptic_tx` | `Option<Sender<HapticRequest>>` | Send haptic chains here. `None` if no hidraw sibling. |
| `lizard_mode` | `LizardModeHandle` | **Must be kept alive** — dropping it shuts down the hidraw writer. Call `.set(cfg)` for live updates. |
| `click_pressure` | `Option<ClickPressureHandle>` | Call `.set(Some(cfg))` once at session start; the firmware retains the value. |
| `grab_handle` | `Option<GrabbedHandle>` | RAII guard for the evdev grab (see yield protocol below). **Must be kept alive** for the full grab session — dropping it emits `GrabReleased` on D-Bus. `None` when `grab=false`. |

---

## ControllerEvent variants

```rust
pub enum ControllerEvent {
    Input(InputEvent),   // Normal evdev event
    Reconnected,         // Device reappeared after suspend/resume — release all held output keys
    ReleaseAll,          // Another process is grabbing — release all held output keys immediately
}
```

Consumers must handle all three. Both `Reconnected` and `ReleaseAll` require the same action: call `release_all_held()` (or equivalent) to avoid stuck virtual keys.

---

## Suspend / resume transparency

`reconnecting_reader_task` subscribes to the logind `PrepareForSleep` signal and proactively drops and reopens the evdev stream on resume. Consumers see a `ControllerEvent::Reconnected` and must release held keys.

**Critical quirk:** the old `EventStream` must be `drop()`ped *before* calling `try_open_event_stream` again. If `grab=true`, the old fd holds `EVIOCGRAB`. Keeping it alive while attempting a new grab causes `EBUSY` for the entire reconnect timeout (10 s), silently triggering a full reinit. This is handled correctly internally; only relevant if you call `try_open_event_stream` directly.

Reconnect constants:
- `RECONNECT_TIMEOUT` — 10 s before firing `device_error_notify`
- `RECONNECT_POLL_INTERVAL` — 200 ms between attempts
- 300 ms initial wait on reactive reconnect (kernel USB re-enumeration)

---

## Lizard Mode suppression

The Steam Deck controller ships in "Lizard Mode": it emulates a mouse and keyboard by default so Steam Input can run without a driver. `LizardModeSuppression` disables this by periodically sending HID feature report `0x87` via hidraw.

The heartbeat runs inside the hidraw writer task — no separate timer needed. Dropping `LizardModeHandle` stops the heartbeat and exits the writer cleanly.

```rust
let lizard_cfg = LizardModeSuppression {
    suppress_buttons: true,
    suppress_mouse:   true,
};
session.lizard_mode.set(Some(lizard_cfg)); // live update, no restart needed
```

---

## hidraw discovery

`find_controller_hidraw_for_evdev` walks sysfs to find the raw controller hidraw sibling of an evdev node. The Steam Deck exposes three hidraw nodes per USB interface; the correct one is identified by the absence of an `input/` subdirectory in its sysfs path (the other two are the emulated keyboard/mouse channels).

If discovery fails (non-Steam Deck hardware, sysfs not available), `hidraw_path` is `None` and `pad_rx`, `haptic_tx`, `click_pressure` are all `None`.

---

## Cooperative grab yield protocol

### Problem

`EVIOCGRAB` is exclusive: only one process can hold it per evdev device. When `deckery-auth` needs to grab the controller for PIN entry, any other `grab=true` session must release first. Additionally, `grab=false` sessions (like makima) hold virtual output keys that will become stuck if the physical release events occur while their evdev stream is paused.

### Solution — D-Bus signals

```
Interface:  org.Deckery.Controller1  (system bus; session bus in tests)
Object:     /org/Deckery/Controller1
Signals:    GrabPending(device_path: str)
            GrabReleased(device_path: str)
```

### Requester flow (`grab=true, yieldable=false` — e.g. deckery-auth)

1. Establish one D-Bus connection for the entire grab session.
2. Emit `GrabPending` on that connection — all subscribed yieldable sessions receive it.
3. Retry `EVIOCGRAB` every 100 ms for up to 5 s (`GRAB_TIMEOUT`).
4. On success: return `(EventStream, GrabbedHandle)`. `GrabbedHandle` holds the connection.
5. On `drop(handle)`: emit `GrabReleased` on the **same** connection — no new handshake.

The connection is established once so signal emission costs microseconds (one round-trip on an already-authenticated socket) rather than a full connect + handshake.

### Yieldable session flow (`yieldable=true`)

#### grab=false (e.g. makima)

Subscribes a D-Bus listener. On `GrabPending`:
- Sends `ControllerEvent::ReleaseAll` → consumer flushes held virtual output keys.

The evdev stream pauses automatically while `EVIOCGRAB` is held by another process and resumes on its own — no EVIOCGRAB coordination needed.

#### grab=true (stub — no caller today)

On `GrabPending`:
- Send `ControllerEvent::ReleaseAll` (flush held output keys).
- Drop the `EventStream` (releases `EVIOCGRAB`).
- Wait for `GrabReleased`.

On `GrabReleased`:
- Retry `EVIOCGRAB` in a loop.
- On success: resume normally.
- On timeout: trigger error or continue without grab.

**This path is not yet implemented.** The listener sends `ReleaseAll` but does not release `EVIOCGRAB`. There is no `grab=true + yieldable=true` caller in the current codebase.

### The `yieldable` flag

| `yieldable` | `grab` | Effect |
|---|---|---|
| `false` | `false` | No listener, no protocol. |
| `false` | `true` | Requester: emits GrabPending/GrabReleased, never receives them. |
| `true` | `false` | Listener active: receives GrabPending → ReleaseAll. EVIOCGRAB not involved. |
| `true` | `true` | Listener active: ReleaseAll on GrabPending. EVIOCGRAB release on GrabPending + re-grab on GrabReleased **— stub, not implemented.** |

### GrabbedHandle

```rust
pub struct GrabbedHandle { /* private */ }
```

RAII guard returned by `open_grabbed`. Holds the pre-established D-Bus connection. Dropping it emits `GrabReleased` by spawning an async task on the current Tokio runtime.

**Must be kept alive** for the full grab session. Store it in the session owner struct (e.g. `TokenReader::_grab_handle`). If dropped outside a Tokio runtime (process shutdown edge case), `GrabReleased` is not emitted — logged as a warning, not a panic.

### DOs and DON'Ts

✅ **DO:**
- Keep `GrabbedHandle` alive for the full grab lifetime.
- Emit `GrabPending` before the first `EVIOCGRAB` attempt (not only on `EBUSY`).
- Establish the D-Bus connection once per grab session; reuse for both signals.
- Let the library decide what `yieldable` means — consumers just pass the flag.

❌ **DON'T:**
- Create a new D-Bus connection per signal (`Connection::system().await` is expensive and can hang).
- Add yield protocol logic to makima or other consumers — it belongs entirely in this library.
- Drop `GrabbedHandle` before the grab session ends.
- Emit `GrabPending` only on `EBUSY` — `grab=false` sessions need it too for key flushing.

---

## Testing

### D-Bus tests — `grab_coordinator` (4 tests)

Use the **session bus** (`Connection::session()`) in `#[cfg(test)]` so no root access is required. Tests verify signal delivery, path filtering, and listener lifecycle.

### Protocol integration tests — `yield_protocol` (4 tests)

`open_grabbed_with<S>` accepts an injected `try_grab: impl Fn(&Path) -> io::Result<S>` closure, making the grab operation mockable without real evdev devices. Tests use `S = ()` and an `Arc<AtomicBool>` to simulate `EBUSY` / release.

The `spawn_yieldable` helper in the tests subscribes two D-Bus listeners on the same signal:
- One via `spawn_grab_listener` → delivers `ControllerEvent::ReleaseAll` to the test channel.
- One raw `MessageStream` → flips the `AtomicBool` to simulate `EVIOCGRAB` release.

This simulates the full D-Bus roundtrip:
`GrabPending` → yieldable reacts → `AtomicBool` clears → requester succeeds → `drop(handle)` → `GrabReleased` on bus.

### What is NOT tested

- Full `EVIOCGRAB` handoff between two real processes (requires two evdev devices or uinput setup; tested manually on the Steam Deck).
- `grab=true + yieldable=true` EVIOCGRAB release / re-grab (stub, no caller).
- `grab_coordinator` D-Bus connection failure fallback (D-Bus unavailable path).
- Actual timeout (5 s `GRAB_TIMEOUT`) when `EBUSY` never clears.

---

## Known limitations

- **`grab=true + yieldable=true` EVIOCGRAB release is a stub.** The listener sends `ReleaseAll` but does not drop the `EventStream` or wait for `GrabReleased` to re-grab. Implementation requires two `Arc<Notify>` channels between the listener task and `reconnecting_reader_task`.
- **`GrabbedHandle::drop` spawns a fire-and-forget task.** If the Tokio runtime shuts down before the task completes, `GrabReleased` may not be emitted.
- **hidraw discovery is sysfs-dependent.** Non-standard kernel configurations or containers without `/sys` will result in `hidraw_path = None`.
- **`click_pressure` is sent once at startup.** The firmware retains it until USB reset; no re-send on reconnect is needed, but no re-send happens either if the value changes after startup.
