//! Reads Steam Deck trackpad position AND touch state directly from the raw
//! hidraw HID report, instead of the evdev ABS_HAT0/1X/Y axes.
//!
//! Why: evdev (HAT0/1X/Y) and hidraw (touch bits, byte 10) are two different
//! HID interfaces of the same physical device (confirmed via sysfs — see
//! `find_hidraw_for_evdev`), read by two independent kernel/userspace paths.
//! Combining "position from evdev" with "touch from hidraw" meant the two
//! could arrive out of order relative to each other, causing large cursor
//! jumps on reposition. Reading both from the same 64-byte hidraw report
//! instead makes them atomic by construction — no ordering bug is possible,
//! because there is only one read() producing both values together.
//!
//! Byte offsets were determined empirically (2026-07-08) by recording evdev
//! and hidraw simultaneously with a shared monotonic clock and correlating
//! ABS_HAT0/1X/Y against every int16 offset in the raw report. Offsets match
//! exactly (median deviation ~19 out of a ~65500 range, explained by ~4ms
//! sampling-phase jitter, not a scaling/calibration difference) and hidraw
//! consistently leads evdev by about one report (~4ms) — evidence that the
//! evdev path goes through an extra translation layer in the hid-steam
//! kernel driver.
use std::path::PathBuf;
use tokio::sync::mpsc;

/// One consistent snapshot of both trackpads, parsed from a single hidraw
/// report. Because both pads' position and touch state come from the same
/// 64-byte read, this is atomic — there is no way for e.g. RPAD's position
/// and touch bit to reflect different points in time.
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
            // Click (physical press-through) lives in the exact same byte as
            // touch — confirmed against the upstream hid-steam kernel driver
            // (steam_do_deck_input_event): BTN_THUMB = b10 & BIT(1), BTN_THUMB2
            // = b10 & BIT(2). Reading it here too makes click atomic with
            // position/touch, same reasoning as the rest of this module —
            // instead of sourcing it from evdev BTN_THUMB/BTN_THUMB2 separately.
            lclick: (byte10 & 0x02) != 0,
            rx: i16::from_le_bytes([buf[20], buf[21]]) as i32,
            ry: i16::from_le_bytes([buf[22], buf[23]]) as i32,
            rtouch: (byte10 & 0x10) != 0,
            rclick: (byte10 & 0x04) != 0,
        }
    }
}

/// Given an evdev device path (e.g. `/dev/input/event5`), find the hidraw sibling
/// that belongs to the same physical device via sysfs.
///
/// Sysfs layout on Steam Deck:
///   evdev:   /sys/class/input/eventN/device  → …/usb_iface/HID_A/input/inputN
///   hidraw:  /sys/class/hidraw/hidrawN/device → …/usb_iface/HID_B
///
/// evdev and hidraw may belong to different HID functions (e.g. .0003 vs .0005)
/// of the same USB interface. We therefore go up to the shared USB interface
/// (three levels above inputN: input/ → HID_A/ → usb_iface/) and look for any
/// hidraw device whose sysfs path starts with that same USB interface path.
pub fn find_hidraw_for_evdev(evdev_path: &std::path::Path) -> Option<PathBuf> {
    let dev_name = evdev_path.file_name()?.to_str()?;
    let evdev_sysfs =
        std::fs::canonicalize(format!("/sys/class/input/{}/device", dev_name)).ok()?;
    // evdev_sysfs is …/usb_iface/HID_A/input/inputN
    // Go up three levels: inputN → input/ → HID_A/ → usb_iface/
    let usb_iface = evdev_sysfs.parent()?.parent()?.parent()?;
    for entry in std::fs::read_dir("/sys/class/hidraw/").ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name().to_string_lossy().to_string();
        if let Ok(hidraw_sysfs) =
            std::fs::canonicalize(format!("/sys/class/hidraw/{}/device", name))
        {
            // The hidraw device lives at …/usb_iface/HID_B — its parent is usb_iface.
            if hidraw_sysfs.parent() == Some(usb_iface) {
                return Some(PathBuf::from(format!("/dev/{}", name)));
            }
        }
    }
    None
}

/// Reads hidraw reports and sends a `PadFrame` through `tx` each time the
/// parsed frame differs from the last one sent. This is a plain mpsc channel
/// (not a `watch`) so every transition is delivered in order and none are
/// coalesced away, even if the consumer is momentarily busy — the failure
/// mode that caused the old touch-bit `watch` channel to silently lose fast
/// lift/retouch cycles.
///
/// Exits silently on read error (e.g. device unplugged).
pub async fn run_pad_hidraw_reader(path: PathBuf, tx: mpsc::Sender<PadFrame>) {
    use tokio::io::AsyncReadExt;
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) => {
            eprintln!("makima: pad hidraw reader: cannot open {:?}: {}", path, e);
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
                    eprintln!(
                        "[trackpad-debug] hidraw frame: ltouch={} lclick={} rtouch={} rclick={}",
                        frame.ltouch, frame.lclick, frame.rtouch, frame.rclick
                    );
                    if tx.send(frame).await.is_err() {
                        // Receiver dropped — nothing left to do.
                        return;
                    }
                }
            }
            Err(_) => break,
        }
    }
}

/// Which pad(s) a haptic pulse should play on — mirrors the Steam Deck's own
/// firmware constants (`STEAM_PAD_LEFT`/`RIGHT`/`BOTH` in the upstream
/// `hid-steam` kernel driver), not the `Pad` enum above: haptics additionally
/// need a "both" option that gesture-transition logic has no use for.
///
/// `Both` isn't constructed anywhere right now — the gesture-pad handler
/// (`gesture_pad.rs`) fires no click haptic at all (see its module doc) — but
/// stays available for when gesture-lifecycle haptics get wired up.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HapticPad {
    Left,
    Right,
    Both,
}

impl HapticPad {
    fn wire_value(self) -> u8 {
        // The kernel source names read as STEAM_PAD_LEFT=0/RIGHT=1/BOTH=2,
        // and that's what this returned originally — but on-hardware testing
        // (2026-07-09) showed pressing the *right* pad buzzing the *left*
        // actuator: with a real device in hand, 0 fires the right pad and 1
        // fires the left one. Swapped to match observed behaviour rather
        // than the assumption from reading hid-steam.c; "both" is unaffected.
        match self {
            HapticPad::Left => 1,
            HapticPad::Right => 0,
            HapticPad::Both => 2,
        }
    }
}

/// One haptic "rumble" pulse to send to the trackpad's linear resonant
/// actuator, matching the parameters of the Steam Deck firmware's
/// `ID_TRIGGER_HAPTIC_PULSE` (0x8F) HID feature report — see
/// `build_haptic_report` for the exact wire layout.
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

/// HID feature report ID for `ID_TRIGGER_HAPTIC_PULSE`, straight from
/// upstream `hid-steam.c`.
const ID_TRIGGER_HAPTIC_PULSE: u8 = 0x8F;

/// Serializes a `HapticCommand` into the exact 65-byte HID feature report
/// buffer the kernel's `hid-steam` driver sends over USB/BT, reproduced here
/// so we can send it directly via `HIDIOCSFEATURE` from userspace without
/// going through the kernel driver at all (hidraw bypasses hid-steam's
/// input-only interface).
///
/// Layout (from `steam_send_report`/`steam_haptic_pulse` in hid-steam.c):
///   buf[0]    = 0x00           report ID (always 0)
///   buf[1]    = 0x8F           ID_TRIGGER_HAPTIC_PULSE
///   buf[2]    = 0x08           payload length (8 bytes follow)
///   buf[3]    = pad            0=left, 1=right, 2=both
///   buf[4..6] = duration (u16 LE, microseconds)
///   buf[6..8] = interval (u16 LE, microseconds)
///   buf[8..10]= count    (u16 LE, pulses)
///   buf[10]   = gain (i8, dB)
///   buf[11..65] = 0 padding — `hid_hw_raw_request` is called with
///                 `max(size, 64) + 1` bytes regardless of payload length.
/// Pure and I/O-free so the wire format itself is unit-testable without a
/// real hidraw device.
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

/// Computes the `HIDIOCSFEATURE(len)` ioctl request number. Not exposed by
/// the `libc` crate (it's HID-specific, defined in `<linux/hid.h>`), so we
/// reproduce the standard Linux `_IOC(dir, type, nr, size)` macro by hand:
///   _IOC_WRITE | _IOC_READ = 3, type = 'H', nr = 0x06.
/// `len` is the full buffer size passed to the ioctl (65 here, including the
/// leading report-ID byte) — matches `#define HIDIOCSFEATURE(len) \
/// _IOC(_IOC_WRITE|_IOC_READ, 'H', 0x06, len)` in `<linux/hidraw.h>`.
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

/// Sends one already-serialized haptic feature report to an open hidraw fd
/// via `HIDIOCSFEATURE`. Blocking (it's a synchronous ioctl syscall) —
/// callers running in an async context should use `spawn_blocking`.
fn send_haptic_report(fd: std::os::fd::RawFd, buf: &mut [u8; 65]) -> std::io::Result<()> {
    let ret = unsafe {
        libc::ioctl(fd, hidiocsfeature(buf.len()) as _, buf.as_mut_ptr())
    };
    if ret < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Receives `HapticCommand`s over `rx` and writes each one out to `path` as
/// a HID feature report. Runs as its own task, entirely separate from the
/// read loop (`run_pad_hidraw_reader`) — the hidraw character device
/// supports independent read and write file descriptors, so haptics can be
/// fired without any coordination with the position/touch read path.
/// Exits silently once the channel closes or the device can't be opened.
pub async fn run_pad_haptic_writer(path: PathBuf, mut rx: mpsc::Receiver<HapticCommand>) {
    use std::os::unix::io::AsRawFd;
    let file = match std::fs::OpenOptions::new().write(true).open(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("makima: pad haptic writer: cannot open {:?}: {}", path, e);
            return;
        }
    };
    let fd = file.as_raw_fd();
    while let Some(cmd) = rx.recv().await {
        let mut buf = build_haptic_report(&cmd);
        // ioctl is a blocking syscall; run it off the async executor thread.
        let result = tokio::task::spawn_blocking(move || {
            send_haptic_report(fd, &mut buf)
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("makima: haptic pulse failed: {}", e),
            Err(e) => eprintln!("makima: haptic pulse task panicked: {}", e),
        }
    }
}

/// Spawns the pad hidraw reader task if a hidraw sibling is found for
/// `evdev_path`. Returns the receiving end of the position/touch channel it
/// feeds and the sending end of a haptic-command channel, or `None` if no
/// hidraw sibling exists (trackpad position/touch will then simply never
/// update — `MtTrackpad` mode requires hidraw, there is no evdev-based
/// fallback).
pub fn spawn(
    evdev_path: &std::path::Path,
) -> Option<(mpsc::Receiver<PadFrame>, mpsc::Sender<HapticCommand>)> {
    let hidraw_path = find_hidraw_for_evdev(evdev_path)?;
    println!("makima: pad hidraw reader attached to {:?}", hidraw_path);
    let (tx, rx) = mpsc::channel(64);
    let read_path = hidraw_path.clone();
    tokio::spawn(async move {
        run_pad_hidraw_reader(read_path, tx).await;
    });
    let (haptic_tx, haptic_rx) = mpsc::channel(16);
    tokio::spawn(async move {
        run_pad_haptic_writer(hidraw_path, haptic_rx).await;
    });
    Some((rx, haptic_tx))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a synthetic 64-byte hidraw report with the given RPAD/LPAD
    /// position and touch bits at the empirically-determined offsets
    /// (see module docs), everything else zeroed. Clicks default to false —
    /// use `make_report_full` when a test cares about click bits too.
    fn make_report(lx: i16, ly: i16, ltouch: bool, rx: i16, ry: i16, rtouch: bool) -> [u8; 64] {
        make_report_full(lx, ly, ltouch, false, rx, ry, rtouch, false)
    }

    /// Like `make_report`, but also sets the click bits (byte 10, bits 1/2 —
    /// see `PadFrame::parse` docs for why click lives in the same byte as
    /// touch).
    #[allow(clippy::too_many_arguments)]
    fn make_report_full(
        lx: i16,
        ly: i16,
        ltouch: bool,
        lclick: bool,
        rx: i16,
        ry: i16,
        rtouch: bool,
        rclick: bool,
    ) -> [u8; 64] {
        let mut buf = [0u8; 64];
        let mut byte10 = 0u8;
        if lclick {
            byte10 |= 0x02;
        }
        if rclick {
            byte10 |= 0x04;
        }
        if ltouch {
            byte10 |= 0x08;
        }
        if rtouch {
            byte10 |= 0x10;
        }
        buf[10] = byte10;
        buf[16..18].copy_from_slice(&lx.to_le_bytes());
        buf[18..20].copy_from_slice(&ly.to_le_bytes());
        buf[20..22].copy_from_slice(&rx.to_le_bytes());
        buf[22..24].copy_from_slice(&ry.to_le_bytes());
        buf
    }

    /// Regression test for the offsets found by correlating a real recorded
    /// trace (2026-07-08): LPAD X/Y at 16/18, RPAD X/Y at 20/22, touch bits
    /// in byte 10 (0x08 = LPAD, 0x10 = RPAD). If a future refactor gets an
    /// offset wrong, this fails immediately instead of silently producing
    /// garbage coordinates on real hardware.
    #[test]
    fn parse_extracts_known_offsets_correctly() {
        let buf = make_report(1111, -2222, true, 3333, -4444, false);
        let frame = PadFrame::parse(&buf);
        assert_eq!(
            frame,
            PadFrame {
                lx: 1111,
                ly: -2222,
                ltouch: true,
                lclick: false,
                rx: 3333,
                ry: -4444,
                rtouch: false,
                rclick: false,
            }
        );
    }

    /// Click lives in the same byte as touch (bits 1/2 vs. 3/4) — verify the
    /// two don't get confused with each other or with the other pad's bits.
    #[test]
    fn parse_extracts_click_bits_independently_of_touch_and_other_pad() {
        // Left touching+clicked, right touching but not clicked.
        let buf = make_report_full(1, 2, true, true, 3, 4, true, false);
        let frame = PadFrame::parse(&buf);
        assert!(frame.ltouch && frame.lclick);
        assert!(frame.rtouch && !frame.rclick);

        // Right touching+clicked, left touching but not clicked — the mirror
        // case, to catch a swapped bitmask.
        let buf = make_report_full(1, 2, true, false, 3, 4, true, true);
        let frame = PadFrame::parse(&buf);
        assert!(frame.ltouch && !frame.lclick);
        assert!(frame.rtouch && frame.rclick);

        // Click without touch must be possible in principle (the parser
        // shouldn't gate one bit on the other).
        let buf = make_report_full(0, 0, false, true, 0, 0, false, true);
        let frame = PadFrame::parse(&buf);
        assert!(!frame.ltouch && frame.lclick);
        assert!(!frame.rtouch && frame.rclick);
    }

    /// Negative i16 values (finger left/above center) must round-trip
    /// correctly through the little-endian byte encoding — a sign-handling
    /// bug here would look like a random cursor jump on real hardware.
    #[test]
    fn parse_handles_full_i16_range() {
        let buf = make_report(i16::MIN, i16::MAX, false, i16::MAX, i16::MIN, true);
        let frame = PadFrame::parse(&buf);
        assert_eq!(frame.lx, i16::MIN as i32);
        assert_eq!(frame.ly, i16::MAX as i32);
        assert_eq!(frame.rx, i16::MAX as i32);
        assert_eq!(frame.ry, i16::MIN as i32);
        assert!(frame.rtouch);
        assert!(!frame.ltouch);
    }

    /// Regression test for the "staircase diagonal" bug: on evdev, X and Y
    /// arrived as two separate SYN_REPORT frames, so an intermediate state
    /// with only one fresh axis genuinely existed and had to be coalesced in
    /// software. A hidraw report has no such intermediate state — X and Y for
    /// a pad are always two fields of the *same* read(), so every parsed
    /// frame is a complete, consistent (x, y) pair by construction. This test
    /// drives a diagonal drag (both axes changing every step, as real
    /// hardware does) and asserts every single step's frame already has both
    /// coordinates from the same point in time — there is no "wait for the
    /// other axis" step to get wrong.
    #[test]
    fn diagonal_movement_never_produces_a_half_updated_frame() {
        for step in 1..=20i16 {
            let x = step * 100;
            let y = -step * 50;
            let buf = make_report(0, 0, false, x, y, true);
            let frame = PadFrame::parse(&buf);
            // Both axes must reflect *this* step, never a mix of this step's
            // X with a previous step's Y (the old staircase failure mode).
            assert_eq!(frame.rx, x as i32, "x did not update for step {step}");
            assert_eq!(frame.ry, y as i32, "y did not update for step {step}");
        }
    }

    /// Regression test for the cursor-jump-on-reposition bug: touch state and
    /// position used to come from two independently-timed sources (evdev
    /// position vs. hidraw touch bit via a `watch` channel), so a lift could
    /// be processed after stale position data, or be silently coalesced away
    /// entirely under fast lift/retouch cycles. Here, touch and position come
    /// from the same parsed frame, and frames are processed strictly in the
    /// order they were parsed (mpsc, not watch) — so a lift is always seen
    /// with its own frame's data, and a subsequent touch-down elsewhere is
    /// always a distinct, later frame, never merged or reordered with the
    /// lift.
    #[test]
    fn lift_and_retouch_elsewhere_are_seen_as_distinct_ordered_frames() {
        let touching_at_a = PadFrame::parse(&make_report(0, 0, false, 1000, 2000, true));
        let lifted = PadFrame::parse(&make_report(0, 0, false, 1000, 2000, false));
        let touching_at_b = PadFrame::parse(&make_report(0, 0, false, 5000, 6000, true));

        assert!(touching_at_a.rtouch);
        assert!(!lifted.rtouch, "lifted frame must report not-touching");
        assert!(touching_at_b.rtouch);
        assert_eq!((touching_at_b.rx, touching_at_b.ry), (5000, 6000));
        // Crucially: the lift frame and the new touch-down frame are two
        // separate, ordered frames — there is no way to observe the new
        // position (5000, 6000) while `rtouch` still reads the old lift
        // state, because both always come from one read() together. Whether
        // this pair of frames constitutes a "change" worth routing is a
        // `trackpad_router::diff_frames` concern, tested there.
    }

    // --- Haptic feedback wire format ---------------------------------------

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
        assert_eq!(buf[0], 0x00, "report ID must always be 0");
        assert_eq!(buf[1], 0x8F, "command must be ID_TRIGGER_HAPTIC_PULSE");
        assert_eq!(buf[2], 8, "payload length must be 8");
        assert_eq!(buf[3], 0, "HapticPad::Right must be wire value 0 (swapped from hid-steam.c naming — see wire_value)");
        assert_eq!(&buf[4..6], &0x1234u16.to_le_bytes());
        assert_eq!(&buf[6..8], &0x5678u16.to_le_bytes());
        assert_eq!(&buf[8..10], &3u16.to_le_bytes());
        assert_eq!(buf[10] as i8, -6);
        assert!(buf[11..].iter().all(|&b| b == 0), "padding must be zeroed");
        assert_eq!(buf.len(), 65);
    }

    #[test]
    fn build_haptic_report_pad_wire_values_match_steam_firmware_constants() {
        // Wire values are swapped relative to the hid-steam.c constant names
        // (STEAM_PAD_LEFT/RIGHT = 0/1) based on on-hardware testing — see the
        // comment on `HapticPad::wire_value`. "Both" is unaffected.
        let base = HapticCommand { pad: HapticPad::Left, duration_us: 0, interval_us: 0, count: 0, gain_db: 0 };
        assert_eq!(build_haptic_report(&base)[3], 1);
        assert_eq!(build_haptic_report(&HapticCommand { pad: HapticPad::Right, ..base })[3], 0);
        assert_eq!(build_haptic_report(&HapticCommand { pad: HapticPad::Both, ..base })[3], 2);
    }

    /// The ioctl request number is a fixed, well-known constant on Linux
    /// (verified against `<linux/hidraw.h>`'s `HIDIOCSFEATURE(len)` macro
    /// expansion for len=65: dir=3, type='H'=0x48, nr=0x06, size=65) — pin it
    /// down so a refactor of the bit-shifting can't silently break it.
    #[test]
    fn hidiocsfeature_65_matches_known_constant() {
        // dir(3) << 30 | type(0x48) << 8 | nr(0x06) | size(65) << 16
        let expected: u32 = (3u32 << 30) | (0x48u32 << 8) | 0x06u32 | (65u32 << 16);
        assert_eq!(hidiocsfeature(65) as u32, expected);
    }
}
