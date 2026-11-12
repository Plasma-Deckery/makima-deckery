//! The sticks: the timer-driven motion loops, when they run, and decoding a
//! stick bound to directions.

use super::*;

fn axis_pairs(plan: &[OutputAction]) -> Vec<(u16, i32)> {
    plan.iter()
        .flat_map(|a| match a {
            OutputAction::Frame { events, .. } => {
                events.iter().map(|e| (e.code, e.value)).collect::<Vec<_>>()
            }
            _ => vec![],
        })
        .collect()
}

/// The framing fact, now where it matters: X and Y are two frames, so the kernel
/// reads two motions rather than one diagonal. Anyone merging them later changes
/// what the desktop sees.
#[test]
fn a_stick_tick_sends_two_frames_not_one() {
    let plan = plan_stick_tick(CURSOR_AXES, (5, -3), false, &[], &[], true);

    assert_eq!(plan.len(), 2, "two frames");
    assert_eq!(axis_pairs(&plan), vec![(0, 5), (1, -3)]);
}

/// Scrolling uses different axis codes, and the pair is stored X-then-Y while
/// the codes run 12 then 11 — which looks like a mistake in both places and is
/// not one.
#[test]
fn scrolling_uses_the_wheel_axes_in_that_order() {
    let plan = plan_stick_tick(SCROLL_AXES, (2, 7), false, &[], &[], true);
    assert_eq!(axis_pairs(&plan), vec![(12, 2), (11, 7)]);
}

/// A stick at rest sends nothing, rather than a stream of zeroes.
#[test]
fn a_stick_at_rest_sends_nothing() {
    assert!(plan_stick_tick(CURSOR_AXES, (0, 0), false, &[], &[], true).is_empty());
}

/// Inverting flips both axes, not just one.
#[test]
fn inverting_flips_both_axes() {
    let plan = plan_stick_tick(CURSOR_AXES, (5, -3), true, &[], &[], true);
    assert_eq!(axis_pairs(&plan), vec![(0, -5), (1, 3)]);
}

/// An activation modifier has to match exactly: holding an extra one switches
/// the stick off rather than leaving it on.
#[test]
fn activation_modifiers_must_match_exactly() {
    let needs_l1 = [ev(Key::BTN_TL)];

    let held_right = plan_stick_tick(CURSOR_AXES, (1, 1), false, &needs_l1, &needs_l1, true);
    assert_eq!(held_right.len(), 2);

    let nothing_held = plan_stick_tick(CURSOR_AXES, (1, 1), false, &needs_l1, &[], true);
    assert!(nothing_held.is_empty(), "the stick moved without its modifier");

    let one_too_many = plan_stick_tick(
        CURSOR_AXES, (1, 1), false, &needs_l1, &[ev(Key::BTN_TL), ev(Key::BTN_TR)], true,
    );
    assert!(one_too_many.is_empty(), "an extra modifier did not switch it off");
}

/// Suppressed — paused or Gaming Mode — sends nothing.
#[test]
fn an_inactive_stick_sends_nothing() {
    assert!(plan_stick_tick(CURSOR_AXES, (5, 5), false, &[], &[], false).is_empty());
}

/// A key-driven loop sends only the axes that are moving, so a tick can be two
/// frames, one, or none — unlike the analog loops, which always send both.
#[test]
fn a_key_tick_sends_only_the_moving_axes() {
    let (plan, _) = plan_key_tick(CURSOR_AXES, (1, 0), 10, 1.0, 10.0, true);
    assert_eq!(axis_pairs(&plan), vec![(0, 10)], "only X is moving");

    let (plan, _) = plan_key_tick(CURSOR_AXES, (0, -1), 10, 1.0, 10.0, true);
    assert_eq!(axis_pairs(&plan), vec![(1, -10)], "only Y is moving");
}

/// The ramp builds while a direction is held and is capped at the configured
/// speed, so a long hold does not accelerate for ever.
#[test]
fn the_ramp_builds_and_is_capped() {
    let (_, after_one) = plan_key_tick(CURSOR_AXES, (1, 0), 10, 1.0, 0.0, true);
    assert_eq!(after_one, 1.0, "one tick of a speed-10 ramp at full acceleration");

    let (_, capped) = plan_key_tick(CURSOR_AXES, (1, 0), 10, 1.0, 10.0, true);
    assert_eq!(capped, 10.0, "the ramp does not exceed the configured speed");
}

/// Releasing collapses the ramp immediately, so the next hold starts from a
/// standstill rather than from wherever it left off.
#[test]
fn releasing_collapses_the_ramp() {
    let (plan, speed) = plan_key_tick(CURSOR_AXES, (0, 0), 10, 1.0, 9.5, true);
    assert!(plan.is_empty());
    assert_eq!(speed, 0.0);
}

/// Suppressed still advances the ramp and only skips the sending — which is what
/// the old path does, so letting go of a stick during Gaming Mode does not leave
/// a stale ramp behind.
#[test]
fn a_suppressed_key_tick_still_advances_the_ramp() {
    let (plan, speed) = plan_key_tick(CURSOR_AXES, (1, 0), 10, 1.0, 0.0, false);
    assert!(plan.is_empty(), "nothing is sent while suppressed");
    assert_eq!(speed, 1.0, "but the ramp moved");
}

// ── stick_loop_active ─────────────────────────────────────────────────────

/// Normal operating state — not paused, not gaming: stick loop runs.
#[test]
fn stick_loop_active_normally() {
    assert!(stick_loop_active(false, false, false));
}

/// Gaming Mode active → stick loop must be suppressed.
#[test]
fn stick_loop_suppressed_in_gaming_mode() {
    assert!(!stick_loop_active(false, false, true),
        "gaming_mode=true must suppress stick loop");
    // Even cursor_when_paused cannot override gaming_mode suppression.
    assert!(!stick_loop_active(false, true, true),
        "cursor_when_paused does not override gaming_mode");
}

/// Paused without cursor_when_paused → stick loop suppressed.
#[test]
fn stick_loop_suppressed_when_paused() {
    assert!(!stick_loop_active(true, false, false),
        "paused=true without cursor_when_paused must suppress stick loop");
}

/// cursor_when_paused restores stick loop while paused, but not in Gaming Mode.
#[test]
fn stick_loop_cursor_when_paused_overrides_pause_but_not_gaming_mode() {
    assert!(stick_loop_active(true, true, false),
        "cursor_when_paused must allow stick loop while paused");
    assert!(!stick_loop_active(true, true, true),
        "cursor_when_paused must NOT allow stick loop in Gaming Mode");
}

/// Both paused and gaming_mode — stick loop suppressed.
#[test]
fn stick_loop_suppressed_when_both_paused_and_gaming_mode() {
    assert!(!stick_loop_active(true, false, true));
    assert!(!stick_loop_active(true, true, true));
}

// ── decoding a stick ──────────────────────────────────────────────────────

fn axes(events: &[(Axis, i32)]) -> Vec<(Axis, i32)> {
    events.to_vec()
}

/// Pushing into a direction fires once, and pushing further fires nothing —
/// events are transitions, not states.
#[test]
fn a_stick_fires_once_per_direction() {
    let (events, values) = decode_stick_bind(Stick::Left, false, -1, (0, 0));
    assert_eq!(axes(&events), vec![(Axis::LSTICK_UP, 1)]);
    assert_eq!(values, (0, -1));

    let (again, values) = decode_stick_bind(Stick::Left, false, -1, values);
    assert!(again.is_empty(), "pushing further in the same direction fired again");
    assert_eq!(values, (0, -1));
}

/// Returning to centre releases whichever direction was held — and only that
/// one, not both ends of the axis.
#[test]
fn returning_to_centre_releases_what_was_held() {
    let (events, values) = decode_stick_bind(Stick::Left, false, 0, (0, -1));
    assert_eq!(axes(&events), vec![(Axis::LSTICK_UP, 0)]);
    assert_eq!(values, (0, 0));

    let (events, _) = decode_stick_bind(Stick::Left, false, 0, (0, 1));
    assert_eq!(axes(&events), vec![(Axis::LSTICK_DOWN, 0)]);
}

/// Up is negative Y, because evdev counts downward. This is the one thing a
/// reader is likely to expect the other way round.
#[test]
fn up_is_negative_y() {
    let (up, _) = decode_stick_bind(Stick::Left, false, -1, (0, 0));
    let (down, _) = decode_stick_bind(Stick::Left, false, 1, (0, 0));
    assert_eq!(axes(&up), vec![(Axis::LSTICK_UP, 1)]);
    assert_eq!(axes(&down), vec![(Axis::LSTICK_DOWN, 1)]);
}

/// The two axes of one stick are tracked independently: holding up and then
/// pushing left must not release up.
#[test]
fn the_two_axes_do_not_interfere() {
    let (_, values) = decode_stick_bind(Stick::Left, false, -1, (0, 0));
    let (events, values) = decode_stick_bind(Stick::Left, true, -1, values);

    assert_eq!(axes(&events), vec![(Axis::LSTICK_LEFT, 1)]);
    assert_eq!(values, (-1, -1), "both directions are held at once");
}

/// The right stick is the same decision with different names, which is the
/// whole reason one function replaced two arms.
#[test]
fn the_right_stick_names_its_own_directions() {
    let (y, _) = decode_stick_bind(Stick::Right, false, -1, (0, 0));
    let (x, _) = decode_stick_bind(Stick::Right, true, 1, (0, 0));
    assert_eq!(axes(&y), vec![(Axis::RSTICK_UP, 1)]);
    assert_eq!(axes(&x), vec![(Axis::RSTICK_RIGHT, 1)]);
}

/// The axis codes the sides answer to: 0/1 for the left stick, 3/4 for the
/// right. Getting this wrong would route X to Y.
#[test]
fn the_sides_answer_to_their_own_codes() {
    assert_eq!(Stick::Left.base_code(), 0);
    assert_eq!(Stick::Right.base_code(), 3);
}

/// The deadzone swallows small movement entirely, and the scaling is what the
/// reader's own method did — it awaited nothing and read one setting.
#[test]
fn the_deadzone_swallows_small_movement() {
    assert_eq!(axis_value(0, 10, true), 0, "dead centre");
    assert_eq!(axis_value(1500, 10, true), 0, "inside a deadzone of 10");
    assert_eq!(axis_value(-1500, 10, true), 0);
    assert!(axis_value(20000, 10, true) > 0, "outside it");
    assert!(axis_value(-20000, 10, true) < 0);
}

/// An 8-bit stick is centred at 128 rather than 0, so the same raw number means
/// opposite things depending on the setting.
#[test]
fn eight_bit_sticks_are_centred_at_128() {
    assert_eq!(axis_value(128, 0, false), 0, "128 is centre on an 8-bit axis");
    assert!(axis_value(255, 0, false) > 0);
    assert!(axis_value(0, 0, false) < 0);
}
