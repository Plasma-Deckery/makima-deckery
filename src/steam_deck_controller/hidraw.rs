//! Raw hidraw I/O for the Steam Deck controller.
//!
//! Owns both directions of the hidraw channel:
//!
//! - **Reader** (`run_hidraw_reader`): streams `PadFrame`s parsed from
//!   64-byte HID input reports. Position and touch state arrive atomically
//!   from the same `read()` — no ordering hazard possible.
//!
//! - **Writer** (`run_hidraw_writer`): serializes all outbound HID feature
//!   reports (haptic pulses, Lizard Mode heartbeats, future firmware settings)
//!   through a single `mpsc` channel onto one file descriptor. One fd, one
//!   writer, no concurrent-ioctl races.
//!
//! Callers receive `(Receiver<PadFrame>, Sender<HidrawWrite>)` from
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

use std::path::PathBuf;
use tokio::sync::mpsc;

// ── PadFrame ─────────────────────────────────────────────────────────────────

/// One consistent snapshot of both trackpads, parsed from a single hidraw
/// report. Because both pads' position and touch state come from the same
/// 64-byte `read()`, this is atomic — there is no way for RPAD's position and
/// touch bit to reflect different points in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PadFrame {
    pub lx: i32,
    pub ly: i32,
    pub ltouch: bool,
    pub lclick: bool,
    pub rx: i32,
    pub ry: i32,
    pub rtouch: bool,
    pub rclick: bool,
}

impl PadFrame {
    fn parse(buf: &[u8; 64]) -> Self {
        let byte10 = buf[10];
        Self {
            lx: i16::from_le_bytes([buf[16], buf[17]]) as i32,
            ly: i16::from_le_bytes([buf[18], buf[19]]) as i32,
            ltouch: (byte10 & 0x08) != 0,
            // Click (physical press-through) lives in the same byte as touch —
            // confirmed against the upstream hid-steam kernel driver
            // (steam_do_deck_input_event): BTN_THUMB = b10 & BIT(1), BTN_THUMB2
            // = b10 & BIT(2). Reading both from the same byte makes click
            // atomic with position/touch — no evdev ordering hazard.
            lclick: (byte10 & 0x02) != 0,
            rx: i16::from_le_bytes([buf[20], buf[21]]) as i32,
            ry: i16::from_le_bytes([buf[22], buf[23]]) as i32,
            rtouch: (byte10 & 0x10) != 0,
            rclick: (byte10 & 0x04) != 0,
        }
    }
}

// ── HapticPad ────────────────────────────────────────────────────────────────

/// Which pad(s) a haptic pulse should play on.
///
/// Wire values are swapped relative to the hid-steam.c constant names
/// (`STEAM_PAD_LEFT`/`RIGHT` = 0/1): on-hardware testing (2026-07-09) showed
/// pressing the right pad buzzing the left actuator with the "obvious" values,
/// so `Left → 1` and `Right → 0`. `Both` is unaffected.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HapticPad {
    Left,
    Right,
    Both,
}

impl HapticPad {
    fn wire_value(self) -> u8 {
        match self {
            HapticPad::Left => 1,
            HapticPad::Right => 0,
            HapticPad::Both => 2,
        }
    }
}

// ── HapticCommand ────────────────────────────────────────────────────────────

/// One haptic "rumble" pulse to send to the trackpad's linear resonant
/// actuator via the `ID_TRIGGER_HAPTIC_PULSE` (0x8F) HID feature report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HapticCommand {
    pub pad: HapticPad,
    /// Pulse duration in microseconds.
    pub duration_us: u16,
    /// Time between pulses in microseconds (only matters when `count > 1`).
    pub interval_us: u16,
    /// Number of pulses to fire.
    pub count: u16,
    /// Gain in decibels — roughly -24 (quiet) to +6 (loud) per the firmware.
    pub gain_db: i8,
}

// ── HidrawWrite ──────────────────────────────────────────────────────────────

/// A command to be serialized onto the hidraw file descriptor by the writer
/// task.
///
/// External callers construct only `Haptic`. The `LizardReport` variant is an
/// internal detail used by the lizard mode heartbeat task inside
/// `steam_deck_controller` and is not part of the stable public API.
pub enum HidrawWrite {
    /// A haptic pulse sent via `HIDIOCSFEATURE(65)`.
    Haptic(HapticCommand),
    /// A raw 64-byte settings/Lizard Mode report sent via `HIDIOCSFEATURE(64)`.
    /// Internal use by `lizard_mode` only — callers configure Lizard Mode via
    /// `ControllerSession::lizard_tx` instead.
    #[doc(hidden)]
    LizardReport([u8; 64]),
}

// ── Haptic wire format ────────────────────────────────────────────────────────

/// HID feature report ID for `ID_TRIGGER_HAPTIC_PULSE`, straight from
/// upstream `hid-steam.c`.
const ID_TRIGGER_HAPTIC_PULSE: u8 = 0x8F;

/// Serializes a `HapticCommand` into the exact 65-byte HID feature report
/// buffer the kernel's `hid-steam` driver sends over USB/BT.
///
/// Layout (from `steam_send_report`/`steam_haptic_pulse` in hid-steam.c):
///   buf[0]    = 0x00           report ID (always 0)
///   buf[1]    = 0x8F           ID_TRIGGER_HAPTIC_PULSE
///   buf[2]    = 0x08           payload length (8 bytes follow)
///   buf[3]    = pad            0=right, 1=left, 2=both (swapped — see HapticPad)
///   buf[4..6] = duration (u16 LE, microseconds)
///   buf[6..8] = interval (u16 LE, microseconds)
///   buf[8..10]= count    (u16 LE, pulses)
///   buf[10]   = gain (i8, dB)
///   buf[11..65] = 0 padding
///
/// Pure and I/O-free — the wire format is unit-testable without a real device.
pub fn build_haptic_report(cmd: &HapticCommand) -> [u8; 65] {
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
    const IOC_NRSHIFT: u32 = 0;
    const IOC_TYPESHIFT: u32 = IOC_NRSHIFT + 8;
    const IOC_SIZESHIFT: u32 = IOC_TYPESHIFT + 8;
    const IOC_DIRSHIFT: u32 = IOC_SIZESHIFT + 14;
    const IOC_WRITE: u32 = 1;
    const IOC_READ: u32 = 2;
    let dir = IOC_WRITE | IOC_READ;
    let ty = b'H' as u32;
    let nr = 0x06u32;
    let size = len as u32;
    ((dir << IOC_DIRSHIFT) | (ty << IOC_TYPESHIFT) | (nr << IOC_NRSHIFT) | (size << IOC_SIZESHIFT))
        as libc::c_ulong
}

/// Sends a feature report buffer to an open hidraw fd via `HIDIOCSFEATURE`.
///
/// Blocking syscall — callers in async context use `spawn_blocking`.
fn send_feature_report_raw(fd: std::os::fd::RawFd, buf: &mut [u8], len: usize) -> std::io::Result<()> {
    let ret = unsafe {
        libc::ioctl(fd, hidiocsfeature(len) as _, buf.as_mut_ptr())
    };
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
    let mut buf = [0u8; 64];
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

/// Serializes all outbound hidraw writes onto a single file descriptor.
///
/// All writers — haptic commands from `mt_trackpad`/`gesture_pad`, Lizard Mode
/// heartbeats from `lizard_mode` — share one `Sender<HidrawWrite>`. This task
/// is the sole owner of the write fd and processes commands sequentially,
/// eliminating any possibility of concurrent-ioctl races between the old
/// separate `run_pad_haptic_writer` and `lizard_mode` file handles.
///
/// Exits silently once the channel closes or the device can't be opened.
pub async fn run_hidraw_writer(path: PathBuf, mut rx: mpsc::Receiver<HidrawWrite>) {
    use std::os::unix::io::AsRawFd;
    let file = match std::fs::OpenOptions::new().write(true).open(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("makima: hidraw writer: cannot open {:?}: {}", path, e);
            return;
        }
    };
    let fd = file.as_raw_fd();
    while let Some(write) = rx.recv().await {
        let result = match write {
            HidrawWrite::Haptic(cmd) => {
                let mut buf = build_haptic_report(&cmd);
                let len = buf.len(); // 65
                tokio::task::spawn_blocking(move || send_feature_report_raw(fd, &mut buf, len)).await
            }
            HidrawWrite::LizardReport(mut buf) => {
                let len = buf.len(); // 64
                tokio::task::spawn_blocking(move || send_feature_report_raw(fd, &mut buf, len)).await
            }
        };
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("makima: hidraw write failed: {}", e),
            Err(e) => eprintln!("makima: hidraw writer task panicked: {}", e),
        }
    }
}

// ── Spawn ─────────────────────────────────────────────────────────────────────

/// Spawns the hidraw reader and writer tasks for the given path.
///
/// Returns `(Receiver<PadFrame>, Sender<HidrawWrite>)` so the caller can
/// subscribe to trackpad frames and send haptic/settings commands.
///
/// Called internally by `SteamDeckController::start()` — callers never
/// open the hidraw fd directly.
pub(super) fn spawn_hidraw_tasks(
    path: PathBuf,
) -> (mpsc::Receiver<PadFrame>, mpsc::Sender<HidrawWrite>) {
    println!(
        "makima: hidraw attached to {:?}. +{}ms since startup",
        path,
        crate::startup_ms()
    );
    let (frame_tx, frame_rx) = mpsc::channel(64);
    let (write_tx, write_rx) = mpsc::channel::<HidrawWrite>(32);

    let read_path = path.clone();
    tokio::spawn(async move { run_hidraw_reader(read_path, frame_tx).await });
    tokio::spawn(async move { run_hidraw_writer(path, write_rx).await });

    (frame_rx, write_tx)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a synthetic 64-byte hidraw report with given RPAD/LPAD position
    /// and touch bits at the empirically-determined offsets.
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

    /// Regression: LPAD X/Y at 16/18, RPAD X/Y at 20/22, touch bits in byte
    /// 10 (0x08 = LPAD, 0x10 = RPAD). Pins the empirically-found offsets.
    #[test]
    fn parse_extracts_known_offsets_correctly() {
        let buf = make_report(1111, -2222, true, 3333, -4444, false);
        let frame = PadFrame::parse(&buf);
        assert_eq!(frame, PadFrame {
            lx: 1111, ly: -2222, ltouch: true, lclick: false,
            rx: 3333, ry: -4444, rtouch: false, rclick: false,
        });
    }

    /// Click bits (1/2) must not be confused with touch bits (3/4) or the
    /// other pad's bits.
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

    /// Negative i16 values must round-trip correctly through little-endian
    /// encoding.
    #[test]
    fn parse_handles_full_i16_range() {
        let buf = make_report(i16::MIN, i16::MAX, false, i16::MAX, i16::MIN, true);
        let frame = PadFrame::parse(&buf);
        assert_eq!(frame.lx, i16::MIN as i32);
        assert_eq!(frame.ly, i16::MAX as i32);
        assert_eq!(frame.rx, i16::MAX as i32);
        assert_eq!(frame.ry, i16::MIN as i32);
    }

    /// Diagonal drag: every frame has both axes from the same read() — no
    /// "staircase" intermediate state where only one axis updated.
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

    /// Lift and retouch elsewhere must be two distinct, ordered frames.
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
        assert_eq!(&buf[4..6], &0x1234u16.to_le_bytes());
        assert_eq!(&buf[6..8], &0x5678u16.to_le_bytes());
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

    /// `HIDIOCSFEATURE(65)` must match the known Linux constant.
    #[test]
    fn hidiocsfeature_65_matches_known_constant() {
        let expected: u32 = (3u32 << 30) | (0x48u32 << 8) | 0x06u32 | (65u32 << 16);
        assert_eq!(hidiocsfeature(65) as u32, expected);
    }

    /// `HIDIOCSFEATURE(64)` matches the constant used for settings/lizard reports.
    #[test]
    fn hidiocsfeature_64_matches_known_constant() {
        let expected: u32 = (3u32 << 30) | (0x48u32 << 8) | 0x06u32 | (64u32 << 16);
        assert_eq!(hidiocsfeature(64) as u32, expected);
    }
}
