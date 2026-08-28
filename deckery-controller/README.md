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

`ControllerSession` holds the caller-facing ends of all internal channels. The most important fields:

- `event_rx` — the main event channel. Receive from this in a loop; the internal task reconnects transparently on suspend/resume.
- `pad_rx` — raw hidraw trackpad frames (`None` if no hidraw sibling was found).
- `haptic_tx` — send `HapticRequest` here to play haptic feedback.
- `lizard_mode` — call `.set(cfg)` to update Lizard Mode suppression live. **Must be kept alive** for the session lifetime — dropping it shuts down the hidraw writer.
- `grab_handle` — RAII guard for the evdev grab. **Must be kept alive**; dropping it emits `GrabReleased` on D-Bus. `None` when `grab=false`.

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

`reconnecting_reader_task` watches the logind `PrepareForSleep` signal and proactively reopens the evdev stream on resume. From the consumer's perspective, the channel simply keeps producing events with a `Reconnected` in the middle.

One subtlety: the old `EventStream` must be dropped *before* attempting to open a new one. The `EventStream` holds an open evdev fd, and if `grab=true` that fd holds `EVIOCGRAB`. Trying to re-grab while the old fd is still alive returns `EBUSY`, causing the entire reconnect timeout (10 s) to elapse before triggering a full reinit. The library handles this correctly internally.

---

## Lizard Mode

The Steam Deck controller ships in Lizard Mode: it emulates a mouse and keyboard so Steam Input can run without a driver. The library suppresses this by periodically sending HID feature report `0x87` to the hidraw fd. The heartbeat runs inside the hidraw writer task. Dropping `LizardModeHandle` stops the heartbeat and cleanly exits the writer.

---

## hidraw discovery

`find_controller_hidraw_for_evdev` walks sysfs to find the raw controller hidraw node that corresponds to a given evdev node. The Steam Deck exposes three hidraw nodes per USB interface; the raw controller channel is identified by the absence of an `input/` subdirectory in its sysfs path (the other two nodes back the emulated keyboard and mouse). If discovery fails — non-Steam Deck hardware, missing sysfs — `hidraw_path` is `None` and `pad_rx`, `haptic_tx`, `click_pressure` are all `None`.

---

## Cooperative grab yield protocol

### Background

`EVIOCGRAB` gives a process exclusive access to an evdev device. When `deckery-auth` needs to grab the controller for PIN entry, any other `grab=true` session holding the grab must release it first. Additionally, `grab=false` sessions like makima accumulate virtual output key state; if the physical release events arrive while their stream is paused during the grab, those virtual keys will be stuck.

The protocol coordinates this via D-Bus signals on the system bus (session bus in tests):

```
Interface:  org.Deckery.Controller1
Object:     /org/Deckery/Controller1
Signals:    GrabPending(device_path: str)
            GrabReleased(device_path: str)
```

### How the requester works

`open_grabbed` establishes a single D-Bus connection for the entire grab session, emits `GrabPending` immediately, then retries `EVIOCGRAB` every 100 ms until success or a 5 s timeout. On success it returns `(EventStream, GrabbedHandle)`. `GrabbedHandle` holds the connection; dropping it emits `GrabReleased` on the same socket — no new handshake, no additional latency.

### How yieldable sessions work

A session with `yieldable=true` has a background listener subscribed to `GrabPending` for its device path. When the signal arrives, the listener forwards `ControllerEvent::ReleaseAll` to the consumer so it can flush held output keys.

For `grab=false` sessions (makima) this is the entire job: flush keys, continue. The evdev stream pauses automatically while the grab is held and resumes on its own.

For `grab=true + yieldable=true` sessions the listener should additionally drop the `EventStream` to release `EVIOCGRAB`, wait for `GrabReleased`, then re-grab. This path is not yet implemented — see Limitations.

### The `yieldable` flag

Pass `yieldable=true` to opt the session into the protocol. The library decides what to do based on the combination with `grab`:

- `grab=false, yieldable=true` — listener active, receives `ReleaseAll` on `GrabPending`. No EVIOCGRAB involvement.
- `grab=true, yieldable=false` — requester: emits signals, never receives them.
- `grab=true, yieldable=true` — full bidirectional participation. EVIOCGRAB release on `GrabPending`, re-grab on `GrabReleased`. Currently a stub.

The consuming binary's only protocol touchpoint is the `yieldable` flag. Everything else is internal to the library.

### GrabbedHandle lifetime

`GrabbedHandle` is a RAII guard. Keep it alive in the session owner struct for as long as the grab should be held. Dropping it emits `GrabReleased` by spawning a task on the current Tokio runtime. If dropped outside a runtime (process shutdown edge case), `GrabReleased` is not emitted and a warning is printed.

---

## Testing

Tests in `grab_coordinator` use the D-Bus **session bus** so no root access is required. Four tests cover signal delivery, path filtering, and listener teardown.

Tests in `yield_protocol` use `open_grabbed_with<S>`, which accepts an injected `try_grab: Fn(&Path) -> io::Result<S>` closure. This makes the grab operation mockable without real evdev hardware. Tests use `S = ()` and an `Arc<AtomicBool>` to simulate `EBUSY`/release, producing a genuine D-Bus roundtrip: `GrabPending` out → yieldable reacts → bool clears → requester succeeds → `drop(handle)` → `GrabReleased` on bus.

The `grab_coordinator` tests and `yield_protocol` tests run in parallel on the same session bus; each test uses a unique fake device path as a filter to avoid cross-test signal interference.

---

## Limitations

**`grab=true + yieldable=true` EVIOCGRAB release is not implemented.** The listener sends `ReleaseAll` but does not drop the `EventStream` or wait for `GrabReleased` to re-grab. There is currently no caller with this combination — deckery-auth uses `yieldable=false` and makima uses `grab=false`. When needed, the implementation requires two `Arc<Notify>` channels connecting the listener task to `reconnecting_reader_task`.

**`EVIOCGRAB` only covers the evdev stream, not hidraw.** Grabbing `/dev/input/eventN` gives exclusive ownership of the evdev event stream. The hidraw node (`/dev/hidrawN`) is a separate kernel interface with no equivalent exclusive-lock mechanism. Any process that can open the hidraw node can read raw HID reports — which include full button state — even while an evdev grab is active. Access control for hidraw must be enforced at the udev/filesystem level, not through the grab protocol.

**`GrabbedHandle::drop` is fire-and-forget.** It spawns a task that may not complete if the Tokio runtime shuts down before the emission finishes. In practice this is not a problem since the grab session ends when the process exits, but it means `GrabReleased` is not guaranteed on abrupt shutdown.

**hidraw discovery is sysfs-dependent.** Non-standard kernel configurations, containers without `/sys`, or non-Steam Deck hardware will result in `hidraw_path = None` and no trackpad, haptics, or Lizard Mode suppression.

**`click_pressure` is not re-sent on reconnect.** The Steam Deck firmware retains the threshold value until USB reset, so re-sending on reconnect is not necessary under normal conditions. If a USB reset does occur, the firmware reverts to its default threshold. Re-sending on reconnect is not currently implemented.
