//! What one trackpad frame should feel like.
//!
//! These exist because `plan_haptics` is pure: before issue #57 the same
//! decisions were interleaved with `tx.send().await` calls, so checking any of
//! them meant a Deck in your hands.

use super::*;
use deckery_controller::{HapticChain, HapticPad, HapticPulse};

fn frame(x: i32, y: i32, touching: bool, click: bool) -> SinglePadFrame {
    SinglePadFrame { x, y, touching, click }
}

/// Pulses that differ only in duration, so a test can tell which one fired.
fn pulse_of(duration_us: u16) -> HapticPulse {
    HapticPulse { duration_us, ..Default::default() }
}

fn movement(pixel_interval: u32) -> MovementHaptic {
    MovementHaptic { pixel_interval, pulse: pulse_of(30) }
}

fn felt(pad: HapticPad, chain: HapticChain) -> OutputAction {
    OutputAction::Haptic { pad, chain }
}

#[test]
fn press_edge_fires_the_press_chain() {
    let press = HapticChain::single(pulse_of(10));
    let (next, actions) = plan_haptics(
        HapticProgress::default(), &frame(0, 0, true, true),
        HapticPad::Left, &press, None, None,
    );
    assert_eq!(actions, vec![felt(HapticPad::Left, press)]);
    assert!(next.prev_click, "the edge is consumed, so holding must not fire again");
}

#[test]
fn a_held_click_fires_nothing() {
    let press = HapticChain::single(pulse_of(10));
    let held = HapticProgress { prev_click: true, ..Default::default() };
    let (_, actions) = plan_haptics(
        held, &frame(0, 0, true, true), HapticPad::Left, &press, None, None,
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
        HapticPad::Right, &press, Some(&release), None,
    );
    assert_eq!(actions, vec![felt(HapticPad::Right, release)],
               "press and release are two separately-feelable edges");
}

#[test]
fn release_edge_without_a_release_chain_is_silent() {
    let press = HapticChain::single(pulse_of(10));
    let held = HapticProgress { prev_click: true, ..Default::default() };
    let (_, actions) = plan_haptics(
        held, &frame(0, 0, true, false), HapticPad::Left, &press, None, None,
    );
    assert!(actions.is_empty(), "omitting on_release silences release only");
}

#[test]
fn movement_fires_once_per_interval_and_resets() {
    let press = HapticChain::single(pulse_of(10));
    let mov = movement(10);
    let plan = |progress, f| plan_haptics(
        progress, &f, HapticPad::Left, &press, None, Some(mov),
    );

    // The first touching frame only seeds the position — there is no previous
    // one to measure against, so nothing is felt.
    let (p1, a1) = plan(HapticProgress::default(), frame(0, 0, true, false));
    assert!(a1.is_empty());
    assert_eq!(p1.prev_pos, Some((0, 0)));

    let (p2, a2) = plan(p1, frame(6, 0, true, false));
    assert!(a2.is_empty(), "six raw units is under the ten-unit gate");

    let (p3, a3) = plan(p2, frame(12, 0, true, false));
    assert_eq!(a3, vec![felt(HapticPad::Left, HapticChain::single(mov.pulse))]);
    assert_eq!(p3.move_accum, 0.0, "crossing the gate resets the distance");
}

#[test]
fn lifting_the_finger_clears_the_accumulator() {
    let press = HapticChain::single(pulse_of(10));
    let mov = movement(100);
    let moved = HapticProgress { prev_pos: Some((50, 50)), move_accum: 90.0, prev_click: false };
    let (next, actions) = plan_haptics(
        moved, &frame(50, 50, false, false), HapticPad::Left, &press, None, Some(mov),
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
        HapticPad::Left, &press, None, None,
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
        moving, &frame(10, 0, true, true), HapticPad::Left, &press, None, Some(mov),
    );
    assert_eq!(actions, vec![
        felt(HapticPad::Left, press),
        felt(HapticPad::Left, HapticChain::single(mov.pulse)),
    ], "within one frame the click edge comes first");
}
