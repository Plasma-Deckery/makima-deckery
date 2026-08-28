# deckery-controller

Steam Deck controller library used by `makima-deckery` and `deckery-auth`.

The crate owns everything specific to the Steam Deck as a physical device: evdev event streaming with transparent suspend/resume handling, hidraw I/O for trackpad position and haptic feedback, Lizard Mode suppression, click-pressure thresholds, and the cooperative grab yield protocol. Higher-level concerns — input mapping, virtual devices, config routing — belong in the consuming binary.

---

## Module layout

```
lib.rs              SteamDeckController, ControllerSession, public API
hidraw.rs           PadFrame reader + unified writer (owns the hidraw fd)
haptic.rs           HapticChain API + async player task
lizard_mode.rs      Lizard Mode suppression heartbeat
resume_watcher.rs   logind PrepareForSleep D-Bus watcher
grab_coordinator.rs D-Bus signals for cooperative grab coordination
yield_protocol.rs   open_grabbed, GrabbedHandle RAII
```

---

## Getting started

Construct a `SteamDeckController` either from a known path (makima, which gets the path from udev) or by scanning for the device by name (deckery-auth, standalone tools):

```rust
// From a known path:
let controller = SteamDeckController::from_evdev(
    Path::new("/dev/input/event7"),
    /*yieldable=*/ true,
);

// Or by scanning:
let controller = SteamDeckController::find(/*yieldable=*/ false)
    .ok_or("no Steam Deck controller found")?;
```

Then call `start()` to open the device and spawn all background tasks:

```rust
let session = controller.start(
    /*grab=*/ true,
    device_error_notify.clone(),
    Some(LizardModeSuppression { suppress_buttons: true, suppress_mouse: true }),
).await?;
```

`ControllerSession` holds the caller-facing ends of all internal channels:

- `event_rx: Receiver<ControllerEvent>` — the main event channel. Receive from this in a loop; the internal task reconnects transparently on suspend/resume.
- `pad_rx: Option<Receiver<PadFrame>>` — raw hidraw trackpad frames. `None` if no hidraw sibling was found via sysfs.
- `haptic_tx: Option<Sender<HapticRequest>>` — send haptic chains here. `None` if no hidraw sibling.
- `lizard_mode: LizardModeHandle` — call `.set(cfg)` to update Lizard Mode suppression live. **Must be kept alive** for the session lifetime — dropping it shuts down the hidraw writer.
- `click_pressure: Option<ClickPressureHandle>` — call `.set(Some(cfg))` once at session start; the firmware retains the value across reconnects.
- `grab_handle: Option<GrabbedHandle>` — RAII guard for the evdev grab (see yield protocol below). **Must be kept alive** for the full grab session; dropping it emits `GrabReleased` on D-Bus. `None` when `grab=false`.

---

## Events

The `event_rx` channel produces three variants:

```rust
ControllerEvent::Input(InputEvent)  // normal evdev event
ControllerEvent::Reconnected        // device reappeared after suspend/resume
ControllerEvent::ReleaseAll         // another process is about to grab
```

Both `Reconnected` and `ReleaseAll` require the same consumer action: release all currently held virtual output keys. If you ignore `Reconnected`, modifier keys stay stuck after resume. If you ignore `ReleaseAll`, output keys stay pressed while another process holds the exclusive grab.

---

## Suspend/resume

`reconnecting_reader_task` watches the logind `PrepareForSleep` signal and proactively reopens the evdev stream on resume. From the consumer's perspective the channel simply keeps producing events with a `Reconnected` in the middle — no special handling beyond releasing held keys is required.

One important implementation detail: the old `EventStream` must be dropped *before* attempting to open a new one. The `EventStream` holds an open evdev fd, and if `grab=true` that fd holds `EVIOCGRAB`. Trying to re-grab while the old fd is still alive returns `EBUSY`, causing the entire reconnect timeout (10 s) to elapse before triggering a spurious full reinit. The library handles this correctly internally; it is only relevant if you call `try_open_event_stream` directly.

Reconnect constants:
- `RECONNECT_TIMEOUT` — 10 s before firing `device_error_notify` and exiting the task
- `RECONNECT_POLL_INTERVAL` — 200 ms between attempts
- 300 ms initial wait on reactive reconnect to allow the kernel to complete USB re-enumeration

---

## Lizard Mode suppression

The Steam Deck controller ships in Lizard Mode: it emulates a mouse and keyboard by default so Steam can use it without a driver. The library suppresses this by periodically sending HID feature report `0x87` to the hidraw fd. The heartbeat runs inside the hidraw writer task — no separate timer is needed in the consumer. Dropping `LizardModeHandle` stops the heartbeat and cleanly exits the writer.

```rust
// Live update without session restart:
session.lizard_mode.set(Some(LizardModeSuppression {
    suppress_buttons: true,
    suppress_mouse:   true,
}));
```

---

## hidraw discovery

`find_controller_hidraw_for_evdev` walks sysfs to find the raw controller hidraw node that corresponds to a given evdev node. The Steam Deck exposes three hidraw nodes per USB interface; the raw controller channel is identified by the absence of an `input/` subdirectory in its sysfs path (the other two nodes back the emulated keyboard and mouse).

```
evdev:  /sys/class/input/eventN/device  → …/usb_iface/HID_A/input/inputN
hidraw: /sys/class/hidraw/hidrawN/device → …/usb_iface/HID_B   (no input/ → raw controller)
        /sys/class/hidraw/hidrawM/device → …/usb_iface/HID_C   (has input/ → emulated)
```

If discovery fails — non-Steam Deck hardware, missing sysfs — `hidraw_path` is `None` and `pad_rx`, `haptic_tx`, and `click_pressure` are all `None`.

---

## Cooperative grab yield protocol

### Background

`EVIOCGRAB` gives a process exclusive access to an evdev device. When `deckery-auth` needs to grab the controller for PIN entry, any other `grab=true` session holding the grab must release it first. Additionally, `grab=false` sessions like makima accumulate virtual output key state; if physical release events arrive while their evdev stream is paused during the grab period, those virtual keys remain stuck.

The protocol coordinates this via D-Bus signals on the system bus (session bus in tests):

```
Interface:  org.Deckery.Controller1
Object:     /org/Deckery/Controller1
Signals:    GrabPending(device_path: str)
            GrabReleased(device_path: str)
```

### Requester flow

`open_grabbed` establishes a single D-Bus connection for the entire grab session, then emits `GrabPending` immediately on that connection before the first `EVIOCGRAB` attempt. It then retries `EVIOCGRAB` every 100 ms for up to 5 s (`GRAB_TIMEOUT`). On success it returns `(EventStream, GrabbedHandle)`. `GrabbedHandle` holds the pre-established connection; dropping it emits `GrabReleased` on the same already-authenticated socket — no new handshake, no additional latency. The connection is established once so each signal costs microseconds rather than a full connect + auth round-trip.

`GrabPending` is emitted before the first attempt — not deferred to the first `EBUSY` — because `grab=false` sessions need to flush held output keys regardless of whether a grab conflict actually exists.

### Yieldable session flow

A session with `yieldable=true` has a background listener subscribed to `GrabPending` for its device path. When the signal arrives, the listener forwards `ControllerEvent::ReleaseAll` to the consumer so it can flush held virtual output keys.

For `grab=false` sessions (makima) this is the entire job: flush keys, continue. The evdev stream pauses automatically while `EVIOCGRAB` is held by another process and resumes on its own — no further coordination is needed.

For `grab=true + yieldable=true` sessions the listener additionally signals `reconnecting_reader_task` to drop the `EventStream` (releasing `EVIOCGRAB`), waits for `GrabReleased`, then re-grabs using the same retry loop as suspend/resume reconnect.

### The `yieldable` flag

The consuming binary's only protocol touchpoint is the `yieldable` flag passed to `from_evdev` or `find`. The library decides what to do based on the combination with `grab`:

| `grab` | `yieldable` | Effect |
|---|---|---|
| `false` | `false` | No listener, no protocol involvement. |
| `false` | `true` | Listener active: `GrabPending` → `ReleaseAll`. No EVIOCGRAB involvement. |
| `true` | `false` | Requester: emits `GrabPending`/`GrabReleased`, never receives them. |
| `true` | `true` | Full participation: `ReleaseAll` + EVIOCGRAB release on `GrabPending`, re-grab on `GrabReleased`. |

### GrabbedHandle

`GrabbedHandle` is a RAII guard. Store it in the session owner struct for the full grab lifetime — typically as a field prefixed with `_` to signal that it is held only for its `Drop` side effect. Dropping it emits `GrabReleased` by spawning a task on the current Tokio runtime. If dropped outside a runtime (process shutdown edge case), `GrabReleased` is not emitted and a warning is printed.

```rust
// In the session owner struct:
struct MySession {
    event_rx:     mpsc::Receiver<ControllerEvent>,
    _lizard_mode: LizardModeHandle,   // must live as long as the session
    _grab_handle: Option<GrabbedHandle>, // GrabReleased on drop
}
```

---

## Testing

### grab_coordinator — signal delivery (4 tests)

Tests use the D-Bus **session bus** (`Connection::session()`) so no root access is required and any CI environment with a running `dbus-daemon` works. Each test uses a unique fake device path as a filter so tests can run in parallel on the same bus without interfering with each other's signals.

The four tests cover: `GrabPending` delivers `ReleaseAll` to the subscriber, path filtering (signals for other devices are ignored), `GrabReleased` is silently ignored (stub), listener task exits when the consumer channel is closed.

### yield_protocol — protocol integration (4 tests)

The grab operation is injected via `open_grabbed_with<S>`, which accepts a `try_grab: Fn(&Path) -> io::Result<S>` closure. This allows the EVIOCGRAB interaction to be replaced with a mock — no real evdev hardware required. Tests use `S = ()` and an `Arc<AtomicBool>` to simulate `EBUSY`/release.

The `spawn_yieldable` helper subscribes two D-Bus listeners on the same signal for a given device path: one via `spawn_grab_listener` (delivers `ControllerEvent::ReleaseAll` to the test channel) and one raw `MessageStream` (flips the `AtomicBool` to simulate EVIOCGRAB release). Together they produce a genuine D-Bus roundtrip: `GrabPending` out → yieldable reacts → bool clears → requester loop succeeds → `drop(handle)` → `GrabReleased` on bus.

The four tests cover: full handoff (yieldable releases on `GrabPending`, requester acquires), `GrabbedHandle::drop` emits `GrabReleased` verified via raw D-Bus subscription, non-EBUSY errors propagate immediately without entering the retry loop, `GrabPending` is always emitted upfront even when the device is immediately available.

### What is not tested

The actual EVIOCGRAB handoff between two real processes requires two evdev devices or uinput setup and is tested manually on the Steam Deck. The `grab=true + yieldable=true` EVIOCGRAB release/re-grab path is implemented but has no automated test (the two-process handoff needs real hardware). The D-Bus unavailable fallback (silent no-op when `connect()` fails) and the 5 s `GRAB_TIMEOUT` with persistent `EBUSY` are also not covered by automated tests.

---

## Limitations

**`EVIOCGRAB` only covers the evdev stream, not hidraw.** Grabbing `/dev/input/eventN` gives exclusive ownership of the evdev event stream. The hidraw node (`/dev/hidrawN`) is a completely separate kernel interface with no equivalent exclusive-lock mechanism. Any process that can open the hidraw node can read raw HID reports — which include the full button bitmask — even while an evdev grab is active. Access control for hidraw must be enforced at the udev/filesystem level, not through the grab protocol.

**`GrabbedHandle::drop` is fire-and-forget.** It spawns a task that may not complete if the Tokio runtime shuts down before the emission finishes. In practice this is not a problem since the grab session ends with the process, but `GrabReleased` is not guaranteed on abrupt shutdown.

**hidraw discovery is sysfs-dependent.** Non-standard kernel configurations, containers without `/sys`, or non-Steam Deck hardware result in `hidraw_path = None` and no trackpad, haptics, or Lizard Mode suppression.

**`click_pressure` is not re-sent on USB reset.** The Steam Deck firmware retains click-pressure thresholds until a USB reset, so re-sending on reconnect is not necessary under normal conditions. If a USB reset does occur, the firmware reverts to its default threshold. The library does not currently re-send on reconnect.
