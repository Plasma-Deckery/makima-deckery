//! The trackball's physics and config, without a Deck and without a clock.
//!
//! `step` takes the elapsed time as an argument, so every test here says how
//! much time passes and nothing depends on how fast the machine runs.

use super::*;
use deckery_controller::{HapticChain, HapticPad, HapticPulse};

const PAD: HapticPad = HapticPad::Right;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn frame(x: i32, y: i32, touching: bool, click: bool) -> SinglePadFrame {
    SinglePadFrame { x, y, touching, click, interrupted: false }
}

fn touch(x: i32, y: i32) -> Input {
    Input::Frame(frame(x, y, true, false))
}

fn lift() -> Input {
    Input::Frame(frame(0, 0, false, false))
}

/// Defaults without any haptics, so a test about motion sees only motion.
fn quiet() -> TrackballConfig {
    TrackballConfig { on_press: None, on_release: None, on_movement: None, on_roll: None, ..Default::default() }
}

/// Feed `inputs` (each with the time since the previous one) through `step`.
fn run(
    state: TrackballState,
    cfg: &TrackballConfig,
    inputs: &[(Input, Duration)],
) -> (TrackballState, Vec<OutputAction>) {
    let mut s = state;
    let mut all = Vec::new();
    for (input, elapsed) in inputs {
        let (next, actions) = step(s, *input, *elapsed, cfg, PAD);
        s = next;
        all.extend(actions);
    }
    (s, all)
}

/// Total pointer motion in a list of actions.
fn motion(actions: &[OutputAction]) -> (i64, i64) {
    let mut total = (0i64, 0i64);
    for a in actions {
        if let OutputAction::Frame { device: OutputDevice::Axis, events } = a {
            for e in events {
                assert_eq!(e.ev_type, EventType::RELATIVE.0);
                match e.code {
                    c if c == RelativeAxisType::REL_X.0 => total.0 += e.value as i64,
                    c if c == RelativeAxisType::REL_Y.0 => total.1 += e.value as i64,
                    c => panic!("unexpected relative axis {c}"),
                }
            }
        }
    }
    total
}

fn haptics(actions: &[OutputAction]) -> usize {
    actions.iter().filter(|a| matches!(a, OutputAction::Haptic { .. })).count()
}

fn button(pressed: bool) -> OutputAction {
    OutputAction::Frame {
        device: OutputDevice::Keys,
        events: vec![Ev { ev_type: EventType::KEY.0, code: Key::BTN_LEFT.code(), value: pressed as i32 }],
    }
}

/// A finger dragged right at `speed` raw units/s in 4 ms frames for `frames`
/// frames, starting at the origin. Returns the inputs, not yet lifted.
fn drag(speed: f64, frames: usize) -> Vec<(Input, Duration)> {
    let per_frame = (speed * 0.004) as i32;
    let mut v = vec![(touch(0, 0), ms(500))];
    for i in 1..=frames {
        v.push((touch(per_frame * i as i32, 0), ms(4)));
    }
    v
}

/// A ball already rolling right at `speed` raw units/s.
fn rolling(speed: f64, cfg: &TrackballConfig) -> TrackballState {
    let mut inputs = drag(speed, 20);
    inputs.push((lift(), ms(4)));
    let (s, _) = run(TrackballState::default(), cfg, &inputs);
    assert!(s.is_rolling(), "setup: a {speed} units/s drag must fling");
    s
}

/// Tick a state until it stops, or `limit` ticks pass. Returns the state,
/// all actions, and how many ticks it took.
fn roll_out(
    mut s: TrackballState,
    cfg: &TrackballConfig,
    tick: Duration,
    limit: usize,
) -> (TrackballState, Vec<OutputAction>, usize) {
    let mut all = Vec::new();
    for n in 1..=limit {
        let (next, actions) = step(s, Input::Tick, tick, cfg, PAD);
        s = next;
        all.extend(actions);
        if !s.is_rolling() {
            return (s, all, n);
        }
    }
    (s, all, limit)
}

fn roll_of(s: &TrackballState) -> Roll {
    match &s.phase {
        Phase::Rolling(r) => *r,
        other => panic!("expected a rolling ball, got {other:?}"),
    }
}

// ── Finger movement ──────────────────────────────────────────────────────────

#[test]
fn touching_down_moves_nothing() {
    // Wherever the finger lands is where counting starts — an absolute pad
    // position must never become a relative jump.
    let (s, actions) = run(TrackballState::default(), &quiet(), &[(touch(20_000, -15_000), ms(100))]);
    assert!(actions.is_empty());
    assert!(matches!(s.phase, Phase::Touching(_)));
}

#[test]
fn finger_movement_moves_the_pointer_by_sensitivity() {
    let cfg = quiet(); // sensitivity 0.02
    let (_, actions) = run(TrackballState::default(), &cfg, &[
        (touch(0, 0), ms(0)),
        (touch(1000, 500), ms(4)),
    ]);
    // Pad y grows upward, screen y downward.
    assert_eq!(motion(&actions), (20, -10));
    assert_eq!(actions.len(), 1, "a diagonal is one frame with both axes");
}

#[test]
fn slow_movement_still_arrives_through_the_remainder() {
    // 10 raw units at 0.02 is a fifth of a pixel. Truncating each frame on its
    // own would lose it entirely; carrying the remainder must not.
    let mut inputs = vec![(touch(0, 0), ms(0))];
    for i in 1..=50 {
        inputs.push((touch(10 * i, 0), ms(4)));
    }
    let (_, actions) = run(TrackballState::default(), &quiet(), &inputs);
    assert_eq!(motion(&actions), (10, 0));
}

#[test]
fn negative_directions_move_the_pointer_negatively() {
    let (_, actions) = run(TrackballState::default(), &quiet(), &[
        (touch(0, 0), ms(0)),
        (touch(-1000, 1000), ms(4)),
    ]);
    assert_eq!(motion(&actions), (-20, -20));
}

// ── Fling ────────────────────────────────────────────────────────────────────

#[test]
fn a_fast_lift_flings_with_the_release_velocity() {
    let cfg = quiet();
    let mut inputs = drag(300_000.0, 20);
    inputs.push((lift(), ms(4)));
    let (s, _) = run(TrackballState::default(), &cfg, &inputs);
    let r = roll_of(&s);
    assert!((r.vx - 300_000.0).abs() < 300_000.0 * 0.15, "vx = {}", r.vx);
    assert!(r.vy.abs() < 1.0);
}

#[test]
fn fling_direction_follows_the_finger() {
    // Down-left on the pad is down-left on the screen: x negative, and pad y
    // negative means screen y positive.
    let mut inputs = vec![(touch(0, 0), ms(500))];
    for i in 1..=20 {
        inputs.push((touch(-1200 * i, -1200 * i), ms(4)));
    }
    inputs.push((lift(), ms(4)));
    let (s, _) = run(TrackballState::default(), &quiet(), &inputs);
    let r = roll_of(&s);
    assert!(r.vx < 0.0 && r.vy > 0.0, "{r:?}");
}

#[test]
fn a_slow_lift_does_not_fling() {
    let cfg = quiet(); // min_fling_speed 40 000
    let mut inputs = drag(20_000.0, 20);
    inputs.push((lift(), ms(4)));
    let (s, _) = run(TrackballState::default(), &cfg, &inputs);
    assert_eq!(s.phase, Phase::Idle);
}

#[test]
fn a_finger_that_rested_before_lifting_does_not_fling() {
    // Frames are deduped upstream: a finger holding still sends nothing, so
    // the rest shows up only as time passing before the lift.
    let mut inputs = drag(300_000.0, 20);
    inputs.push((lift(), ms(150)));
    let (s, _) = run(TrackballState::default(), &quiet(), &inputs);
    assert_eq!(s.phase, Phase::Idle);
}

#[test]
fn the_lift_frames_own_position_is_ignored() {
    let cfg = quiet();
    let mut a = drag(300_000.0, 20);
    let mut b = a.clone();
    a.push((lift(), ms(4)));
    b.push((Input::Frame(frame(-32_000, 32_000, false, false)), ms(4)));
    let (sa, _) = run(TrackballState::default(), &cfg, &a);
    let (sb, _) = run(TrackballState::default(), &cfg, &b);
    assert_eq!(roll_of(&sa), roll_of(&sb));
}

#[test]
fn a_flick_shorter_than_the_window_still_flings() {
    // Three frames, 12 ms in total — less than the 50 ms window.
    let mut inputs = drag(400_000.0, 3);
    inputs.push((lift(), ms(4)));
    let (s, _) = run(TrackballState::default(), &quiet(), &inputs);
    assert!(roll_of(&s).vx > 200_000.0);
}

#[test]
fn a_tap_with_no_time_and_no_motion_does_not_fling_or_divide_by_zero() {
    let (s, actions) = run(TrackballState::default(), &quiet(), &[
        (touch(100, 100), ms(0)),
        (lift(), ms(0)),
    ]);
    assert_eq!(s.phase, Phase::Idle);
    assert!(actions.is_empty());
}

#[test]
fn min_fling_speed_zero_still_needs_motion_above_stop_speed() {
    let cfg = TrackballConfig { min_fling_speed: 0.0, ..quiet() };
    let (s, _) = run(TrackballState::default(), &cfg, &[
        (touch(0, 0), ms(0)),
        (lift(), ms(10)),
    ]);
    assert_eq!(s.phase, Phase::Idle, "a still finger has nothing to fling");
}

// ── Rolling ──────────────────────────────────────────────────────────────────

#[test]
fn a_rolling_ball_moves_the_pointer_on_ticks() {
    let cfg = quiet();
    let s = rolling(300_000.0, &cfg);
    let (_, actions) = step(s, Input::Tick, ROLL_TICK, &cfg, PAD);
    // 300 000 units/s for 8 ms at 0.02 is about 48 px, a little less after decay.
    let (dx, dy) = motion(&actions);
    assert!((38..=50).contains(&dx), "dx = {dx}");
    assert_eq!(dy, 0);
}

#[test]
fn a_roll_decays_and_stops() {
    let cfg = quiet();
    let s = rolling(300_000.0, &cfg);
    let v0 = roll_of(&s).vx;

    let mut prev = v0;
    let mut state = s;
    for _ in 0..20 {
        let (next, _) = step(state, Input::Tick, ROLL_TICK, &cfg, PAD);
        state = next;
        let v = roll_of(&state).vx;
        assert!(v < prev, "every tick slows the ball");
        prev = v;
    }

    let (end, _, ticks) = roll_out(rolling(300_000.0, &cfg), &cfg, ROLL_TICK, 10_000);
    assert_eq!(end.phase, Phase::Idle);
    // ln(v0 / stop) / friction = ln(200) / 4 ≈ 1.3 s ≈ 165 ticks.
    let expected = ((v0 / cfg.stop_speed).ln() / cfg.friction / ROLL_TICK.as_secs_f64()) as usize;
    assert!(ticks.abs_diff(expected) <= 3, "stopped after {ticks} ticks, expected ~{expected}");
}

#[test]
fn a_roll_travels_v0_over_friction() {
    let cfg = quiet();
    let s = rolling(300_000.0, &cfg);
    let v0 = roll_of(&s).vx;
    let (_, actions, _) = roll_out(s, &cfg, ROLL_TICK, 10_000);
    // The full roll is v0/k raw units; stopping at stop_speed cuts off the
    // last stop_speed/k of it.
    let expected = (v0 - cfg.stop_speed) / cfg.friction * cfg.sensitivity;
    let (dx, _) = motion(&actions);
    assert!((dx as f64 - expected).abs() <= 2.0, "rolled {dx} px, expected {expected:.1}");
}

#[test]
fn distance_does_not_depend_on_the_tick_rate() {
    let cfg = quiet();
    let (_, fine, _) = roll_out(rolling(300_000.0, &cfg), &cfg, ms(4), 100_000);
    let (_, coarse, _) = roll_out(rolling(300_000.0, &cfg), &cfg, ms(16), 100_000);
    let (a, b) = (motion(&fine).0, motion(&coarse).0);
    assert!((a - b).abs() <= 3, "4 ms ticks rolled {a} px, 16 ms ticks {b} px");
}

#[test]
fn zero_friction_rolls_until_touched() {
    let cfg = TrackballConfig { friction: 0.0, ..quiet() };
    let s = rolling(300_000.0, &cfg);
    let v0 = roll_of(&s).vx;
    let (s, _, ticks) = roll_out(s, &cfg, ROLL_TICK, 1250); // 10 s
    assert_eq!(ticks, 1250);
    assert_eq!(roll_of(&s).vx, v0, "no friction, no slowing");
    let (s, _) = step(s, touch(0, 0), ms(1), &cfg, PAD);
    assert!(matches!(s.phase, Phase::Touching(_)));
}

#[test]
fn a_huge_gap_is_clamped_instead_of_jumping() {
    // A suspend in the middle of a roll must not land the rest of it at once.
    let cfg = quiet();
    let s = rolling(300_000.0, &cfg);
    let v = roll_of(&s).vx;
    let (_, actions) = step(s, Input::Tick, Duration::from_secs(30), &cfg, PAD);
    let limit = v * MAX_STEP * cfg.sensitivity;
    assert!((motion(&actions).0 as f64) <= limit + 1.0);
}

#[test]
fn a_zero_length_tick_changes_nothing() {
    let cfg = quiet();
    let s = rolling(300_000.0, &cfg);
    let (next, actions) = step(s.clone(), Input::Tick, Duration::ZERO, &cfg, PAD);
    assert!(actions.is_empty());
    assert_eq!(next, s);
}

// ── Grab and continue ────────────────────────────────────────────────────────

#[test]
fn touching_grabs_the_ball() {
    let cfg = quiet();
    let s = rolling(300_000.0, &cfg);
    let (s, _) = step(s, touch(5000, 5000), Duration::ZERO, &cfg, PAD);
    assert!(matches!(s.phase, Phase::Touching(_)));
    // A tick that was already due changes nothing any more.
    let (s, actions) = step(s, Input::Tick, ROLL_TICK, &cfg, PAD);
    assert!(actions.is_empty());
    assert!(matches!(s.phase, Phase::Touching(_)));
}

#[test]
fn moving_after_a_grab_carries_on_from_there() {
    let cfg = quiet();
    let s = rolling(300_000.0, &cfg);
    // Grab far from where the drag ended: no jump, just a fresh start.
    let (_, actions) = run(s, &cfg, &[
        (touch(-20_000, 10_000), Duration::ZERO),
        (touch(-19_000, 10_000), ms(4)),
    ]);
    assert_eq!(motion(&actions), (20, 0));
}

#[test]
fn a_grab_then_a_still_lift_leaves_the_ball_stopped() {
    let cfg = quiet();
    let s = rolling(300_000.0, &cfg);
    let (s, _) = run(s, &cfg, &[(touch(0, 0), Duration::ZERO), (lift(), ms(80))]);
    assert_eq!(s.phase, Phase::Idle);
}

#[test]
fn a_grab_can_be_flung_again() {
    let cfg = quiet();
    let s = rolling(300_000.0, &cfg);
    let mut inputs = drag(-200_000.0, 20);
    inputs[0].1 = Duration::ZERO;
    inputs.push((lift(), ms(4)));
    let (s, _) = run(s, &cfg, &inputs);
    assert!(roll_of(&s).vx < -100_000.0);
}

// ── Click ────────────────────────────────────────────────────────────────────

#[test]
fn the_click_is_a_left_button_with_its_haptic_chains() {
    let press = HapticChain::single(HapticPulse { duration_us: 10, ..Default::default() });
    let release = HapticChain::single(HapticPulse { duration_us: 20, ..Default::default() });
    let cfg = TrackballConfig { on_press: Some(press.clone()), on_release: Some(release.clone()), ..quiet() };

    let (s, a) = step(TrackballState::default(), Input::Frame(frame(0, 0, true, true)), ms(0), &cfg, PAD);
    assert_eq!(a, vec![button(true), OutputAction::Haptic { pad: PAD, chain: press }]);

    let (s, a) = step(s, Input::Frame(frame(0, 0, true, true)), ms(4), &cfg, PAD);
    assert!(a.is_empty(), "a held button is not sent again");

    let (_, a) = step(s, Input::Frame(frame(0, 0, true, false)), ms(4), &cfg, PAD);
    assert_eq!(a, vec![button(false), OutputAction::Haptic { pad: PAD, chain: release }]);
}

#[test]
fn a_silenced_click_still_clicks() {
    let (_, a) = step(TrackballState::default(), Input::Frame(frame(0, 0, true, true)), ms(0), &quiet(), PAD);
    assert_eq!(a, vec![button(true)]);
}

// ── Interruption (gesture session, Gaming Mode) ──────────────────────────────

#[test]
fn an_interruption_while_dragging_does_not_fling() {
    let cfg = quiet();
    let mut inputs = drag(300_000.0, 20);
    inputs.push((Input::Frame(SinglePadFrame::interrupt()), ms(4)));
    let (s, _) = run(TrackballState::default(), &cfg, &inputs);
    assert_eq!(s.phase, Phase::Idle);
}

#[test]
fn an_interruption_stops_a_rolling_ball() {
    let cfg = TrackballConfig::default(); // with haptics, to see none fire
    let s = rolling(300_000.0, &cfg);
    let (s, actions) = step(s, Input::Frame(SinglePadFrame::interrupt()), Duration::ZERO, &cfg, PAD);
    assert_eq!(s, TrackballState::default());
    assert!(actions.is_empty());
}

#[test]
fn an_interruption_releases_a_held_button_silently() {
    let cfg = TrackballConfig::default();
    let (s, _) = step(TrackballState::default(), Input::Frame(frame(0, 0, true, true)), ms(0), &cfg, PAD);
    let (s, actions) = step(s, Input::Frame(SinglePadFrame::interrupt()), ms(4), &cfg, PAD);
    assert_eq!(actions, vec![button(false)], "released, but no release haptic: nothing was felt");
    assert!(!s.click);
}

#[test]
fn after_an_interruption_the_next_touch_starts_fresh() {
    let cfg = quiet();
    let mut inputs = drag(300_000.0, 5);
    inputs.push((Input::Frame(SinglePadFrame::interrupt()), ms(4)));
    let (interrupted, before) = run(TrackballState::default(), &cfg, &inputs);
    // The survivor pad comes back somewhere else after the gesture.
    let (s, after) = run(interrupted, &cfg, &[(touch(-30_000, -30_000), ms(400))]);
    assert!(after.is_empty(), "the return touch is a touch-down, not a jump");
    assert!(matches!(s.phase, Phase::Touching(_)));
    assert!(motion(&before).0 > 0);
}

// ── Haptics ──────────────────────────────────────────────────────────────────

#[test]
fn roll_ticks_follow_the_speed() {
    let cfg = TrackballConfig { on_press: None, on_release: None, ..Default::default() };
    let window = 12; // ~100 ms
    let fast = {
        let (_, a, _) = roll_out(rolling(300_000.0, &cfg), &cfg, ROLL_TICK, window);
        haptics(&a)
    };
    let slow = {
        let (_, a, _) = roll_out(rolling(80_000.0, &cfg), &cfg, ROLL_TICK, window);
        haptics(&a)
    };
    assert!(fast > slow * 2, "fast roll ticked {fast}×, slow {slow}×");
    assert!(slow > 0);
}

#[test]
fn roll_ticks_thin_out_as_the_ball_slows() {
    let cfg = TrackballConfig { on_press: None, on_release: None, ..Default::default() };
    let mut s = rolling(300_000.0, &cfg);
    let mut counts = Vec::new();
    for _ in 0..4 {
        let (next, a, _) = roll_out(s, &cfg, ROLL_TICK, 25); // 200 ms each
        s = next;
        counts.push(haptics(&a));
    }
    assert!(counts.windows(2).all(|w| w[0] >= w[1]), "{counts:?}");
    assert!(counts[0] > counts[3], "{counts:?}");
}

#[test]
fn roll_ticks_are_at_most_one_per_step() {
    let cfg = TrackballConfig {
        // One-unit cells: every step crosses several.
        grid: MAX_GRID,
        on_press: None,
        on_release: None,
        ..Default::default()
    };
    let (_, a, ticks) = roll_out(rolling(300_000.0, &cfg), &cfg, ROLL_TICK, 50);
    assert_eq!(haptics(&a), ticks);
}

#[test]
fn silenced_roll_ticks_stay_silent() {
    let cfg = quiet();
    let (_, a, _) = roll_out(rolling(300_000.0, &cfg), &cfg, ROLL_TICK, 10_000);
    assert_eq!(haptics(&a), 0);
}

#[test]
fn movement_ticks_are_off_by_default_and_grid_gated_when_on() {
    let off = TrackballConfig { on_press: None, on_release: None, ..Default::default() };
    let (_, a) = run(TrackballState::default(), &off, &drag(300_000.0, 20));
    assert_eq!(haptics(&a), 0);

    let on = TrackballConfig { on_movement: Some(default_tick()), ..off };
    // 0 → 24 000 across 8192-unit cells, starting on a cell's lower edge:
    // into the next cell past 8192 + 10 %, and the one after past 16 384.
    let (_, a) = run(TrackballState::default(), &on, &drag(300_000.0, 20));
    assert_eq!(haptics(&a), 2);
}

/// Grid 8: cells are 8192 raw units, and x = 8192 is the edge between two.
fn grid_on() -> TrackballConfig {
    TrackballConfig { on_movement: Some(default_tick()), ..quiet() }
}

fn drag_through(points: &[(i32, i32)]) -> usize {
    let inputs: Vec<_> = points.iter().map(|&(x, y)| (touch(x, y), ms(4))).collect();
    haptics(&run(TrackballState::default(), &grid_on(), &inputs).1)
}

#[test]
fn a_resting_finger_inside_a_cell_never_ticks() {
    let jitter: Vec<_> = (0..200).map(|i| (4000 + (i % 7) * 300 - 900, 4000 - (i % 5) * 400)).collect();
    assert_eq!(drag_through(&jitter), 0);
}

#[test]
fn crossing_one_boundary_ticks_once() {
    assert_eq!(drag_through(&[(4000, 4000), (6000, 4000), (9500, 4000), (12_000, 4000)]), 1);
    assert_eq!(drag_through(&[(4000, 4000), (4000, -1000)]), 1, "vertical crossings count too");
}

#[test]
fn jitter_on_a_boundary_ticks_at_most_once() {
    let jitter: Vec<_> = (0..200).map(|i| (8192 + if i % 2 == 0 { -700 } else { 700 }, 4000)).collect();
    assert!(drag_through(&jitter) <= 1);
    // Started just below the edge, it stays put.
    assert_eq!(drag_through(&jitter[1..]), 0);
}

#[test]
fn crossing_several_cells_in_one_frame_ticks_once() {
    assert_eq!(drag_through(&[(-30_000, 0), (30_000, 0)]), 1);
}

#[test]
fn the_default_grid_is_8() {
    assert_eq!(TrackballConfig::default().grid, 8);
    let (c, w) = parse("grid = 5\n[haptic]\non_movement = { count = 2 }");
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(c.grid, 5);
    assert_eq!(c.on_movement, Some(HapticChain::Single(HapticPulse { count: 2, ..Default::default() })));
}

#[test]
fn bad_grids_warn_and_fall_back() {
    for bad in ["0", "-3", "\"big\"", "2.5", "70000"] {
        let (c, w) = parse(&format!("grid = {bad}\nfriction = 1.5"));
        assert_eq!(w.len(), 1, "{bad}: {w:?}");
        assert!(w[0].contains("grid"), "{w:?}");
        assert_eq!(c.grid, 8, "{bad}");
        assert_eq!(c.friction, 1.5, "a bad grid leaves the other keys alone");
    }
}

#[test]
fn old_flat_gate_keys_warn_once_and_use_the_key_default() {
    let (c, w) = parse(r#"
        grid = 4
        [haptic]
        on_movement = { grid = 4, duration_us = 4000 }
        on_roll = { pixel_interval = 3000, duration_us = 2000 }
    "#);
    assert_eq!(w.len(), 2, "{w:?}");
    assert!(w[0].contains("`grid`") && w[0].contains("pad section"), "{w:?}");
    assert!(w[1].contains("`pixel_interval`"), "{w:?}");
    assert_eq!(c.on_movement, None, "on_movement's default is off");
    assert_eq!(c.on_roll, Some(default_tick()));
    assert_eq!(c.grid, 4);
}

#[test]
fn every_haptic_key_takes_a_single_event_or_a_list() {
    for key in ["on_press", "on_release", "on_movement", "on_roll"] {
        let (c, w) = parse(&format!("[haptic]\n{key} = {{ duration_us = 1234, count = 2 }}"));
        assert!(w.is_empty(), "{key}: {w:?}");
        let single = HapticChain::Single(HapticPulse { duration_us: 1234, count: 2, ..Default::default() });
        let got = |c: &TrackballConfig| match key {
            "on_press" => c.on_press.clone(),
            "on_release" => c.on_release.clone(),
            "on_movement" => c.on_movement.clone(),
            _ => c.on_roll.clone(),
        };
        assert_eq!(got(&c), Some(single), "{key}");

        let (c, w) = parse(&format!(
            "[haptic]\n{key} = [{{ duration_us = 8000, pause_ms = 40 }}, {{ duration_us = 4000 }}]"
        ));
        assert!(w.is_empty(), "{key}: {w:?}");
        assert!(matches!(got(&c), Some(HapticChain::Chain(ref s)) if s.len() == 2 && s[0].pause_ms == Some(40)), "{key}");
    }
}

#[test]
fn a_pad_inside_a_trackpad_haptic_is_refused() {
    let (c, w) = parse("[haptic]\non_press = { pad = \"left\", duration_us = 100 }");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("pad"), "{w:?}");
    assert_eq!(c.on_press, Some(HapticChain::default()));
}

/// Every haptic in `actions`, as how far the pointer had moved (x) when it
/// fired. With `sensitivity = 1`, pixels are raw units.
fn tick_positions(actions: &[OutputAction]) -> Vec<i64> {
    let mut x = 0;
    let mut at = Vec::new();
    for a in actions {
        match a {
            OutputAction::Haptic { .. } => at.push(x),
            other => x += motion(std::slice::from_ref(other)).0,
        }
    }
    at
}

/// A drag right at 100 raw units per 4 ms frame for `frames` frames from the
/// pad's centre, then a lift, then 8 ms roll ticks with no friction — ticking
/// on the drag and the roll alike.
fn drag_then_roll(grid: u32, frames: usize, roll_steps: usize) -> (Vec<i64>, usize) {
    let cfg = TrackballConfig {
        sensitivity: 1.0,
        friction: 0.0,
        min_fling_speed: 0.0,
        stop_speed: 1.0,
        grid,
        on_movement: Some(default_tick()),
        on_roll: Some(default_tick()),
        ..quiet()
    };
    let mut inputs = vec![(touch(0, 0), ms(500))];
    for i in 1..=frames {
        inputs.push((touch(100 * i as i32, 0), ms(4)));
    }
    let (s, mut actions) = run(TrackballState::default(), &cfg, &inputs);
    let drag_ticks = haptics(&actions);
    let (s, lift_actions) = step(s, lift(), ms(4), &cfg, PAD);
    assert!(s.is_rolling(), "setup: the drag must fling");
    assert_eq!(haptics(&lift_actions), 0, "the lift itself never ticks");
    actions.extend(lift_actions);
    let (_, roll_actions) = run(s, &cfg, &vec![(Input::Tick, ROLL_TICK); roll_steps]);
    actions.extend(roll_actions);
    (tick_positions(&actions), drag_ticks)
}

#[test]
fn one_grid_spaces_drag_and_roll_ticks_alike() {
    // Grid 5: 13 107-unit cells. 300 frames drag 30 000 units, a couple of
    // ticks; the roll (25 000 units/s, 200 per step) carries on for 60 000.
    let (at, drag_ticks) = drag_then_roll(5, 300, 300);
    assert!(drag_ticks >= 2 && at.len() >= drag_ticks + 3, "{drag_ticks} drag ticks of {at:?}");
    let cell = 65_536.0 / 5.0;
    for (i, w) in at.windows(2).enumerate() {
        let gap = (w[1] - w[0]) as f64;
        // Quantised to one frame (100) while dragging, one step (200) rolling.
        assert!((gap - cell).abs() <= 200.0, "gap {i} is {gap}, not one {cell}-unit cell: {at:?}");
    }
}

#[test]
fn the_raster_carries_on_across_the_lift() {
    // Lift just after a drag tick: the first roll tick comes one cell after
    // it, not at the lift and not one cell after the lift.
    let cell = 65_536.0 / 8.0;
    let (at, drag_ticks) = drag_then_roll(8, 100, 100);
    assert_eq!(drag_ticks, 1, "{at:?}");
    let gap = (at[1] - at[0]) as f64;
    assert!((gap - cell).abs() <= 200.0, "drag tick at {}, first roll tick at {}", at[0], at[1]);
}

#[test]
fn the_pixel_interval_key_is_gone() {
    let (_, w) = parse("pixel_interval = 3000");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("pixel_interval"));
}

// ── The handler loop ─────────────────────────────────────────────────────────

/// The loop around `step`: with time paused, a fling must keep moving the
/// pointer after the last frame — the roll tick is what does it — and the
/// handler must still end once its channel closes.
#[tokio::test(start_paused = true)]
async fn run_single_keeps_rolling_without_frames_and_ends_with_its_channel() {
    use crate::output::action::Emission;
    use crate::output::outputs::Tap;

    let (tap_tx, mut tap_rx) = mpsc::channel::<Emission>(4096);
    let outputs = Outputs { tap: Some(Tap::new(tap_tx)), ..Default::default() };
    let (tx, rx) = mpsc::channel(64);

    let driver = async move {
        tx.send(frame(0, 0, true, false)).await.unwrap();
        for i in 1..=20 {
            tokio::time::sleep(ms(4)).await;
            tx.send(frame(1200 * i, 0, true, false)).await.unwrap();
        }
        tokio::time::sleep(ms(4)).await;
        tx.send(frame(0, 0, false, false)).await.unwrap();
        // Long enough for the default roll to finish on its own.
        tokio::time::sleep(Duration::from_secs(3)).await;
        drop(tx);
    };
    tokio::join!(run_single(rx, &outputs, PAD, quiet()), driver);

    let mut actions = Vec::new();
    while let Ok(e) = tap_rx.try_recv() {
        actions.push(e.action);
    }
    let dragged = 20 * 1200 / 50; // 0.02 px per unit
    let (dx, _) = motion(&actions);
    assert!(dx > dragged as i64 * 3, "rolled on after the lift: {dx} px in total vs {dragged} dragged");
}

// ── Config ───────────────────────────────────────────────────────────────────

fn parse(src: &str) -> (TrackballConfig, Vec<String>) {
    TrackballConfig::parse(&toml::from_str::<toml::Value>(src).unwrap())
}

#[test]
fn an_empty_table_is_all_defaults() {
    let (c, w) = parse("");
    assert_eq!(c, TrackballConfig::default());
    assert!(w.is_empty());
    assert_eq!(c.on_roll, Some(default_tick()), "roll ticks are on by default");
    assert_eq!(c.grid, 8);
    assert_eq!(c.on_movement, None);
}

#[test]
fn defaults_are_sane() {
    let c = TrackballConfig::default();
    assert!(c.sensitivity > 0.0 && c.friction > 0.0 && c.stop_speed > 0.0);
    assert!(c.min_fling_speed > c.stop_speed);
}

#[test]
fn core_keys_are_not_mistaken_for_typos() {
    // The shape of the shipped right-pad table, with the mode switched.
    let (_, w) = parse(r#"
        mode = "trackball"
        click_pressure = 30000
        [haptic]
        on_press   = { duration_us = 8000, interval_us = 8000, count = 3, gain_db = 0 }
        on_release = { duration_us = 8000, interval_us = 8000, count = 3, gain_db = 0 }
        [kde]
        pointer_acceleration = 0.2
    "#);
    assert!(w.is_empty(), "{w:?}");
}

#[test]
fn every_key_is_read() {
    let (c, w) = parse(r#"
        sensitivity = 0.05
        friction = 2
        min_fling_speed = 10000
        stop_speed = 500.5
        grid = 4
        [haptic]
        on_press = false
        on_release = [ { duration_us = 100, count = 1, pause_ms = 5 }, { duration_us = 200 } ]
        on_movement = { count = 1 }
        on_roll = { duration_us = 5000, count = 2 }
    "#);
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(c.sensitivity, 0.05);
    assert_eq!(c.friction, 2.0, "an integer is a number too");
    assert_eq!(c.min_fling_speed, 10_000.0);
    assert_eq!(c.stop_speed, 500.5);
    assert_eq!(c.on_press, None);
    assert!(matches!(c.on_release, Some(HapticChain::Chain(ref s)) if s.len() == 2));
    assert_eq!(c.grid, 4);
    assert_eq!(c.on_movement, Some(HapticChain::Single(HapticPulse { count: 1, ..Default::default() })));
    // Fields left out take the pulse defaults, as in a binding's `haptic =`.
    assert_eq!(c.on_roll, Some(HapticChain::Single(HapticPulse { duration_us: 5000, count: 2, ..Default::default() })));
}

#[test]
fn a_bad_number_warns_and_falls_back_on_its_own() {
    let (c, w) = parse(r#"
        sensitivity = -1
        friction = "lots"
        min_fling_speed = -5
        stop_speed = 0
    "#);
    assert_eq!(w.len(), 4, "{w:?}");
    assert_eq!(c, TrackballConfig::default());

    // One bad key leaves the good ones as written.
    let (c, w) = parse("sensitivity = 0\nfriction = 1.5");
    assert_eq!(w.len(), 1);
    assert_eq!(c.sensitivity, DEFAULT_SENSITIVITY);
    assert_eq!(c.friction, 1.5);
}

#[test]
fn non_finite_numbers_are_rejected() {
    let (c, w) = parse("friction = nan\nsensitivity = inf");
    assert_eq!(w.len(), 2, "{w:?}");
    assert_eq!(c.friction, DEFAULT_FRICTION);
    assert_eq!(c.sensitivity, DEFAULT_SENSITIVITY);
}

#[test]
fn zero_friction_and_zero_fling_speed_are_allowed() {
    let (c, w) = parse("friction = 0\nmin_fling_speed = 0");
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(c.friction, 0.0);
    assert_eq!(c.min_fling_speed, 0.0);
}

#[test]
fn unknown_keys_warn() {
    let (c, w) = parse("friciton = 2\n[haptic]\non_rol = false");
    assert_eq!(w.len(), 2, "{w:?}");
    assert!(w[0].contains("friciton"));
    assert_eq!(c, TrackballConfig::default());
}

#[test]
fn bad_haptics_warn_and_fall_back() {
    let (c, w) = parse(r#"
        [haptic]
        on_press = "buzz"
        on_release = { duration_us = -3 }
        on_movement = 7
        on_roll = { pixel_interval = 0 }
    "#);
    assert_eq!(w.len(), 4, "{w:?}");
    assert_eq!(c.on_press, Some(HapticChain::default()));
    assert_eq!(c.on_release, Some(HapticChain::default()));
    assert_eq!(c.on_movement, None, "a bad optional tick stays off");
    assert_eq!(c.on_roll, Some(default_tick()));
}

#[test]
fn roll_ticks_can_be_silenced_and_movement_ticks_switched_on() {
    let (c, w) = parse("[haptic]\non_roll = false\non_movement = true");
    assert!(w.is_empty());
    assert_eq!(c.on_roll, None);
    assert_eq!(c.on_movement, Some(default_tick()));
}

#[test]
fn a_haptic_value_that_is_not_a_table_warns() {
    let (c, w) = parse("haptic = 5");
    assert_eq!(w.len(), 1);
    assert_eq!(c, TrackballConfig::default());
}

#[test]
fn a_config_that_is_not_a_table_is_all_defaults() {
    let (c, w) = TrackballConfig::parse(&toml::Value::Integer(3));
    assert_eq!(w.len(), 1);
    assert_eq!(c, TrackballConfig::default());
}
