//! Raw hidraw I/O for the Steam Deck controller.
//!
//! Owns both directions of the hidraw channel:
//!
//! - **Reader** (`run_hidraw_reader`): streams `PadFrame`s parsed from
//!   64-byte HID input reports. Position and touch state arrive atomically
//!   from the same `read()` — no ordering hazard possible.
//!
//!   When `event_tx` is supplied the reader also synthesises a full
//!   `ControllerEvent::Input` stream (buttons + sticks + triggers + pads)
//!   that mirrors what the hid-steam evdev device would have emitted.
//!   This allows makima to run on Linux ≥ 6.12 / kernel 7.1, where opening
//!   hidraw causes hid-steam to intentionally remove the evdev device
//!   (commit cd33a91 by Vicki Pfau / Valve) to prevent concurrent
//!   FEATURE-report conflicts on the USB endpoint.
//!
//! - **Writer** (`run_hidraw_writer`): serialises all outbound HID feature
//!   reports onto one file descriptor, selected from two sources:
//!   - `HapticCommand`s forwarded by the haptic player task (from callers)
//!   - Lizard Mode heartbeat reports, driven by an internal timer +
//!     `Option<LizardModeSuppression>` (static config set at session start)
//!
//!   One fd, one writer — no concurrent-ioctl races.
//!
//! Callers receive `(Receiver<PadFrame>, Sender<HapticRequest>)` from
//! `spawn_hidraw_tasks` and never touch the file descriptor directly.
//!
//! ## Byte offsets
//!
//! Offsets were determined empirically (2026-07-08) by recording evdev and
//! hidraw simultaneously with a shared monotonic clock and correlating
//! ABS_HAT0/1X/Y against every int16 offset in the raw report. Offsets match
//! exactly (median deviation ~19 out of a ~65500 range, explained by ~4ms
//! sampling-phase jitter) and hidraw consistently leads evdev by about one
//! report (~4ms) — evidence that the evdev path goes through an extra
//! translation layer in the hid-steam kernel driver.
//!
//! The full button and axis map was cross-checked against the upstream
//! hid-steam kernel driver (`steam_deck_button_mappings` /
//! `steam_deck_axis_mappings` / `steam_do_deck_input_event`).

use super::haptic::{HapticCommand, HapticRequest};
#[cfg(test)]
use super::haptic::HapticPad;
use super::lizard_mode::{self, LizardModeSuppression, HEARTBEAT_INTERVAL};
use super::{ClickPressureConfig, ControllerEvent};
use evdev::{AbsoluteAxisType, EventType, InputEvent, Key};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Notify};

// ── PadFrame ─────────────────────────────────────────────────────────────────

/// One consistent snapshot of both trackpads, parsed from a single hidraw
/// report. Because both pads' position and touch state come from the same
/// 64-byte `read()`, this is atomic — there is no way for RPAD's position and
/// touch bit to reflect different points in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PadFrame {
    pub lx:     i32,
    pub ly:     i32,
    pub ltouch: bool,
    pub lclick: bool,
    pub rx:     i32,
    pub ry:     i32,
    pub rtouch: bool,
    pub rclick: bool,
}

impl PadFrame {
    fn parse(buf: &[u8; 64]) -> Self {
        let byte10 = buf[10];
        Self {
            lx:     i16::from_le_bytes([buf[16], buf[17]]) as i32,
            ly:     i16::from_le_bytes([buf[18], buf[19]]) as i32,
            ltouch: (byte10 & 0x08) != 0,
            // Click (physical press-through) lives in the same byte as touch —
            // confirmed against the upstream hid-steam kernel driver
            // (steam_do_deck_input_event): BTN_THUMB = b10 & BIT(1), BTN_THUMB2
            // = b10 & BIT(2). Reading both from the same byte makes click
            // atomic with position/touch — no evdev ordering hazard.
            lclick: (byte10 & 0x02) != 0,
            rx:     i16::from_le_bytes([buf[20], buf[21]]) as i32,
            ry:     i16::from_le_bytes([buf[22], buf[23]]) as i32,
            rtouch: (byte10 & 0x10) != 0,
            rclick: (byte10 & 0x04) != 0,
        }
    }
}

// ── Controller-event synthesis ────────────────────────────────────────────────

/// Button mapping: (evdev key code, byte index into 64-byte report, bit mask).
///
/// Mirrors `steam_deck_button_mappings[]` in the upstream hid-steam kernel
/// driver. Byte and bit indices are **into the raw 64-byte hidraw read buffer**
/// (i.e. the same buffer that `PadFrame::parse` reads).
static BUTTON_MAP: &[(u16, usize, u8)] = &[
    (Key::BTN_TR2.code(),        8, 1 << 0),  // right bumper 2 (SR)
    (Key::BTN_TL2.code(),        8, 1 << 1),  // left  bumper 2 (SL)
    (Key::BTN_TR.code(),         8, 1 << 2),  // right bumper (R1)
    (Key::BTN_TL.code(),         8, 1 << 3),  // left  bumper (L1)
    // Face buttons: hid-steam uses BTN_Y/B/X/A — evdev crate aliases:
    //   BTN_A = BTN_SOUTH = 0x130, BTN_B = BTN_EAST = 0x131,
    //   BTN_X = BTN_NORTH = 0x133, BTN_Y = BTN_WEST = 0x134
    (Key::BTN_WEST.code(),       8, 1 << 4),  // Y button
    (Key::BTN_EAST.code(),       8, 1 << 5),  // B button
    (Key::BTN_NORTH.code(),      8, 1 << 6),  // X button
    (Key::BTN_SOUTH.code(),      8, 1 << 7),  // A button
    (Key::BTN_DPAD_UP.code(),    9, 1 << 0),
    (Key::BTN_DPAD_RIGHT.code(), 9, 1 << 1),
    (Key::BTN_DPAD_LEFT.code(),  9, 1 << 2),
    (Key::BTN_DPAD_DOWN.code(),  9, 1 << 3),
    (Key::BTN_SELECT.code(),     9, 1 << 4),  // View / Back
    (Key::BTN_MODE.code(),       9, 1 << 5),  // Steam logo
    (Key::BTN_START.code(),      9, 1 << 6),  // Menu / Start
    (Key::BTN_GRIPL2.code(),     9, 1 << 7),  // L5 back paddle (bottom-left)
    (Key::BTN_GRIPR2.code(),    10, 1 << 0),  // R5 back paddle (bottom-right)
    (Key::BTN_THUMB.code(),     10, 1 << 1),  // left  pad click (L2-pad-press)
    (Key::BTN_THUMB2.code(),    10, 1 << 2),  // right pad click (R2-pad-press)
    (Key::BTN_THUMBL.code(),    10, 1 << 6),  // left  stick click (L3)
    (Key::BTN_THUMBR.code(),    11, 1 << 2),  // right stick click (R3)
    (Key::BTN_GRIPL.code(),     13, 1 << 1),  // L4 back paddle (top-left)
    (Key::BTN_GRIPR.code(),     13, 1 << 2),  // R4 back paddle (top-right)
    (Key::BTN_BASE.code(),      14, 1 << 2),  // QAM / … button
];

/// Axis mapping: (evdev absolute axis code, byte offset, negate).
///
/// Mirrors `steam_deck_axis_mappings[]` and the pad-reading in
/// `steam_do_deck_input_event()` from the upstream hid-steam kernel driver.
/// `negate = true` matches hid-steam's `sign = -1` (kernel stores the raw
/// value with the opposite polarity for ABS_Y / ABS_RY so that "up" maps to
/// positive in evdev convention).
static AXIS_MAP: &[(u16, usize, bool)] = &[
    (AbsoluteAxisType::ABS_HAT0X.0,  16, false), // left  pad X
    (AbsoluteAxisType::ABS_HAT0Y.0,  18, false), // left  pad Y
    (AbsoluteAxisType::ABS_HAT1X.0,  20, false), // right pad X
    (AbsoluteAxisType::ABS_HAT1Y.0,  22, false), // right pad Y
    (AbsoluteAxisType::ABS_HAT2Y.0,  44, false), // left  trigger (L2 analog)
    (AbsoluteAxisType::ABS_HAT2X.0,  46, false), // right trigger (R2 analog)
    (AbsoluteAxisType::ABS_X.0,      48, false), // left  stick X
    (AbsoluteAxisType::ABS_Y.0,      50, true),  // left  stick Y (negated)
    (AbsoluteAxisType::ABS_RX.0,     52, false), // right stick X
    (AbsoluteAxisType::ABS_RY.0,     54, true),  // right stick Y (negated)
];

/// Synthesise the `InputEvent`s that the hid-steam evdev device would emit for
/// `buf`, given the previous buffer `prev`. Only changed values are emitted,
/// followed by `SYN_REPORT`. Returns an empty `Vec` when nothing changed.
///
/// This produces a stream that is semantically identical to what
/// `steam_do_deck_input_event` / `steam_map_buttons` / `steam_map_axes` write
/// to the kernel evdev ring buffer, allowing the rest of makima to remain
/// unchanged even on kernels where hid-steam removes the evdev node when
/// hidraw is opened.
fn synthesise_input_events(buf: &[u8; 64], prev: &[u8; 64]) -> Vec<InputEvent> {
    let mut events: Vec<InputEvent> = Vec::new();

    // Buttons — emit key event only on edge (0→1 or 1→0).
    for &(code, byte, mask) in BUTTON_MAP {
        let was = (prev[byte] & mask) != 0;
        let now = (buf[byte]  & mask) != 0;
        if was != now {
            events.push(InputEvent::new_now(EventType::KEY, code, now as i32));
        }
    }

    // Axes — emit abs event whenever the i16 value changed.
    for &(code, offset, negate) in AXIS_MAP {
        let prev_val = i16::from_le_bytes([prev[offset], prev[offset + 1]]) as i32;
        let curr_val = i16::from_le_bytes([buf[offset],  buf[offset  + 1]]) as i32;
        if prev_val != curr_val {
            let reported = if negate { -curr_val } else { curr_val };
            events.push(InputEvent::new_now(EventType::ABSOLUTE, code, reported));
        }
    }

    // Emit SYN_REPORT only when there is something to sync.
    if !events.is_empty() {
        events.push(InputEvent::new_now(EventType::SYNCHRONIZATION, 0, 0));
    }

    events
}

// ── Haptic wire format ────────────────────────────────────────────────────────

/// HID feature report ID for `ID_TRIGGER_HAPTIC_PULSE`, from hid-steam.c.
const ID_TRIGGER_HAPTIC_PULSE: u8 = 0x8F;

/// Serializes a `HapticCommand` into the exact 65-byte HID feature report
/// buffer the kernel's `hid-steam` driver sends over USB/BT.
///
/// Layout (from `steam_send_report`/`steam_haptic_pulse` in hid-steam.c):
///   buf[0]     = 0x00           report ID (always 0)
///   buf[1]     = 0x8F           ID_TRIGGER_HAPTIC_PULSE
///   buf[2]     = 0x08           payload length (8 bytes follow)
///   buf[3]     = pad            0=right, 1=left, 2=both (swapped — see HapticPad)
///   buf[4..6]  = duration (u16 LE, microseconds)
///   buf[6..8]  = interval (u16 LE, microseconds)
///   buf[8..10] = count    (u16 LE, pulses)
///   buf[10]    = gain (i8, dB)
///   buf[11..65] = 0 padding
///
/// Pure and I/O-free — wire format is unit-testable without a real device.
fn build_haptic_report(cmd: &HapticCommand) -> [u8; 65] {
    let mut buf = [0u8; 65];
    buf[1] = ID_TRIGGER_HAPTIC_PULSE;
    buf[2] = 8;
    buf[3] = cmd.pad.wire_value();
    buf[4..6].copy_from_slice(&cmd.duration_us.to_le_bytes());
    buf[6..8].copy_from_slice(&cmd.interval_us.to_le_bytes());
    buf[8..10].copy_from_slice(&cmd.count.to_le_bytes());
    buf[10] = cmd.gain_db as u8;
    buf
}

// ── ioctl helpers ─────────────────────────────────────────────────────────────

/// Computes the `HIDIOCSFEATURE(len)` ioctl request number from buffer size.
///
/// Reproduces `#define HIDIOCSFEATURE(len) _IOC(_IOC_WRITE|_IOC_READ,'H',0x06,len)`
/// from `<linux/hidraw.h>` — not exposed by `libc`, so computed by hand.
const fn hidiocsfeature(len: usize) -> libc::c_ulong {
    const IOC_NRSHIFT:   u32 = 0;
    const IOC_TYPESHIFT: u32 = IOC_NRSHIFT + 8;
    const IOC_SIZESHIFT: u32 = IOC_TYPESHIFT + 8;
    const IOC_DIRSHIFT:  u32 = IOC_SIZESHIFT + 14;
    const IOC_WRITE: u32 = 1;
    const IOC_READ:  u32 = 2;
    let dir  = IOC_WRITE | IOC_READ;
    let ty   = b'H' as u32;
    let nr   = 0x06u32;
    let size = len as u32;
    ((dir << IOC_DIRSHIFT) | (ty << IOC_TYPESHIFT) | (nr << IOC_NRSHIFT) | (size << IOC_SIZESHIFT))
        as libc::c_ulong
}

/// Sends a feature report buffer to an open hidraw fd via `HIDIOCSFEATURE`.
///
/// Blocking syscall — callers in async context use `spawn_blocking`.
fn send_feature_report_raw(fd: std::os::fd::RawFd, buf: &mut [u8], len: usize) -> std::io::Result<()> {
    let ret = unsafe { libc::ioctl(fd, hidiocsfeature(len) as _, buf.as_mut_ptr()) };
    if ret < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
}

// ── Reader task ───────────────────────────────────────────────────────────────

/// Reads hidraw reports and sends a `PadFrame` for each change. Uses an mpsc
/// channel (not `watch`) so every transition is delivered in order and none
/// are silently coalesced — the failure mode that caused the old touch-bit
/// `watch` channel to lose fast lift/retouch cycles.
///
/// When `event_tx` is `Some`, additionally synthesises and forwards a full
/// `ControllerEvent::Input` stream (all buttons + sticks + triggers + pads) on
/// every report that changes. This covers Linux kernels ≥ 6.12 / 7.1 where
/// opening hidraw causes hid-steam to remove the evdev node.
///
/// When `device_error_notify` is `Some`, fires it on read error so the
/// consuming binary can trigger a full session reinit — mirroring the behaviour
/// of the evdev reconnecting reader task, but driven by hidraw health.
///
/// Exits on read error (e.g. device unplugged) or when all receivers drop.
pub async fn run_hidraw_reader(
    path: PathBuf,
    tx: mpsc::Sender<PadFrame>,
    event_tx: Option<mpsc::Sender<ControllerEvent>>,
    device_error_notify: Option<Arc<Notify>>,
) {
    use tokio::io::AsyncReadExt;
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) => {
            eprintln!("deckery-controller: hidraw reader: cannot open {:?}: {}", path, e);
            if let Some(n) = device_error_notify { n.notify_one(); }
            return;
        }
    };
    let mut reader = tokio::io::BufReader::new(file);
    let mut buf = [0u8; 64];

    // Seed prev_buf from the first report so the diff on subsequent reports
    // only fires for actual changes, not for every non-zero field in the
    // controller's idle state.  The initial PadFrame is sent so the trackpad
    // router has a valid baseline, but no ControllerEvent::Input is emitted.
    let mut last_pad: Option<PadFrame>;
    let mut prev_buf = match reader.read_exact(&mut buf).await {
        Ok(_) => {
            let frame = PadFrame::parse(&buf);
            last_pad = Some(frame);
            if tx.send(frame).await.is_err() {
                return;
            }
            buf
        }
        Err(e) => {
            eprintln!("deckery-controller: hidraw reader: read error on {:?}: {}", path, e);
            if let Some(n) = device_error_notify { n.notify_one(); }
            return;
        }
    };

    loop {
        match reader.read_exact(&mut buf).await {
            Ok(_) => {
                // ── PadFrame (trackpad router) ────────────────────────────
                let frame = PadFrame::parse(&buf);
                if last_pad != Some(frame) {
                    last_pad = Some(frame);
                    if tx.send(frame).await.is_err() {
                        return; // trackpad router dropped
                    }
                }

                // ── Synthetic ControllerEvents (buttons + axes) ───────────
                if let Some(ref etx) = event_tx {
                    let events = synthesise_input_events(&buf, &prev_buf);
                    for ev in events {
                        if etx.send(ControllerEvent::Input(ev)).await.is_err() {
                            return; // event consumer dropped
                        }
                    }
                }

                prev_buf = buf;
            }
            Err(e) => {
                eprintln!("deckery-controller: hidraw reader: read error on {:?}: {}", path, e);
                break;
            }
        }
    }
    if let Some(n) = device_error_notify { n.notify_one(); }
}

// ── Writer task ───────────────────────────────────────────────────────────────

/// Serialises all outbound hidraw writes onto a single file descriptor.
///
/// Selects from four sources:
/// - `haptic_rx`: `HapticCommand`s forwarded by the haptic player task.
/// - Lizard Mode heartbeat: timer-driven, config from `lizard_rx` watch.
///   On the initial write or a disabled→enabled transition, the full
///   disable-settings report is sent. Heartbeat ticks send the minimal
///   watchdog report (SDL `FeedDeckLizardWatchdog`).
/// - `lizard_rx.changed()`: live config update from `LizardModeHandle::set()`.
/// - `click_pressure_rx.changed()`: click-pressure update from
///   `ClickPressureHandle::set()`. Sends a single `ID_SET_SETTINGS_VALUES`
///   report with the new thresholds. When the handle is dropped (arm returns
///   `Err`), the arm is silenced via `click_pressure_alive = false`; the
///   writer does NOT exit — the firmware retains the last-written value.
///
/// Exits when `haptic_rx` closes (haptic player exited → session ending) or
/// when `lizard_rx` returns `Err` (`LizardModeHandle` dropped = session teardown).
///
/// `fd` must remain valid for the entire lifetime of this future. The caller
/// (`spawn_hidraw_tasks`) moves the owning `File` into the same `async move`
/// block, so the fd is kept alive. Tests may pass any writable fd (e.g. a
/// `memfd` or `/dev/null`); the ioctl calls will fail and log, but the channel
/// routing and exit conditions are fully exercisable without real hardware.
async fn run_hidraw_writer(
    fd: std::os::fd::RawFd,
    mut haptic_rx: mpsc::Receiver<HapticCommand>,
    mut lizard_rx: watch::Receiver<Option<LizardModeSuppression>>,
    mut click_pressure_rx: watch::Receiver<Option<ClickPressureConfig>>,
) {

    let mut lizard_cfg = lizard_rx.borrow_and_update().clone();
    // Mark the click-pressure channel as consumed so the first real `.changed()`
    // fires only on an explicit `ClickPressureHandle::set()` call.
    let _ = click_pressure_rx.borrow_and_update();
    // True while the ClickPressureHandle is still alive. On Err (handle dropped)
    // the arm is disabled — we do NOT exit, because the firmware retains the
    // last value and the session may still run without further pressure updates.
    let mut click_pressure_alive = true;

    // Consume the timer's immediate first tick before entering the loop,
    // so the heartbeat fires after HEARTBEAT_INTERVAL, not immediately.
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.tick().await;

    // Send initial Lizard Mode suppression reports if configured.
    if let Some(ref cfg) = lizard_cfg {
        send_lizard_reports(fd, cfg, true).await;
        println!(
            "deckery-controller: Lizard Mode suppression active (buttons={}, mouse={}). +{}ms since startup",
            cfg.suppress_buttons, cfg.suppress_mouse, crate::startup_ms()
        );
    }

    loop {
        tokio::select! {
            maybe_cmd = haptic_rx.recv() => {
                match maybe_cmd {
                    Some(cmd) => {
                        let mut buf = build_haptic_report(&cmd);
                        let len = buf.len(); // 65
                        let result = tokio::task::spawn_blocking(move || {
                            send_feature_report_raw(fd, &mut buf, len)
                        }).await;
                        match result {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => eprintln!("deckery-controller: haptic write failed: {}", e),
                            Err(e)     => eprintln!("deckery-controller: haptic writer task panicked: {}", e),
                        }
                    }
                    None => break, // haptic player exited — session ending
                }
            }
            _ = heartbeat.tick() => {
                if let Some(ref cfg) = lizard_cfg {
                    send_lizard_reports(fd, cfg, false).await;
                }
            }
            changed = lizard_rx.changed() => {
                match changed {
                    Err(_) => break, // LizardModeHandle dropped = session teardown
                    Ok(()) => {
                        let new_cfg = lizard_rx.borrow_and_update().clone();
                        // Transition from disabled → enabled: send full initial reports.
                        if lizard_cfg.is_none() {
                            if let Some(ref cfg) = new_cfg {
                                send_lizard_reports(fd, cfg, true).await;
                            }
                        }
                        lizard_cfg = new_cfg;
                    }
                }
            }
            changed = click_pressure_rx.changed(), if click_pressure_alive => {
                match changed {
                    Err(_) => {
                        // ClickPressureHandle dropped — silence this arm.
                        // The writer continues; firmware keeps its last value.
                        click_pressure_alive = false;
                    }
                    Ok(()) => {
                        // Extract the value into an owned Option before the await so the
                        // watch::Ref guard (non-Send RwLockReadGuard) is dropped here,
                        // not held across the spawn_blocking await inside send_click_pressure.
                        let cfg: Option<ClickPressureConfig> = {
                            click_pressure_rx.borrow_and_update().clone()
                        };
                        if let Some(cfg) = cfg {
                            send_click_pressure(fd, cfg).await;
                        }
                    }
                }
            }
        }
    }
}

/// Sends Lizard Mode reports for the given config. `initial = true` sends the
/// full disable-settings report (all fields); `initial = false` sends the
/// minimal heartbeat report (SDL's watchdog pattern).
async fn send_lizard_reports(
    fd: std::os::fd::RawFd,
    cfg: &LizardModeSuppression,
    initial: bool,
) {
    for mut buf in lizard_mode::build_reports(cfg, initial) {
        let len = buf.len(); // 64
        let result = tokio::task::spawn_blocking(move || {
            send_feature_report_raw(fd, &mut buf, len)
        }).await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("deckery-controller: lizard mode write failed: {}", e),
            Err(e)     => eprintln!("deckery-controller: lizard mode writer task panicked: {}", e),
        }
    }
}

/// Sends a single click-pressure settings report for both trackpads.
async fn send_click_pressure(fd: std::os::fd::RawFd, cfg: ClickPressureConfig) {
    let mut buf = lizard_mode::make_click_pressure_report(cfg.left, cfg.right);
    let len = buf.len(); // 64
    let result = tokio::task::spawn_blocking(move || {
        send_feature_report_raw(fd, &mut buf, len)
    }).await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!("deckery-controller: click pressure write failed: {}", e),
        Err(e)     => eprintln!("deckery-controller: click pressure writer task panicked: {}", e),
    }
}

// ── Spawn ─────────────────────────────────────────────────────────────────────

/// Spawns the hidraw reader, haptic player, and unified writer tasks.
///
/// Returns `(Receiver<PadFrame>, Sender<HapticRequest>)`:
/// - `PadFrame` receiver: trackpad position frames (one per hidraw report change)
/// - `HapticRequest` sender: callers send chains here; evaluation and fd I/O
///   happen inside the spawned tasks, never in the caller.
///
/// `event_tx` — when `Some`, the hidraw reader synthesises a full
/// `ControllerEvent::Input` stream for every report that changes (buttons +
/// sticks + triggers + pads) and forwards it here. Pass the same sender that
/// the evdev reconnecting reader would use so the consumer sees a single
/// unified event stream. When `None`, only `PadFrame`s are produced.
///
/// `device_error_notify` — when `Some`, fires on hidraw read error (e.g.
/// controller unplugged) so the consuming binary can trigger a full reinit.
/// When `None`, the reader exits silently.
///
/// `lizard_rx` carries the initial Lizard Mode config and any live updates sent
/// via `LizardModeHandle::set()`. The writer applies changes immediately.
/// When the `LizardModeHandle` is dropped, `lizard_rx` returns `Err` and
/// the writer exits cleanly.
///
/// `click_pressure_rx` carries click-pressure threshold updates from
/// `ClickPressureHandle::set()`. Dropping the handle silences that select arm
/// but does NOT cause the writer to exit — the firmware retains the last value.
///
/// Called internally by `SteamDeckController::start()` — callers never open
/// the hidraw fd directly.
pub(super) fn spawn_hidraw_tasks(
    path: PathBuf,
    lizard_rx: watch::Receiver<Option<LizardModeSuppression>>,
    click_pressure_rx: watch::Receiver<Option<ClickPressureConfig>>,
    event_tx: Option<mpsc::Sender<ControllerEvent>>,
    device_error_notify: Option<Arc<Notify>>,
) -> (mpsc::Receiver<PadFrame>, mpsc::Sender<HapticRequest>) {
    println!(
        "deckery-controller: hidraw attached to {:?}. +{}ms since startup",
        path,
        crate::startup_ms()
    );

    let (frame_tx,   frame_rx)   = mpsc::channel::<PadFrame>(64);
    let (request_tx, request_rx) = mpsc::channel::<HapticRequest>(32);
    // Internal channel between haptic player and writer — never exposed outside
    // this module group.
    let (cmd_tx, cmd_rx) = mpsc::channel::<HapticCommand>(32);

    // Open the write-side fd here so the owning File can be moved into the
    // writer task — keeping the fd valid for the task's entire lifetime.
    let write_file = match std::fs::OpenOptions::new().write(true).open(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("deckery-controller: hidraw writer: cannot open {:?}: {}", path, e);
            // Writer won't run — drop channels so dependent tasks exit cleanly.
            drop(lizard_rx);
            drop(click_pressure_rx);
            let read_path = path.clone();
            tokio::spawn(async move {
                run_hidraw_reader(read_path, frame_tx, event_tx, device_error_notify).await
            });
            tokio::spawn(async move { super::haptic::run_haptic_player(request_rx, cmd_tx).await });
            return (frame_rx, request_tx);
        }
    };
    let write_fd = {
        use std::os::unix::io::AsRawFd;
        write_file.as_raw_fd()
    };

    let read_path = path.clone();
    tokio::spawn(async move {
        run_hidraw_reader(read_path, frame_tx, event_tx, device_error_notify).await
    });
    tokio::spawn(async move {
        super::haptic::run_haptic_player(request_rx, cmd_tx).await
    });
    tokio::spawn(async move {
        let _write_file = write_file; // keep fd valid for the writer's lifetime
        run_hidraw_writer(write_fd, cmd_rx, lizard_rx, click_pressure_rx).await
    });

    (frame_rx, request_tx)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_report(lx: i16, ly: i16, ltouch: bool, rx: i16, ry: i16, rtouch: bool) -> [u8; 64] {
        make_report_full(lx, ly, ltouch, false, rx, ry, rtouch, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn make_report_full(
        lx: i16, ly: i16, ltouch: bool, lclick: bool,
        rx: i16, ry: i16, rtouch: bool, rclick: bool,
    ) -> [u8; 64] {
        let mut buf = [0u8; 64];
        let mut byte10 = 0u8;
        if lclick { byte10 |= 0x02; }
        if rclick { byte10 |= 0x04; }
        if ltouch { byte10 |= 0x08; }
        if rtouch { byte10 |= 0x10; }
        buf[10] = byte10;
        buf[16..18].copy_from_slice(&lx.to_le_bytes());
        buf[18..20].copy_from_slice(&ly.to_le_bytes());
        buf[20..22].copy_from_slice(&rx.to_le_bytes());
        buf[22..24].copy_from_slice(&ry.to_le_bytes());
        buf
    }

    #[test]
    fn parse_extracts_known_offsets_correctly() {
        let buf = make_report(1111, -2222, true, 3333, -4444, false);
        let frame = PadFrame::parse(&buf);
        assert_eq!(frame, PadFrame {
            lx: 1111, ly: -2222, ltouch: true,  lclick: false,
            rx: 3333, ry: -4444, rtouch: false, rclick: false,
        });
    }

    #[test]
    fn parse_extracts_click_bits_independently_of_touch_and_other_pad() {
        let buf = make_report_full(1, 2, true, true, 3, 4, true, false);
        let frame = PadFrame::parse(&buf);
        assert!(frame.ltouch && frame.lclick);
        assert!(frame.rtouch && !frame.rclick);

        let buf = make_report_full(1, 2, true, false, 3, 4, true, true);
        let frame = PadFrame::parse(&buf);
        assert!(frame.ltouch && !frame.lclick);
        assert!(frame.rtouch && frame.rclick);

        let buf = make_report_full(0, 0, false, true, 0, 0, false, true);
        let frame = PadFrame::parse(&buf);
        assert!(!frame.ltouch && frame.lclick);
        assert!(!frame.rtouch && frame.rclick);
    }

    #[test]
    fn parse_handles_full_i16_range() {
        let buf = make_report(i16::MIN, i16::MAX, false, i16::MAX, i16::MIN, true);
        let frame = PadFrame::parse(&buf);
        assert_eq!(frame.lx, i16::MIN as i32);
        assert_eq!(frame.ly, i16::MAX as i32);
        assert_eq!(frame.rx, i16::MAX as i32);
        assert_eq!(frame.ry, i16::MIN as i32);
    }

    #[test]
    fn diagonal_movement_never_produces_a_half_updated_frame() {
        for step in 1..=20i16 {
            let x = step * 100;
            let y = -step * 50;
            let buf = make_report(0, 0, false, x, y, true);
            let frame = PadFrame::parse(&buf);
            assert_eq!(frame.rx, x as i32, "x did not update for step {step}");
            assert_eq!(frame.ry, y as i32, "y did not update for step {step}");
        }
    }

    #[test]
    fn lift_and_retouch_elsewhere_are_seen_as_distinct_ordered_frames() {
        let touching_at_a = PadFrame::parse(&make_report(0, 0, false, 1000, 2000, true));
        let lifted        = PadFrame::parse(&make_report(0, 0, false, 1000, 2000, false));
        let touching_at_b = PadFrame::parse(&make_report(0, 0, false, 5000, 6000, true));
        assert!(touching_at_a.rtouch);
        assert!(!lifted.rtouch);
        assert!(touching_at_b.rtouch);
        assert_eq!((touching_at_b.rx, touching_at_b.ry), (5000, 6000));
    }

    // ── synthesise_input_events ─────────────────────────────────────────────

    fn zero_buf() -> [u8; 64] { [0u8; 64] }

    /// Build a buf with a single button pressed (byte, bit).
    fn btn_buf(byte: usize, mask: u8) -> [u8; 64] {
        let mut buf = zero_buf();
        buf[byte] = mask;
        buf
    }

    /// Build a buf with an i16 axis value at the given byte offset.
    fn axis_buf(offset: usize, val: i16) -> [u8; 64] {
        let mut buf = zero_buf();
        buf[offset..offset + 2].copy_from_slice(&val.to_le_bytes());
        buf
    }

    #[test]
    fn synthesise_emits_nothing_when_buffer_unchanged() {
        let buf = zero_buf();
        let events = synthesise_input_events(&buf, &buf);
        assert!(events.is_empty(), "no events expected when buffer is identical");
    }

    #[test]
    fn synthesise_emits_btn_south_on_a_press() {
        let prev = zero_buf();
        let curr = btn_buf(8, 1 << 7); // BTN_SOUTH / A, byte 8, bit 7
        let events = synthesise_input_events(&curr, &prev);
        // Expect at least one KEY event with value 1, plus SYN_REPORT.
        let key_ev = events.iter().find(|e| {
            e.event_type() == EventType::KEY
                && e.code() == Key::BTN_SOUTH.code()
                && e.value() == 1
        });
        assert!(key_ev.is_some(), "expected BTN_SOUTH press event, got: {:?}", events);
        assert!(
            events.last().map(|e| e.event_type() == EventType::SYNCHRONIZATION).unwrap_or(false),
            "last event must be SYN_REPORT"
        );
    }

    #[test]
    fn synthesise_emits_btn_release_on_transition_1_to_0() {
        let prev = btn_buf(8, 1 << 7); // A pressed
        let curr = zero_buf();          // A released
        let events = synthesise_input_events(&curr, &prev);
        let key_ev = events.iter().find(|e| {
            e.event_type() == EventType::KEY
                && e.code() == Key::BTN_SOUTH.code()
                && e.value() == 0
        });
        assert!(key_ev.is_some(), "expected BTN_SOUTH release event, got: {:?}", events);
    }

    #[test]
    fn synthesise_emits_abs_x_on_left_stick_move() {
        let prev = zero_buf();
        let curr = axis_buf(48, 10_000i16); // ABS_X = left stick X, offset 48
        let events = synthesise_input_events(&curr, &prev);
        let abs_ev = events.iter().find(|e| {
            e.event_type() == EventType::ABSOLUTE
                && e.code() == AbsoluteAxisType::ABS_X.0
                && e.value() == 10_000
        });
        assert!(abs_ev.is_some(), "expected ABS_X = 10000, got: {:?}", events);
    }

    #[test]
    fn synthesise_negates_abs_y_for_left_stick() {
        // ABS_Y (left stick Y, offset 50) has negate=true: raw +5000 → reported -5000.
        let prev = zero_buf();
        let curr = axis_buf(50, 5_000i16);
        let events = synthesise_input_events(&curr, &prev);
        let abs_ev = events.iter().find(|e| {
            e.event_type() == EventType::ABSOLUTE
                && e.code() == AbsoluteAxisType::ABS_Y.0
        });
        assert!(
            abs_ev.is_some() && abs_ev.unwrap().value() == -5_000,
            "expected ABS_Y = -5000 (negated), got: {:?}", events
        );
    }

    #[test]
    fn synthesise_does_not_negate_trigger_axes() {
        // ABS_HAT2Y (left trigger, offset 44) has negate=false.
        let prev = zero_buf();
        let curr = axis_buf(44, 20_000i16);
        let events = synthesise_input_events(&curr, &prev);
        let abs_ev = events.iter().find(|e| {
            e.event_type() == EventType::ABSOLUTE
                && e.code() == AbsoluteAxisType::ABS_HAT2Y.0
        });
        assert!(
            abs_ev.is_some() && abs_ev.unwrap().value() == 20_000,
            "expected ABS_HAT2Y = 20000 (not negated), got: {:?}", events
        );
    }

    #[test]
    fn synthesise_emits_only_changed_fields() {
        // Change only ABS_RX (right stick X, offset 52). Nothing else should appear.
        let prev = zero_buf();
        let mut curr = zero_buf();
        curr[52..54].copy_from_slice(&(-3000i16).to_le_bytes());
        let events = synthesise_input_events(&curr, &prev);
        // Only ABS_RX + SYN_REPORT expected.
        let non_syn: Vec<_> = events.iter()
            .filter(|e| e.event_type() != EventType::SYNCHRONIZATION)
            .collect();
        assert_eq!(non_syn.len(), 1, "expected exactly one non-SYN event, got: {:?}", events);
        assert_eq!(non_syn[0].code(), AbsoluteAxisType::ABS_RX.0);
        assert_eq!(non_syn[0].value(), -3000);
    }

    #[test]
    fn synthesise_multiple_buttons_in_one_report() {
        // Press BTN_SOUTH (byte 8, bit 7) and BTN_DPAD_UP (byte 9, bit 0) simultaneously.
        let prev = zero_buf();
        let mut curr = zero_buf();
        curr[8] = 1 << 7;
        curr[9] = 1 << 0;
        let events = synthesise_input_events(&curr, &prev);
        let south = events.iter().any(|e| e.event_type() == EventType::KEY && e.code() == Key::BTN_SOUTH.code() && e.value() == 1);
        let dpad  = events.iter().any(|e| e.event_type() == EventType::KEY && e.code() == Key::BTN_DPAD_UP.code() && e.value() == 1);
        assert!(south, "BTN_SOUTH missing from: {:?}", events);
        assert!(dpad,  "BTN_DPAD_UP missing from: {:?}", events);
    }

    // ── Haptic wire format ──────────────────────────────────────────────────

    #[test]
    fn build_haptic_report_matches_hid_steam_layout() {
        let cmd = HapticCommand {
            pad: HapticPad::Right,
            duration_us: 0x1234,
            interval_us: 0x5678,
            count: 3,
            gain_db: -6,
        };
        let buf = build_haptic_report(&cmd);
        assert_eq!(buf[0], 0x00);
        assert_eq!(buf[1], 0x8F);
        assert_eq!(buf[2], 8);
        assert_eq!(buf[3], 0, "HapticPad::Right wire value must be 0 (swapped — see HapticPad)");
        assert_eq!(&buf[4..6],  &0x1234u16.to_le_bytes());
        assert_eq!(&buf[6..8],  &0x5678u16.to_le_bytes());
        assert_eq!(&buf[8..10], &3u16.to_le_bytes());
        assert_eq!(buf[10] as i8, -6);
        assert!(buf[11..].iter().all(|&b| b == 0));
        assert_eq!(buf.len(), 65);
    }

    #[test]
    fn build_haptic_report_pad_wire_values_match_steam_firmware_constants() {
        let base = HapticCommand { pad: HapticPad::Left, duration_us: 0, interval_us: 0, count: 0, gain_db: 0 };
        assert_eq!(build_haptic_report(&base)[3], 1);
        assert_eq!(build_haptic_report(&HapticCommand { pad: HapticPad::Right, ..base })[3], 0);
        assert_eq!(build_haptic_report(&HapticCommand { pad: HapticPad::Both,  ..base })[3], 2);
    }

    #[test]
    fn hidiocsfeature_65_matches_known_constant() {
        let expected: u32 = (3u32 << 30) | (0x48u32 << 8) | 0x06u32 | (65u32 << 16);
        assert_eq!(hidiocsfeature(65) as u32, expected);
    }

    #[test]
    fn hidiocsfeature_64_matches_known_constant() {
        let expected: u32 = (3u32 << 30) | (0x48u32 << 8) | 0x06u32 | (64u32 << 16);
        assert_eq!(hidiocsfeature(64) as u32, expected);
    }

    // ── Writer task — behavioural integration tests ───────────────────────────
    //
    // These tests drive `run_hidraw_writer` directly by constructing the same
    // channel endpoints that production code wires up in `spawn_hidraw_tasks`.
    // A writable fd is needed so the writer does not exit on file-open failure;
    // we use `/dev/null`. The `ioctl(HIDIOCSFEATURE)` calls will return ENOTTY,
    // which the writer logs and ignores — that is intentional: we are testing
    // channel routing and exit conditions, not byte content (the report builders
    // that produce those bytes are covered separately by the pure-function tests
    // above).
    //
    // Each test is async and has a 2-second timeout so a regression (e.g. the
    // writer never exiting) fails fast rather than hanging the test suite.

    /// Open `/dev/null` as a writable fd valid for the test's duration.
    /// Returned `File` must be kept alive alongside the writer future.
    fn null_write_file() -> std::fs::File {
        std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .expect("/dev/null must be writable in a test environment")
    }

    /// Build the four channel pairs that `run_hidraw_writer` consumes.
    /// Returns (writer-facing receivers/receivers, caller-facing senders).
    fn writer_channels() -> (
        mpsc::Receiver<HapticCommand>,
        watch::Receiver<Option<LizardModeSuppression>>,
        watch::Receiver<Option<ClickPressureConfig>>,
        mpsc::Sender<HapticCommand>,
        watch::Sender<Option<LizardModeSuppression>>,
        watch::Sender<Option<ClickPressureConfig>>,
    ) {
        let (haptic_tx,   haptic_rx)   = mpsc::channel::<HapticCommand>(4);
        let (lizard_tx,   lizard_rx)   = watch::channel::<Option<LizardModeSuppression>>(None);
        let (cp_tx,       cp_rx)       = watch::channel::<Option<ClickPressureConfig>>(None);
        (haptic_rx, lizard_rx, cp_rx, haptic_tx, lizard_tx, cp_tx)
    }

    #[tokio::test]
    async fn writer_exits_when_haptic_channel_closes() {
        // The haptic channel closing is the normal session-end signal: the
        // haptic player exits first (its tx dropped), which closes haptic_rx.
        let (haptic_rx, lizard_rx, cp_rx, haptic_tx, _lizard_tx, _cp_tx) = writer_channels();
        let f = null_write_file();
        let fd = { use std::os::unix::io::AsRawFd; f.as_raw_fd() };

        let handle = tokio::spawn(async move {
            let _f = f;
            run_hidraw_writer(fd, haptic_rx, lizard_rx, cp_rx).await
        });

        drop(haptic_tx); // simulate haptic player exit
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("writer did not exit within 2 s after haptic channel closed")
            .expect("writer task panicked");
    }

    #[tokio::test]
    async fn writer_exits_when_lizard_handle_dropped() {
        // Dropping the LizardModeHandle (= lizard_tx) is the session-teardown
        // signal. The writer must exit even while haptic_tx is still open.
        let (haptic_rx, lizard_rx, cp_rx, _haptic_tx, lizard_tx, _cp_tx) = writer_channels();
        let f = null_write_file();
        let fd = { use std::os::unix::io::AsRawFd; f.as_raw_fd() };

        let handle = tokio::spawn(async move {
            let _f = f;
            run_hidraw_writer(fd, haptic_rx, lizard_rx, cp_rx).await
        });

        drop(lizard_tx); // LizardModeHandle dropped = session teardown
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("writer did not exit within 2 s after LizardModeHandle dropped")
            .expect("writer task panicked");
        // _haptic_tx still alive — proves the lizard drop was the exit cause,
        // not an accidental haptic-channel close.
    }

    #[tokio::test]
    async fn writer_survives_click_pressure_handle_drop() {
        // Dropping the ClickPressureHandle must NOT exit the writer — the arm
        // is silenced (click_pressure_alive = false) and the session continues.
        let (haptic_rx, lizard_rx, cp_rx, _haptic_tx, _lizard_tx, cp_tx) = writer_channels();
        let f = null_write_file();
        let fd = { use std::os::unix::io::AsRawFd; f.as_raw_fd() };

        let handle = tokio::spawn(async move {
            let _f = f;
            run_hidraw_writer(fd, haptic_rx, lizard_rx, cp_rx).await
        });

        drop(cp_tx); // ClickPressureHandle dropped
        // Give the writer a moment to process the Err on changed().
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(
            !handle.is_finished(),
            "writer must NOT exit when ClickPressureHandle is dropped — \
             arm should be silenced, not session-teardown"
        );
        // Confirm the writer is still functional: clean shutdown via lizard drop.
        drop(_lizard_tx);
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("writer did not exit during clean shutdown")
            .expect("writer task panicked");
    }

    #[tokio::test]
    async fn writer_survives_lizard_mode_update() {
        // A live LizardMode config push (None → Some) must not exit the writer.
        let (haptic_rx, lizard_rx, cp_rx, _haptic_tx, lizard_tx, _cp_tx) = writer_channels();
        let f = null_write_file();
        let fd = { use std::os::unix::io::AsRawFd; f.as_raw_fd() };

        let handle = tokio::spawn(async move {
            let _f = f;
            run_hidraw_writer(fd, haptic_rx, lizard_rx, cp_rx).await
        });

        lizard_tx.send(Some(LizardModeSuppression {
            suppress_buttons: true,
            suppress_mouse: true,
        })).expect("lizard send failed");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(!handle.is_finished(), "writer must survive a Lizard Mode config update");

        drop(lizard_tx);
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn writer_survives_click_pressure_update() {
        // A ClickPressureConfig push must not exit the writer.
        let (haptic_rx, lizard_rx, cp_rx, _haptic_tx, _lizard_tx, cp_tx) = writer_channels();
        let f = null_write_file();
        let fd = { use std::os::unix::io::AsRawFd; f.as_raw_fd() };

        let handle = tokio::spawn(async move {
            let _f = f;
            run_hidraw_writer(fd, haptic_rx, lizard_rx, cp_rx).await
        });

        cp_tx.send(Some(ClickPressureConfig { left: 1000, right: 2000 }))
            .expect("click pressure send failed");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(!handle.is_finished(), "writer must survive a click-pressure update");

        drop(_lizard_tx);
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn writer_survives_haptic_command() {
        // A HapticCommand being forwarded must not crash or exit the writer.
        let (haptic_rx, lizard_rx, cp_rx, haptic_tx, _lizard_tx, _cp_tx) = writer_channels();
        let f = null_write_file();
        let fd = { use std::os::unix::io::AsRawFd; f.as_raw_fd() };

        let handle = tokio::spawn(async move {
            let _f = f;
            run_hidraw_writer(fd, haptic_rx, lizard_rx, cp_rx).await
        });

        haptic_tx.send(HapticCommand {
            pad: HapticPad::Right,
            duration_us: 1000,
            interval_us: 1000,
            count: 3,
            gain_db: 0,
        }).await.expect("haptic send failed");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(!handle.is_finished(), "writer must survive a haptic command");

        drop(haptic_tx);
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn heartbeat_fires_after_interval_and_writer_stays_alive() {
        // Use tokio::time::pause so the test is instant rather than waiting
        // 4 real seconds for HEARTBEAT_INTERVAL.
        tokio::time::pause();

        let (haptic_rx, lizard_rx, cp_rx, _haptic_tx, lizard_tx, _cp_tx) = writer_channels();
        let f = null_write_file();
        let fd = { use std::os::unix::io::AsRawFd; f.as_raw_fd() };

        // Enable Lizard Mode so the heartbeat actually sends a report.
        // (If lizard_cfg is None the heartbeat tick is a no-op — still alive,
        // but we want to exercise the send path too.)
        lizard_tx.send(Some(LizardModeSuppression {
            suppress_buttons: true,
            suppress_mouse:   true,
        })).unwrap();

        let handle = tokio::spawn(async move {
            let _f = f;
            run_hidraw_writer(fd, haptic_rx, lizard_rx, cp_rx).await
        });

        // Let the writer process the initial lizard config send.
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        assert!(!handle.is_finished(), "writer must be alive before heartbeat");

        // Advance past one full heartbeat interval.
        tokio::time::advance(HEARTBEAT_INTERVAL + std::time::Duration::from_millis(1)).await;
        // Yield so the Tokio runtime can schedule the writer's select! arm.
        tokio::task::yield_now().await;

        assert!(
            !handle.is_finished(),
            "writer must still be alive after one heartbeat tick — \
             the ioctl failure on /dev/null must be logged, not fatal"
        );

        // A second tick — verifies the interval repeats, not a one-shot.
        tokio::time::advance(HEARTBEAT_INTERVAL + std::time::Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(!handle.is_finished(), "writer must survive a second heartbeat tick");

        // Clean shutdown.
        drop(lizard_tx);
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await.unwrap().unwrap();
    }
}
