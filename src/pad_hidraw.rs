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
        l_changed: (frame.lx, frame.ly, frame.ltouch, frame.lclick)
            != (prev.lx, prev.ly, prev.ltouch, prev.lclick),
        r_changed: (frame.rx, frame.ry, frame.rtouch, frame.rclick)
            != (prev.rx, prev.ry, prev.rtouch, prev.rclick),
        touch_transition: frame.ltouch != prev.ltouch || frame.rtouch != prev.rtouch,
    }
}

/// Identifies one physical trackpad, used by `decide_gesture_transition` to
/// say which pad "survives" when a combined gesture session ends with only
/// one finger still down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pad {
    Left,
    Right,
}

/// Result of feeding one `PadFrame` into the combined-gesture state machine.
/// Pure and I/O-free so the entry/exit logic can be unit-tested directly,
/// independent of which pad physically touched down first or lifted first —
/// see the `tests` module below for the symmetry checks this exists to
/// guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GestureTransition {
    /// Whether a combined gesture session is active *after* this frame.
    pub now_active: bool,
    /// True only on the single frame where the session starts (both pads
    /// just became simultaneously touched) — callers use this to force a
    /// clean lift on any individual per-pad MT devices before combined
    /// events start.
    pub entering: bool,
    /// True only on the single frame where an active session ends (one or
    /// both fingers lifted).
    pub exiting: bool,
    /// If a session just ended (`exiting`) and exactly one finger is still
    /// touching, this is that pad — it should resume on its own individual
    /// MT device with a synthetic touch-down. `None` if both fingers lifted
    /// at the same time (nothing to resume).
    pub resume_survivor: Option<Pad>,
}

/// Decides the combined-gesture state transition for `frame`, given whether
/// a session was already active going into it. Entry requires both pads
/// touching simultaneously; exit fires as soon as either lifts — both
/// checks are pad-order-independent by construction (they only look at
/// `ltouch`/`rtouch` together, never which one changed first), which is
/// what guarantees "start left, add right" and "start right, add left" (and
/// likewise for lifting) behave identically.
pub fn decide_gesture_transition(was_active: bool, frame: PadFrame) -> GestureTransition {
    let mut now_active = was_active;
    if frame.ltouch && frame.rtouch {
        now_active = true;
    }
    if now_active && (!frame.ltouch || !frame.rtouch) {
        let resume_survivor = if frame.ltouch {
            Some(Pad::Left)
        } else if frame.rtouch {
            Some(Pad::Right)
        } else {
            None
        };
        return GestureTransition { now_active: false, entering: false, exiting: was_active, resume_survivor };
    }
    GestureTransition {
        now_active,
        entering: !was_active && now_active,
        exiting: false,
        resume_survivor: None,
    }
}

/// A physical click on either half of the pad while both fingers are down
/// in an active gesture session reads as one combined click — the same way
/// combined movement is reported once on the gesture device instead of
/// twice on two individual ones.
pub fn combined_click(frame: PadFrame) -> bool {
    frame.lclick || frame.rclick
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

    /// Shorthand for building a `PadFrame` straight from touch state, for
    /// the gesture-transition tests below where exact position doesn't
    /// matter.
    fn touch_frame(ltouch: bool, rtouch: bool) -> PadFrame {
        PadFrame::parse(&make_report(0, 0, ltouch, 0, 0, rtouch))
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

    /// A click-only change (touch and position unchanged) must still count
    /// as a change — otherwise a tap-click with the finger resting still
    /// would never reach the emit call in `pad_loop`.
    #[test]
    fn click_only_change_is_detected() {
        let a = PadFrame::parse(&make_report_full(1, 2, true, false, 3, 4, true, false));
        let b = PadFrame::parse(&make_report_full(1, 2, true, true, 3, 4, true, false));
        let delta = diff_frames(a, b);
        assert!(delta.l_changed, "left click-only change was not detected");
        assert!(!delta.r_changed);
    }

    // --- Combined-gesture transition symmetry -----------------------------
    //
    // These pin down that entering/exiting a combined two-finger gesture
    // session behaves identically no matter which pad touched down or
    // lifted first — the asymmetry a manual test session found (right
    // continues after left lifts, but not vice versa) turned out to be
    // explained by config (left pad mode = disabled, no individual device to
    // resume onto), not by the transition logic itself. These tests assume a
    // symmetric setup (both pads capable of resuming) so a *real* asymmetry
    // in `decide_gesture_transition` would fail here regardless of config.

    #[test]
    fn gesture_enters_when_both_touch_regardless_of_which_touched_first() {
        // Left touched first, right added later.
        let mut active = false;
        let t1 = decide_gesture_transition(active, touch_frame(true, false));
        assert!(!t1.now_active && !t1.entering, "single-pad touch must not start a gesture");
        active = t1.now_active;
        let t2 = decide_gesture_transition(active, touch_frame(true, true));
        assert!(t2.now_active && t2.entering, "adding the second pad must start the gesture");

        // Mirror: right touched first, left added later.
        let mut active = false;
        let t1 = decide_gesture_transition(active, touch_frame(false, true));
        assert!(!t1.now_active && !t1.entering);
        active = t1.now_active;
        let t2 = decide_gesture_transition(active, touch_frame(true, true));
        assert!(t2.now_active && t2.entering);
    }

    #[test]
    fn gesture_exit_survivor_is_right_when_left_lifts_first() {
        // Active session, left finger lifts, right keeps touching.
        let transition = decide_gesture_transition(true, touch_frame(false, true));
        assert!(!transition.now_active);
        assert!(transition.exiting);
        assert_eq!(transition.resume_survivor, Some(Pad::Right));
    }

    #[test]
    fn gesture_exit_survivor_is_left_when_right_lifts_first() {
        // Mirror of the above: active session, right lifts, left keeps
        // touching. Must behave identically (just mirrored), or the manual
        // "one direction has no follow-through" report would be a real bug.
        let transition = decide_gesture_transition(true, touch_frame(true, false));
        assert!(!transition.now_active);
        assert!(transition.exiting);
        assert_eq!(transition.resume_survivor, Some(Pad::Left));
    }

    #[test]
    fn gesture_exit_no_survivor_when_both_lift_simultaneously() {
        let transition = decide_gesture_transition(true, touch_frame(false, false));
        assert!(!transition.now_active);
        assert!(transition.exiting);
        assert_eq!(transition.resume_survivor, None);
    }

    #[test]
    fn gesture_continues_while_both_still_touching() {
        let transition = decide_gesture_transition(true, touch_frame(true, true));
        assert!(transition.now_active);
        assert!(!transition.entering);
        assert!(!transition.exiting);
        assert_eq!(transition.resume_survivor, None);
    }

    #[test]
    fn combined_click_true_if_either_pad_clicked() {
        let l_only = PadFrame::parse(&make_report_full(0, 0, true, true, 0, 0, true, false));
        let r_only = PadFrame::parse(&make_report_full(0, 0, true, false, 0, 0, true, true));
        let both = PadFrame::parse(&make_report_full(0, 0, true, true, 0, 0, true, true));
        let neither = PadFrame::parse(&make_report_full(0, 0, true, false, 0, 0, true, false));
        assert!(combined_click(l_only));
        assert!(combined_click(r_only));
        assert!(combined_click(both));
        assert!(!combined_click(neither));
    }
}
