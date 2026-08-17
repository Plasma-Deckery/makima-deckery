//! Raw hidraw I/O for the Steam Deck controller.
//!
//! Owns both directions of the hidraw channel:
//!
//! - **Reader** (`run_hidraw_reader`): streams `PadFrame`s parsed from
//!   64-byte HID input reports. Position and touch state arrive atomically
//!   from the same `read()` — no ordering hazard possible.
//!
//! - **Writer** (`run_hidraw_writer`): serialises all outbound HID feature
//!   reports onto one file descriptor, selected from two sources:
//!   - `HapticCommand`s forwarded by the haptic player task (from callers)
//!   - Lizard Mode heartbeat reports, driven by an internal timer +
//!     `watch::Receiver<Option<LizardModeSuppression>>`
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

use super::haptic::{HapticCommand, HapticPad, HapticRequest};
use super::lizard_mode::{self, LizardModeSuppression, HEARTBEAT_INTERVAL};
use std::path::PathBuf;
use tokio::sync::{mpsc, watch};

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
/// Exits silently on read error (e.g. device unplugged).
pub async fn run_hidraw_reader(path: PathBuf, tx: mpsc::Sender<PadFrame>) {
    use tokio::io::AsyncReadExt;
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) => {
            eprintln!("makima: hidraw reader: cannot open {:?}: {}", path, e);
            return;
        }
    };
    let mut reader = tokio::io::BufReader::new(file);
    let mut buf  = [0u8; 64];
    let mut last: Option<PadFrame> = None;
    loop {
        match reader.read_exact(&mut buf).await {
            Ok(_) => {
                let frame = PadFrame::parse(&buf);
                if last != Some(frame) {
                    last = Some(frame);
                    if tx.send(frame).await.is_err() {
                        return; // receiver dropped
                    }
                }
            }
            Err(_) => break,
        }
    }
}

// ── Writer task ───────────────────────────────────────────────────────────────

/// Serialises all outbound hidraw writes onto a single file descriptor.
///
/// Selects from two sources:
/// - `haptic_rx`: `HapticCommand`s forwarded by the haptic player task.
/// - Lizard Mode heartbeat: timer-driven, config read from `lizard_rx` watch.
///   On the initial write (or when config transitions from `None` to `Some`),
///   the full disable-settings report is sent. Heartbeat ticks send the
///   minimal watchdog report matching SDL's `FeedDeckLizardWatchdog`.
///
/// Exits when `haptic_rx` closes (haptic player exited → session ending) or
/// when `lizard_rx` sender is dropped (same signal).
async fn run_hidraw_writer(
    path: PathBuf,
    mut haptic_rx: mpsc::Receiver<HapticCommand>,
    mut lizard_rx: watch::Receiver<Option<LizardModeSuppression>>,
) {
    use std::os::unix::io::AsRawFd;
    let file = match std::fs::OpenOptions::new().write(true).open(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("makima: hidraw writer: cannot open {:?}: {}", path, e);
            return;
        }
    };
    let fd = file.as_raw_fd();

    let mut lizard_cfg = lizard_rx.borrow_and_update().clone();

    // Consume the timer's immediate first tick before entering the loop,
    // so the heartbeat fires after HEARTBEAT_INTERVAL, not immediately.
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.tick().await;

    // Send initial Lizard Mode suppression reports if configured.
    if let Some(ref cfg) = lizard_cfg {
        send_lizard_reports(fd, cfg, true).await;
        println!(
            "makima: Lizard Mode suppression active (buttons={}, mouse={}). +{}ms since startup",
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
                            Ok(Err(e)) => eprintln!("makima: haptic write failed: {}", e),
                            Err(e)     => eprintln!("makima: haptic writer task panicked: {}", e),
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
                if changed.is_err() { break; } // lizard_tx dropped — session ending
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
            Ok(Err(e)) => eprintln!("makima: lizard mode write failed: {}", e),
            Err(e)     => eprintln!("makima: lizard mode writer task panicked: {}", e),
        }
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
/// `lizard_rx` is consumed by the writer task — Lizard Mode config changes
/// are applied live without restarting any task.
///
/// Called internally by `SteamDeckController::start()` — callers never open
/// the hidraw fd directly.
pub(super) fn spawn_hidraw_tasks(
    path: PathBuf,
    lizard_rx: watch::Receiver<Option<LizardModeSuppression>>,
) -> (mpsc::Receiver<PadFrame>, mpsc::Sender<HapticRequest>) {
    println!(
        "makima: hidraw attached to {:?}. +{}ms since startup",
        path,
        crate::startup_ms()
    );

    let (frame_tx,   frame_rx)   = mpsc::channel::<PadFrame>(64);
    let (request_tx, request_rx) = mpsc::channel::<HapticRequest>(32);
    // Internal channel between haptic player and writer — never exposed outside
    // this module group.
    let (cmd_tx, cmd_rx) = mpsc::channel::<HapticCommand>(32);

    let read_path = path.clone();
    tokio::spawn(async move { run_hidraw_reader(read_path, frame_tx).await });
    tokio::spawn(async move { super::haptic::run_haptic_player(request_rx, cmd_tx).await });
    tokio::spawn(async move { run_hidraw_writer(path, cmd_rx, lizard_rx).await });

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
}
