//! What one trackpad frame should feel like.
//!
//! These exist because `plan_haptics` is pure: before issue #57 the same
//! decisions were interleaved with `tx.send().await` calls, so checking any of
//! them meant a Deck in your hands.

use super::*;
use deckery_controller::{HapticChain, HapticPad, HapticPulse};

fn frame(x: i32, y: i32, touching: bool, click: bool) -> SinglePadFrame {
    SinglePadFrame { x, y, touching, click, interrupted: false }
}

/// Pulses that differ only in duration, so a test can tell which one fired.
fn pulse_of(duration_us: u16) -> HapticPulse {
    HapticPulse { duration_us, ..Default::default() }
}

fn movement(pixel_interval: u32) -> MovementHaptic {
    MovementHaptic { pixel_interval, chain: HapticChain::single(pulse_of(30)) }
}

fn felt(pad: HapticPad, chain: HapticChain) -> OutputAction {
    OutputAction::Haptic { pad, chain }
}

#[test]
fn press_edge_fires_the_press_chain() {
    let press = HapticChain::single(pulse_of(10));
    let (next, actions) = plan_haptics(
        HapticProgress::default(), &frame(0, 0, true, true),
        HapticPad::Left, Some(&press), None, None,
    );
    assert_eq!(actions, vec![felt(HapticPad::Left, press)]);
    assert!(next.prev_click, "the edge is consumed, so holding must not fire again");
}

#[test]
fn a_held_click_fires_nothing() {
    let press = HapticChain::single(pulse_of(10));
    let held = HapticProgress { prev_click: true, ..Default::default() };
    let (_, actions) = plan_haptics(
        held, &frame(0, 0, true, true), HapticPad::Left, Some(&press), None, None,
    );
    assert!(actions.is_empty());
}

#[test]
fn release_edge_fires_the_release_chain() {
    let press   = HapticChain::single(pulse_of(10));
    let release = HapticChain::single(pulse_of(20));
    let held = HapticProgress { prev_click: true, ..Default::default() };
    let (_, actions) = plan_haptics(
        held, &frame(0, 0, true, false),
        HapticPad::Right, Some(&press), Some(&release), None,
    );
    assert_eq!(actions, vec![felt(HapticPad::Right, release)],
               "press and release are two separately-feelable edges");
}

#[test]
fn release_edge_without_a_release_chain_is_silent() {
    let press = HapticChain::single(pulse_of(10));
    let held = HapticProgress { prev_click: true, ..Default::default() };
    let (_, actions) = plan_haptics(
        held, &frame(0, 0, true, false), HapticPad::Left, Some(&press), None, None,
    );
    assert!(actions.is_empty(), "omitting on_release silences release only");
}

#[test]
fn movement_fires_once_per_interval_and_resets() {
    let press = HapticChain::single(pulse_of(10));
    let mov = movement(10);
    let plan = |progress, f| plan_haptics(
        progress, &f, HapticPad::Left, Some(&press), None, Some(&mov),
    );

    // The first touching frame only seeds the position — there is no previous
    // one to measure against, so nothing is felt.
    let (p1, a1) = plan(HapticProgress::default(), frame(0, 0, true, false));
    assert!(a1.is_empty());
    assert_eq!(p1.prev_pos, Some((0, 0)));

    let (p2, a2) = plan(p1, frame(6, 0, true, false));
    assert!(a2.is_empty(), "six raw units is under the ten-unit gate");

    let (p3, a3) = plan(p2, frame(12, 0, true, false));
    assert_eq!(a3, vec![felt(HapticPad::Left, mov.chain.clone())]);
    assert_eq!(p3.move_accum, 0.0, "crossing the gate resets the distance");
}

#[test]
fn lifting_the_finger_clears_the_accumulator() {
    let press = HapticChain::single(pulse_of(10));
    let mov = movement(100);
    let moved = HapticProgress { prev_pos: Some((50, 50)), move_accum: 90.0, prev_click: false };
    let (next, actions) = plan_haptics(
        moved, &frame(50, 50, false, false), HapticPad::Left, Some(&press), None, Some(&mov),
    );
    assert!(actions.is_empty());
    assert_eq!(next.prev_pos, None, "a new touch must not measure from the old one");
    assert_eq!(next.move_accum, 0.0);
}

#[test]
fn without_a_movement_pulse_no_position_is_tracked() {
    let press = HapticChain::single(pulse_of(10));
    let (next, actions) = plan_haptics(
        HapticProgress::default(), &frame(5, 5, true, false),
        HapticPad::Left, Some(&press), None, None,
    );
    assert!(actions.is_empty());
    assert_eq!(next.prev_pos, None,
               "nothing gates on distance, so nothing pays for measuring it");
}

#[test]
fn a_press_is_felt_before_the_movement_it_arrives_with() {
    let press = HapticChain::single(pulse_of(10));
    let mov = movement(10);
    let moving = HapticProgress { prev_click: false, prev_pos: Some((0, 0)), move_accum: 9.0 };
    let (_, actions) = plan_haptics(
        moving, &frame(10, 0, true, true), HapticPad::Left, Some(&press), None, Some(&mov),
    );
    assert_eq!(actions, vec![
        felt(HapticPad::Left, press),
        felt(HapticPad::Left, mov.chain.clone()),
    ], "within one frame the click edge comes first");
}

// ── Config ───────────────────────────────────────────────────────────────────

fn parse(src: &str) -> (MtTrackpadConfig, Vec<String>) {
    MtTrackpadConfig::parse(&toml::from_str::<toml::Value>(src).unwrap())
}

#[test]
fn an_empty_section_keeps_the_click_chains_and_no_movement() {
    let (c, w) = parse("mode = \"mt-trackpad\"\nclick_pressure = 30000");
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(c, MtTrackpadConfig::default());
    assert_eq!(c.press_chain(), Some(HapticChain::default()));
    assert_eq!(c.release_chain(), Some(HapticChain::default()));
    assert_eq!(c.movement_pulse(), None);
}

#[test]
fn every_haptic_key_takes_a_single_event_or_a_list() {
    let (c, w) = parse(r#"
        pixel_interval = 2500
        [haptic]
        on_press    = { duration_us = 100, count = 2 }
        on_release  = [{ duration_us = 200, pause_ms = 40 }, { duration_us = 300 }]
        on_movement = [{ duration_us = 400, count = 1 }]
    "#);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(c.on_press, Some(HapticChain::Single(HapticPulse { duration_us: 100, count: 2, ..Default::default() })));
    assert!(matches!(c.on_release, Some(HapticChain::Chain(ref s)) if s.len() == 2 && s[0].pause_ms == Some(40)));
    let m = c.on_movement.unwrap();
    assert_eq!(m.pixel_interval, 2500, "the gate is the section's");
    assert!(matches!(m.chain, HapticChain::Chain(ref s) if s.len() == 1));
}

#[test]
fn the_gate_defaults_to_3000_and_false_true_work() {
    let (c, w) = parse("[haptic]\non_press = false\non_release = false\non_movement = true");
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(c.on_press, None);
    assert_eq!(c.on_release, None);
    assert_eq!(c.on_movement, Some(MovementHaptic { pixel_interval: 3000, chain: crate::config::default_tick() }));
}

#[test]
fn the_old_flat_movement_form_warns_once_and_stays_off() {
    let (c, w) = parse("[haptic]\non_movement = { pixel_interval = 3000, duration_us = 4000, count = 1 }");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("`pixel_interval`") && w[0].contains("pad section"), "{w:?}");
    assert_eq!(c.on_movement, None);
}

#[test]
fn bad_values_warn_and_fall_back_on_their_own() {
    let (c, w) = parse(r#"
        pixel_interval = -1
        [haptic]
        on_press = "buzz"
        on_release = { duraton_us = 5 }
        on_movement = { duration_us = 4000 }
    "#);
    assert_eq!(w.len(), 3, "{w:?}");
    assert_eq!(c.on_press, Some(HapticChain::default()));
    assert_eq!(c.on_release, Some(HapticChain::default()));
    assert_eq!(c.on_movement.unwrap().pixel_interval, 3000);
}

#[test]
fn a_silenced_press_is_not_felt() {
    let (_, actions) = plan_haptics(
        HapticProgress::default(), &frame(0, 0, true, true), HapticPad::Left, None, None, None,
    );
    assert!(actions.is_empty());
}
