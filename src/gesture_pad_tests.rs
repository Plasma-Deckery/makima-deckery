//! What a two-finger gesture should feel like.
//!
//! Keyed on the lifecycle, not on the click bit — see the module docs for why
//! a click during a gesture has no haptic here at all.

use super::*;
use deckery_controller::{HapticChain, HapticPad, HapticPulse};

fn pulse_of(duration_us: u16) -> HapticPulse {
    HapticPulse { duration_us, ..Default::default() }
}

fn config(
    start: Option<HapticPulse>,
    mov: Option<GestureMoveHaptic>,
    end: Option<HapticPulse>,
) -> GesturePadConfig {
    GesturePadConfig {
        haptic: GestureHapticConfig {
            on_gesture_start: start,
            on_gesture_move:  mov,
            on_gesture_end:   end,
        },
    }
}

fn both_at(lx: i32, ly: i32, rx: i32, ry: i32) -> CombinedPadFrame {
    CombinedPadFrame {
        lx, ly, rx, ry,
        ltouch: true, rtouch: true,
        ..Default::default()
    }
}

fn felt(pulse: HapticPulse) -> OutputAction {
    OutputAction::Haptic { pad: HapticPad::Both, chain: HapticChain::single(pulse) }
}

#[test]
fn start_fires_its_pulse_and_seeds_the_measurement() {
    let cfg = config(Some(pulse_of(10)), None, None);
    let start = both_at(0, 0, 0, 0);
    let (next, actions) = plan_haptics(
        GestureProgress::default(), GestureEvent::Start(start), &cfg,
    );
    assert_eq!(actions, vec![felt(pulse_of(10))]);
    assert_eq!(next.prev_move_frame, Some(start),
               "the first Move must have something to measure against");
}

#[test]
fn end_fires_its_pulse_and_forgets_the_session() {
    let cfg = config(None, None, Some(pulse_of(20)));
    let mid = GestureProgress { prev_move_frame: Some(both_at(5, 5, 5, 5)), move_accum: 40.0 };
    let (next, actions) = plan_haptics(mid, GestureEvent::End, &cfg);
    assert_eq!(actions, vec![felt(pulse_of(20))]);
    assert_eq!(next.prev_move_frame, None);
    assert_eq!(next.move_accum, 0.0);
}

#[test]
fn movement_sums_both_pads_and_fires_once_past_the_gate() {
    let mov = GestureMoveHaptic { pixel_interval: 10, pulse: pulse_of(30) };
    let cfg = config(None, Some(mov), None);

    let first = both_at(0, 0, 0, 0);
    let (p1, _) = plan_haptics(GestureProgress::default(), GestureEvent::Start(first), &cfg);

    // Three units on the left pad and three on the right is six, not three.
    let (p2, a2) = plan_haptics(p1, GestureEvent::Move(both_at(3, 0, 3, 0)), &cfg);
    assert!(a2.is_empty());
    assert_eq!(p2.move_accum, 6.0);

    let (p3, a3) = plan_haptics(p2, GestureEvent::Move(both_at(5, 0, 5, 0)), &cfg);
    assert_eq!(a3, vec![felt(pulse_of(30))]);
    assert_eq!(p3.move_accum, 0.0);
}

#[test]
fn a_pad_touching_down_mid_gesture_contributes_no_jump() {
    let mov = GestureMoveHaptic { pixel_interval: 1, pulse: pulse_of(30) };
    let cfg = config(None, Some(mov), None);

    // The right pad was not touching, and its stale position is far away.
    let before = CombinedPadFrame {
        lx: 0, ly: 0, rx: 9000, ry: 9000,
        ltouch: true, rtouch: false,
        ..Default::default()
    };
    let now = CombinedPadFrame {
        lx: 0, ly: 0, rx: 0, ry: 0,
        ltouch: true, rtouch: true,
        ..Default::default()
    };
    let (next, actions) = plan_haptics(
        GestureProgress { prev_move_frame: Some(before), move_accum: 0.0 },
        GestureEvent::Move(now), &cfg,
    );
    assert_eq!(next.move_accum, 0.0, "only pads touching in both frames count");
    assert!(actions.is_empty(), "so the stale 9000-unit position fires nothing");
}

#[test]
fn the_measurement_advances_even_with_no_movement_pulse_configured() {
    let cfg = config(None, None, None);
    let moved = both_at(7, 7, 7, 7);
    let (next, actions) = plan_haptics(
        GestureProgress::default(), GestureEvent::Move(moved), &cfg,
    );
    assert!(actions.is_empty());
    assert_eq!(next.prev_move_frame, Some(moved),
               "configuring a pulse later must not measure from a stale frame");
}
