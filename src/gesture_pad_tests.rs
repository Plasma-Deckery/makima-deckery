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
            on_gesture_start: start.map(HapticChain::single),
            on_gesture_move:  mov,
            on_gesture_end:   end.map(HapticChain::single),
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
    let mov = GestureMoveHaptic { pixel_interval: 10, chain: HapticChain::single(pulse_of(30)) };
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
    let mov = GestureMoveHaptic { pixel_interval: 1, chain: HapticChain::single(pulse_of(30)) };
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

// ── Config ───────────────────────────────────────────────────────────────────

fn parse(src: &str) -> (GesturePadConfig, Vec<String>) {
    GesturePadConfig::parse(&toml::from_str::<toml::Value>(src).unwrap())
}

#[test]
fn an_empty_section_is_silent() {
    let (c, w) = parse("[kde]\nscroll_factor = 1.0");
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(c, GesturePadConfig::default());
}

#[test]
fn every_haptic_key_takes_a_single_event_or_a_list() {
    let (c, w) = parse(r#"
        pixel_interval = 1500
        [haptic]
        on_gesture_start = { duration_us = 100, count = 1 }
        on_gesture_move  = { duration_us = 200, count = 1 }
        on_gesture_end   = [{ duration_us = 300, pause_ms = 40 }, { duration_us = 400 }]
    "#);
    assert!(w.is_empty(), "{w:?}");
    let one = |d| HapticChain::Single(HapticPulse { duration_us: d, count: 1, ..Default::default() });
    assert_eq!(c.haptic.on_gesture_start, Some(one(100)));
    assert_eq!(c.haptic.on_gesture_move, Some(GestureMoveHaptic { pixel_interval: 1500, chain: one(200) }));
    assert!(matches!(c.haptic.on_gesture_end, Some(HapticChain::Chain(ref s)) if s.len() == 2));
}

#[test]
fn true_false_and_the_default_gate() {
    let (c, w) = parse("[haptic]\non_gesture_start = true\non_gesture_move = true\non_gesture_end = false");
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(c.haptic.on_gesture_start, Some(HapticChain::default()));
    assert_eq!(c.haptic.on_gesture_move.unwrap().pixel_interval, 3000);
    assert_eq!(c.haptic.on_gesture_end, None);
}

#[test]
fn the_old_flat_move_form_warns_once_and_stays_off() {
    let (c, w) = parse("[haptic]\non_gesture_move = { pixel_interval = 3000, duration_us = 4000 }");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("[trackpad.gestures]"), "{w:?}");
    assert_eq!(c.haptic.on_gesture_move, None);
}

#[test]
fn a_list_plays_as_a_list() {
    let (cfg, _) = parse("[haptic]\non_gesture_start = [{ duration_us = 1, pause_ms = 5 }, { duration_us = 2 }]");
    let (_, actions) = plan_haptics(GestureProgress::default(), GestureEvent::Start(both_at(0, 0, 0, 0)), &cfg);
    assert!(matches!(&actions[..], [OutputAction::Haptic { pad: HapticPad::Both, chain: HapticChain::Chain(s) }] if s.len() == 2));
}
