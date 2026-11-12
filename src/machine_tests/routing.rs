//! `plan_route`: which branch an event takes, and in what order — plus the
//! two Gaming Mode guards it is built from.

use super::*;
use crate::ledger::Ledger;
use std::time::{Duration, Instant};

fn trigger(key: Key, ms: u64) -> crate::config::DoubleclickTrigger {
    crate::config::DoubleclickTrigger { key, ms }
}

fn route_of(
    config: &Config,
    event: Event,
    value: i32,
    gaming_mode: bool,
    paused: bool,
) -> Route {
    plan_route(&Input {
        event, value, config, modifiers: &[], gaming_mode, paused,
        trigger: None, trigger_ts: None, now: Instant::now(),
    })
}

/// A press is tracked so the overlay can highlight it; a release always
/// untracks, even in Gaming Mode, or a button held when the mode came on would
/// stay lit for ever.
#[test]
fn held_keys_tracks_presses_but_always_untracks_releases() {
    assert_eq!(held_update(ev(Key::BTN_SOUTH), 1, false), HeldUpdate::Add);
    assert_eq!(held_update(ev(Key::BTN_SOUTH), 1, true), HeldUpdate::Leave,
               "a press in Gaming Mode is not tracked");
    assert_eq!(held_update(ev(Key::BTN_SOUTH), 0, true), HeldUpdate::Remove,
               "a release is untracked even in Gaming Mode");
    assert_eq!(held_update(Event::Axis(crate::config::Axis::SCROLL_WHEEL_UP), 1, false),
               HeldUpdate::Leave, "only buttons are tracked");
}

/// The key-up cleanup runs for every release, whatever the event resolves to
/// now — the resolution can have changed while the button was held. And it has
/// to be decided without the config, because it is applied before the config is
/// re-resolved.
#[test]
fn a_release_always_clears_what_it_emitted() {
    assert!(releases_emitted(ev(Key::BTN_SOUTH), 0));
    assert!(!releases_emitted(ev(Key::BTN_SOUTH), 1));
    assert!(!releases_emitted(ev(Key::BTN_SOUTH), 2), "a repeat clears nothing");
    assert!(releases_emitted(Event::Axis(Axis::ABS_Z), 0), "an axis edge's release too (#63)");
}

/// The first click arms the window and the event goes on to mean whatever else
/// it means, so a single press of the trigger still does its job.
#[test]
fn the_first_click_arms_without_consuming_the_press() {
    let config = make_config(vec![], vec![]);
    let now = Instant::now();
    let route = plan_route(&Input {
        event: ev(Key::BTN_BASE), value: 1, config: &config, modifiers: &[],
        gaming_mode: false, paused: false,
        trigger: Some(&trigger(Key::BTN_BASE, 400)), trigger_ts: None, now,
    });

    assert_eq!(route.branch, Branch::Resolve, "the press still resolves");
    assert_eq!(route.trigger_ts, Some(now), "and the window is armed");
}

/// The second click inside the window toggles and stops there.
#[test]
fn a_second_click_inside_the_window_toggles() {
    let config = make_config(vec![], vec![]);
    let now = Instant::now();
    let route = plan_route(&Input {
        event: ev(Key::BTN_BASE), value: 1, config: &config, modifiers: &[],
        gaming_mode: false, paused: false,
        trigger: Some(&trigger(Key::BTN_BASE, 400)),
        trigger_ts: Some(now - Duration::from_millis(200)), now,
    });

    assert_eq!(route.branch, Branch::ToggleGamingMode { enabled: true, preview: false });
    assert_eq!(route.trigger_ts, None, "the window closes with the toggle");
}

/// Outside the window it is a first click again, not a toggle.
#[test]
fn a_late_second_click_rearms_instead_of_toggling() {
    let config = make_config(vec![], vec![]);
    let now = Instant::now();
    let route = plan_route(&Input {
        event: ev(Key::BTN_BASE), value: 1, config: &config, modifiers: &[],
        gaming_mode: false, paused: false,
        trigger: Some(&trigger(Key::BTN_BASE, 400)),
        trigger_ts: Some(now - Duration::from_millis(900)), now,
    });

    assert_eq!(route.branch, Branch::Resolve);
    assert_eq!(route.trigger_ts, Some(now));
}

/// The trigger runs before the Gaming Mode guard, which is the only reason the
/// mode can ever be switched off again.
#[test]
fn the_toggle_survives_gaming_mode() {
    let config = make_config(vec![], vec![]);
    let now = Instant::now();
    let route = plan_route(&Input {
        event: ev(Key::BTN_BASE), value: 1, config: &config, modifiers: &[],
        gaming_mode: true, paused: false,
        trigger: Some(&trigger(Key::BTN_BASE, 400)),
        trigger_ts: Some(now - Duration::from_millis(100)), now,
    });

    assert_eq!(route.branch, Branch::ToggleGamingMode { enabled: false, preview: false });
}

/// And while paused, because pause and Gaming Mode are orthogonal — but as a
/// preview, so the overlay shows what it would do without doing it.
#[test]
fn the_toggle_survives_pause_as_a_preview() {
    let config = make_config(vec![], vec![]);
    let now = Instant::now();
    let route = plan_route(&Input {
        event: ev(Key::BTN_BASE), value: 1, config: &config, modifiers: &[],
        gaming_mode: false, paused: true,
        trigger: Some(&trigger(Key::BTN_BASE, 400)),
        trigger_ts: Some(now - Duration::from_millis(100)), now,
    });

    assert_eq!(route.branch, Branch::ToggleGamingMode { enabled: true, preview: true });
}

/// A button that is both a custom modifier and has a base remap takes the
/// intercept: the remap output is emitted as a held system key while the *input*
/// key is tracked, so combos keyed on the button still resolve.
#[test]
fn a_modifier_with_a_remap_takes_the_intercept() {
    let config = make_config(
        vec![(ev(Key::BTN_TL), vec![], vec![Key::KEY_LEFTCTRL])],
        vec![ev(Key::BTN_TL)],
    );
    let route = route_of(&config, ev(Key::BTN_TL), 1, false, false);

    assert_eq!(route.branch, Branch::ModifierIntercept {
        out_keys: vec![Key::KEY_LEFTCTRL], emit: true, announce: true,
    });
}

/// A custom modifier without a base remap does not: there is no output to hold
/// down, so it goes to the resolver like anything else.
#[test]
fn a_modifier_without_a_remap_resolves_normally() {
    let config = make_config(vec![], vec![ev(Key::BTN_TL)]);
    assert_eq!(route_of(&config, ev(Key::BTN_TL), 1, false, false).branch, Branch::Resolve);
}

/// The intercept still routes in Gaming Mode and while paused — a modifier has
/// to keep working — but it emits nothing and announces nothing. The release is
/// never emitted here either way; the key-up cleanup does it, which is what
/// keeps a key from sticking when the mode changed mid-hold.
#[test]
fn the_intercept_routes_but_stays_quiet_when_suppressed() {
    let config = make_config(
        vec![(ev(Key::BTN_TL), vec![], vec![Key::KEY_LEFTCTRL])],
        vec![ev(Key::BTN_TL)],
    );

    let in_gaming = route_of(&config, ev(Key::BTN_TL), 1, true, false);
    assert_eq!(in_gaming.branch, Branch::ModifierIntercept {
        out_keys: vec![Key::KEY_LEFTCTRL], emit: false, announce: false,
    });

    let while_paused = route_of(&config, ev(Key::BTN_TL), 1, false, true);
    assert_eq!(while_paused.branch, Branch::ModifierIntercept {
        out_keys: vec![Key::KEY_LEFTCTRL], emit: false, announce: true,
    });

    let released = route_of(&config, ev(Key::BTN_TL), 0, false, false);
    assert_eq!(released.branch, Branch::ModifierIntercept {
        out_keys: vec![Key::KEY_LEFTCTRL], emit: false, announce: true,
    });
}

/// Gaming Mode swallows an ordinary press, and still tracks it if it is a
/// modifier so combo state is right when the mode goes off again.
#[test]
fn gaming_mode_swallows_a_press_but_keeps_tracking_modifiers() {
    let plain = make_config(vec![], vec![]);
    assert_eq!(route_of(&plain, ev(Key::BTN_SOUTH), 1, true, false).branch,
               Branch::Suppressed { track_modifier: false });

    let with_mod = make_config(vec![], vec![ev(Key::BTN_TR)]);
    assert_eq!(route_of(&with_mod, ev(Key::BTN_TR), 1, true, false).branch,
               Branch::Suppressed { track_modifier: true });
}

/// A release passes the guard, so held_keys and emitted_outputs are flushed and
/// no key can stay stuck when the mode was switched on mid-hold.
#[test]
fn gaming_mode_lets_a_release_through() {
    let config = make_config(vec![], vec![]);
    assert_eq!(route_of(&config, ev(Key::BTN_SOUTH), 0, true, false).branch, Branch::Resolve);
}

/// A binding marked `while_gaming` is not suppressed — that is the whole point
/// of the flag, and it is what the QAM layer relies on.
#[test]
fn a_while_gaming_binding_is_not_suppressed() {
    let mut config = make_config(vec![], vec![]);
    config.bindings.while_gaming.insert((ev(Key::BTN_DPAD_UP), vec![]));

    assert_eq!(
        plan_route(&Input {
            event: ev(Key::BTN_DPAD_UP), value: 1, config: &config, modifiers: &[],
            gaming_mode: true, paused: false,
            trigger: None, trigger_ts: None, now: Instant::now(),
        }).branch,
        Branch::Resolve,
    );
}

/// Pause means makima gets out of the way: a binding sends nothing, not even its
/// bookkeeping. Two of the four cases issue #31 asks for, and they were
/// uncheckable while the guard was two lines at the top of emit_event.
#[test]
fn a_paused_binding_sends_nothing() {
    let config = make_config(vec![], vec![]);
    let plan = plan_emit(&EmitRequest {
        config: &config,
        event: ev(Key::BTN_SOUTH),
        event_list: &[Key::KEY_A],
        value: 1,
        modifiers: &[],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: true,
        no_pause: false,
    }, &mut Ledger::new());

    assert!(frames(&plan).is_empty());
    assert!(plan.toggles.is_empty());
}

/// Unless it is marked `no_pause`, which is what keeps the binding that
/// un-pauses reachable while paused.
#[test]
fn a_no_pause_binding_fires_while_paused() {
    let config = make_config(vec![], vec![]);
    let plan = plan_emit(&EmitRequest {
        config: &config,
        event: ev(Key::BTN_SOUTH),
        event_list: &[Key::KEY_A],
        value: 1,
        modifiers: &[],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: true,
        no_pause: true,
    }, &mut Ledger::new());

    assert_eq!(frames(&plan), vec![(Key::KEY_A.code(), 1)]);
}

/// The predicate on its own, because both callers read it and the double
/// negative in "not paused or no_pause" is easy to get backwards.
#[test]
fn fires_while_paused_is_only_false_when_paused_and_not_exempt() {
    assert!(fires_while_paused(false, false));
    assert!(fires_while_paused(false, true));
    assert!(fires_while_paused(true, true));
    assert!(!fires_while_paused(true, false));
}

// ── gaming_mode_suppresses ────────────────────────────────────────────────

/// Key-up (value=0) must NEVER be suppressed regardless of while_gaming.
/// Without this, releasing a held button in Gaming Mode leaves it stuck.
#[test]
fn key_up_never_suppressed() {
    assert!(!gaming_mode_suppresses(0, false),
        "key-up without while_gaming must not be suppressed");
    assert!(!gaming_mode_suppresses(0, true),
        "key-up with while_gaming must not be suppressed either");
}

/// Key-down of a normal (non-while_gaming) binding must be suppressed.
#[test]
fn key_down_suppressed_without_while_gaming() {
    assert!(gaming_mode_suppresses(1, false),
        "key-down without while_gaming must be suppressed");
}

/// Key-repeat (value=2) of a normal binding must also be suppressed.
#[test]
fn key_repeat_suppressed_without_while_gaming() {
    assert!(gaming_mode_suppresses(2, false),
        "key-repeat without while_gaming must be suppressed");
}

/// while_gaming bindings must pass through on both press and repeat.
#[test]
fn while_gaming_binding_not_suppressed() {
    assert!(!gaming_mode_suppresses(1, true),
        "key-down with while_gaming=true must NOT be suppressed");
    assert!(!gaming_mode_suppresses(2, true),
        "key-repeat with while_gaming=true must NOT be suppressed");
}

// ── gaming_mode_tracks_modifier ───────────────────────────────────────────

/// A mapped modifier (e.g. BTN_TL) must be tracked even in Gaming Mode
/// so combo state is consistent when Gaming Mode is disabled mid-hold.
#[test]
fn modifier_event_is_tracked_in_gaming_mode() {
    let btn_tl = Event::Key(Key::BTN_TL);
    let mapped = vec![btn_tl, Event::Key(Key::BTN_TR)];
    assert!(gaming_mode_tracks_modifier(&btn_tl, &mapped),
        "BTN_TL in mapped_modifiers must return true");
}

/// A non-modifier button must NOT be tracked as a modifier.
#[test]
fn non_modifier_event_not_tracked_in_gaming_mode() {
    let btn_south = Event::Key(Key::BTN_SOUTH);
    let mapped = vec![Event::Key(Key::BTN_TL), Event::Key(Key::BTN_TR)];
    assert!(!gaming_mode_tracks_modifier(&btn_south, &mapped),
        "BTN_SOUTH not in mapped_modifiers must return false");
}

/// Empty mapped_modifiers list → nothing is ever tracked.
#[test]
fn no_mapped_modifiers_nothing_tracked() {
    let btn_tl = Event::Key(Key::BTN_TL);
    assert!(!gaming_mode_tracks_modifier(&btn_tl, &[]),
        "empty mapped_modifiers must never match");
}
