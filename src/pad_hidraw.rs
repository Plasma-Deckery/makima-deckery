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
    pub rx: i32,
    pub ry: i32,
    pub rtouch: bool,
}

impl PadFrame {
    fn parse(buf: &[u8; 64]) -> Self {
        let byte10 = buf[10];
        Self {
            lx: i16::from_le_bytes([buf[16], buf[17]]) as i32,
            ly: i16::from_le_bytes([buf[18], buf[19]]) as i32,
            ltouch: (byte10 & 0x08) != 0,
            rx: i16::from_le_bytes([buf[20], buf[21]]) as i32,
            ry: i16::from_le_bytes([buf[22], buf[23]]) as i32,
            rtouch: (byte10 & 0x10) != 0,
        }
    }
}

/// Result of comparing two consecutive `PadFrame`s: which pad(s) actually
/// changed, and whether either pad's touch state flipped. Pure and I/O-free
/// on purpose so it can be unit-tested without an async runtime or a real
/// virtual device — see the `tests` module below. Used by
/// `EventReader::pad_loop` to decide whether to emit/write state at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PadDelta {
    pub l_changed: bool,
    pub r_changed: bool,
    pub touch_transition: bool,
}

pub fn diff_frames(prev: PadFrame, frame: PadFrame) -> PadDelta {
    PadDelta {
        l_changed: (frame.lx, frame.ly, frame.ltouch) != (prev.lx, prev.ly, prev.ltouch),
        r_changed: (frame.rx, frame.ry, frame.rtouch) != (prev.rx, prev.ry, prev.rtouch),
        touch_transition: frame.ltouch != prev.ltouch || frame.rtouch != prev.rtouch,
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

/// Spawns the pad hidraw reader task if a hidraw sibling is found for
/// `evdev_path`. Returns the receiving end of the channel it feeds, or
/// `None` if no hidraw sibling exists (trackpad position/touch will then
/// simply never update — `MtTrackpad` mode requires hidraw, there is no
/// evdev-based fallback).
pub fn spawn(evdev_path: &std::path::Path) -> Option<mpsc::Receiver<PadFrame>> {
    let hidraw_path = find_hidraw_for_evdev(evdev_path)?;
    println!("makima: pad hidraw reader attached to {:?}", hidraw_path);
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        run_pad_hidraw_reader(hidraw_path, tx).await;
    });
    Some(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a synthetic 64-byte hidraw report with the given RPAD/LPAD
    /// position and touch bits at the empirically-determined offsets
    /// (see module docs), everything else zeroed.
    fn make_report(lx: i16, ly: i16, ltouch: bool, rx: i16, ry: i16, rtouch: bool) -> [u8; 64] {
        let mut buf = [0u8; 64];
        let mut byte10 = 0u8;
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
            PadFrame { lx: 1111, ly: -2222, ltouch: true, rx: 3333, ry: -4444, rtouch: false }
        );
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
        let mut prev = PadFrame::default();
        for step in 1..=20i16 {
            let x = step * 100;
            let y = -step * 50;
            let buf = make_report(0, 0, false, x, y, true);
            let frame = PadFrame::parse(&buf);
            // Both axes must reflect *this* step, never a mix of this step's
            // X with a previous step's Y (the old staircase failure mode).
            assert_eq!(frame.rx, x as i32, "x did not update for step {step}");
            assert_eq!(frame.ry, y as i32, "y did not update for step {step}");
            let delta = diff_frames(prev, frame);
            assert!(delta.r_changed, "step {step} not detected as changed");
            prev = frame;
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

        let lift_delta = diff_frames(touching_at_a, lifted);
        assert!(lift_delta.touch_transition, "lift was not detected as a touch transition");
        assert!(!lifted.rtouch, "lifted frame must report not-touching");

        let retouch_delta = diff_frames(lifted, touching_at_b);
        assert!(retouch_delta.touch_transition, "retouch was not detected as a touch transition");
        assert!(touching_at_b.rtouch);
        assert_eq!((touching_at_b.rx, touching_at_b.ry), (5000, 6000));
        // Crucially: the lift frame and the new touch-down frame are two
        // separate, ordered frames — there is no way to observe the new
        // position (5000, 6000) while `rtouch` still reads the old lift
        // state, because both always come from one read() together.
    }

    /// A frame identical to the previous one must be reported as unchanged —
    /// otherwise pad_loop would emit and write state.json continuously even
    /// while the finger rests still.
    #[test]
    fn identical_frame_is_not_a_change() {
        let a = PadFrame::parse(&make_report(1, 2, true, 3, 4, true));
        let b = PadFrame::parse(&make_report(1, 2, true, 3, 4, true));
        let delta = diff_frames(a, b);
        assert!(!delta.l_changed);
        assert!(!delta.r_changed);
        assert!(!delta.touch_transition);
    }
}
