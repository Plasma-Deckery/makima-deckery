use super::*;
use crate::config::{Bindings, MappedModifiers};

use std::collections::{HashMap, HashSet};

fn ev(k: Key) -> Event {
    Event::Key(k)
}

/// A config with the given base remaps and custom modifiers.
///
/// Deliberately hand-built rather than parsed: these tests are about what the
/// machine decides, and a TOML round-trip would put a second thing between the
/// assertion and the decision.
fn make_config(remap: Vec<(Event, Vec<Event>, Vec<Key>)>, custom_modifiers: Vec<Event>) -> Config {
    let mut remap_map: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
    for (trigger, combo, keys) in remap {
        remap_map.entry(trigger).or_default().insert(combo, keys);
    }
    let all = custom_modifiers.clone();
    Config {
        name: "test".to_string(),
        bindings: Bindings {
            remap: remap_map,
            commands: HashMap::new(),
            movements: HashMap::new(),
            no_pause: HashSet::new(),
            while_gaming: HashSet::new(),
            labels: HashMap::new(),
            silent: HashSet::new(),
            ..Default::default()
        },
        override_bindings: None,
        settings: HashMap::new(),
        mapped_modifiers: MappedModifiers { default: vec![], custom: custom_modifiers, all },
        trackpad: Default::default(),
        gaming_mode_config: Default::default(),
        device: None,
        module: Default::default(),
        aliases: Default::default(),
    }
}

/// The frames of a plan as (code, value) pairs, which is what the assertions are
/// actually about.
fn frames(plan: &EmitPlan) -> Vec<(u16, i32)> {
    plan.frames
        .iter()
        .flat_map(|a| match a {
            OutputAction::Frame { events, .. } => events.iter().map(|e| (e.code, e.value)).collect::<Vec<_>>(),
            _ => vec![],
        })
        .collect()
}

/// The ordinary case: a plain key with nothing held.
#[test]
fn a_plain_key_is_one_frame() {
    let config = make_config(vec![], vec![]);
    let plan = plan_emit(&EmitRequest {
        config: &config,
        event_list: &[Key::KEY_A],
        value: 1,
        modifiers: &[],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    });

    assert_eq!(frames(&plan), vec![(Key::KEY_A.code(), 1)]);
    assert!(plan.toggles.is_empty());
    assert!(plan.modifier_was_activated);
}

/// Several keys are several frames, in order — one `emit` per key, which is one
/// kernel frame per key, not one frame carrying all of them.
#[test]
fn each_key_is_its_own_frame_in_order() {
    let config = make_config(vec![], vec![]);
    let plan = plan_emit(&EmitRequest {
        config: &config,
        event_list: &[Key::KEY_LEFTCTRL, Key::KEY_C],
        value: 1,
        modifiers: &[],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    });

    assert_eq!(
        frames(&plan),
        vec![(Key::KEY_LEFTCTRL.code(), 1), (Key::KEY_C.code(), 1)],
    );
    assert_eq!(plan.frames.len(), 2, "two frames, not one batch");
}

/// A custom modifier held and let go without being used reads as a tap: press
/// and release, two frames. This is what makes a paddle usable both as a
/// modifier and as a button.
#[test]
fn an_unused_modifier_taps_on_release() {
    let config = make_config(vec![], vec![ev(Key::BTN_TL)]);
    let plan = plan_emit(&EmitRequest {
        config: &config,
        event_list: &[Key::BTN_TL],
        value: 0,
        modifiers: &[],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: false,
        paused: false,
        no_pause: false,
    });

    assert_eq!(
        frames(&plan),
        vec![(Key::BTN_TL.code(), 1), (Key::BTN_TL.code(), 0)],
        "a tap is a press followed by a release",
    );
    assert!(plan.modifier_was_activated, "the tap counts as having used it");
}

/// Once the modifier *was* used, releasing it sends nothing — otherwise every
/// combo would be followed by a stray tap.
#[test]
fn a_used_modifier_sends_nothing_on_release() {
    let config = make_config(vec![], vec![ev(Key::BTN_TL)]);
    let plan = plan_emit(&EmitRequest {
        config: &config,
        event_list: &[Key::BTN_TL],
        value: 0,
        modifiers: &[],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    });

    assert!(frames(&plan).is_empty());
}

/// Pressing a custom modifier arms it: nothing is sent, and the "was used" flag
/// is cleared so a release can still decide to tap.
#[test]
fn pressing_a_modifier_arms_it_without_sending() {
    let config = make_config(vec![], vec![ev(Key::BTN_TL)]);
    let plan = plan_emit(&EmitRequest {
        config: &config,
        event_list: &[Key::BTN_TL],
        value: 1,
        modifiers: &[],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    });

    assert!(frames(&plan).is_empty());
    assert!(!plan.modifier_was_activated);
    assert_eq!(plan.toggles, vec![(ev(Key::BTN_TL), 1)]);
}

/// An explicit combo consumed its modifier, so the modifier's *output* key is
/// released first — `KEY_LEFTCTRL`, not the raw `BTN_TL` code.
#[test]
fn ignore_modifiers_releases_the_output_key_not_the_input() {
    let config = make_config(
        vec![(ev(Key::BTN_TL), vec![], vec![Key::KEY_LEFTCTRL])],
        vec![ev(Key::BTN_TL)],
    );
    let plan = plan_emit(&EmitRequest {
        config: &config,
        event_list: &[Key::KEY_TAB],
        value: 1,
        modifiers: &[ev(Key::BTN_TL)],
        release_keys: false,
        ignore_modifiers: true,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    });

    assert_eq!(
        frames(&plan),
        vec![(Key::KEY_LEFTCTRL.code(), 0), (Key::KEY_TAB.code(), 1)],
        "the modifier's output goes down before the combo key goes out",
    );
}

/// With no remap behind it, a modifier releases its own code — there is nothing
/// else it could mean.
#[test]
fn ignore_modifiers_falls_back_to_the_modifier_code() {
    let config = make_config(vec![], vec![ev(Key::BTN_TL)]);
    let plan = plan_emit(&EmitRequest {
        config: &config,
        event_list: &[Key::KEY_TAB],
        value: 1,
        modifiers: &[ev(Key::BTN_TL)],
        release_keys: false,
        ignore_modifiers: true,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    });

    assert_eq!(frames(&plan), vec![(Key::BTN_TL.code(), 0), (Key::KEY_TAB.code(), 1)]);
}

/// With no modifiers held there is nothing to clean up, and the release branch
/// must not run — releasing everything the empty set resolves to would knock out
/// a system modifier that merely shares a key code with a base remap output.
#[test]
fn nothing_is_released_when_no_modifier_is_held() {
    let config = make_config(
        vec![(ev(Key::BTN_TR), vec![], vec![Key::KEY_LEFTSHIFT])],
        vec![ev(Key::BTN_TR)],
    );
    let plan = plan_emit(&EmitRequest {
        config: &config,
        event_list: &[Key::KEY_X],
        value: 1,
        modifiers: &[],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    });

    assert_eq!(frames(&plan), vec![(Key::KEY_X.code(), 1)]);
}

/// A key repeat (`value == 2`) neither releases held modifiers nor touches the
/// modifier set — it only repeats.
#[test]
fn a_repeat_does_not_release_or_track() {
    let config = make_config(
        vec![(ev(Key::BTN_TL), vec![], vec![Key::KEY_LEFTCTRL])],
        vec![ev(Key::BTN_TL)],
    );
    let plan = plan_emit(&EmitRequest {
        config: &config,
        event_list: &[Key::KEY_A],
        value: 2,
        modifiers: &[ev(Key::BTN_TL)],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    });

    assert_eq!(frames(&plan), vec![(Key::KEY_A.code(), 2)]);
    assert!(plan.toggles.is_empty());
}

/// The release branch only releases keys that are modifiers themselves. A plain
/// remap output that happens to resolve under the same modifier set stays put.
#[test]
fn only_modifier_outputs_are_released() {
    let config = make_config(
        vec![
            // Resolves under [BTN_TL] but is not a modifier — must not be released.
            (ev(Key::BTN_SOUTH), vec![ev(Key::BTN_TL)], vec![Key::KEY_ENTER]),
            // Is a modifier, so it is.
            (ev(Key::BTN_TR), vec![ev(Key::BTN_TL)], vec![Key::BTN_TR]),
        ],
        vec![ev(Key::BTN_TL), ev(Key::BTN_TR)],
    );
    let plan = plan_emit(&EmitRequest {
        config: &config,
        event_list: &[Key::KEY_Z],
        value: 1,
        modifiers: &[ev(Key::BTN_TL)],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    });

    let sent = frames(&plan);
    assert!(sent.contains(&(Key::BTN_TR.code(), 0)), "the modifier output was released: {sent:?}");
    assert!(!sent.contains(&(Key::KEY_ENTER.code(), 0)), "a plain output was released too: {sent:?}");
}

/// `released_keys` reads only the config, which is why it could be lifted out of
/// the reader unchanged.
#[test]
fn released_keys_resolves_the_held_set() {
    let config = make_config(
        vec![(ev(Key::BTN_SOUTH), vec![ev(Key::BTN_TL)], vec![Key::KEY_ENTER])],
        vec![ev(Key::BTN_TL)],
    );

    assert_eq!(released_keys(&[ev(Key::BTN_TL)], &config), vec![Key::KEY_ENTER]);
    assert!(released_keys(&[ev(Key::BTN_TR)], &config).is_empty());
}

// ── emit_default_event: passing an event straight through ─────────────────

fn raw(ev_type: EventType, code: u16, value: i32) -> evdev::InputEvent {
    evdev::InputEvent::new_now(ev_type, code, value)
}

fn forwarded_to(plan: &[OutputAction]) -> Vec<OutputDevice> {
    plan.iter()
        .filter_map(|a| match a {
            OutputAction::Forward { device, .. } => Some(*device),
            _ => None,
        })
        .collect()
}

/// Each event type has exactly one device it belongs on. MISC goes to the pen
/// device with ABSOLUTE, because a pen's MSC_SERIAL is part of the same report
/// as its position.
#[test]
fn each_event_type_goes_to_its_own_device() {
    let cases = [
        (EventType::KEY, OutputDevice::Keys),
        (EventType::RELATIVE, OutputDevice::Axis),
        (EventType::ABSOLUTE, OutputDevice::Abs),
        (EventType::MISC, OutputDevice::Abs),
    ];
    for (ev_type, device) in cases {
        let plan = plan_default(&raw(ev_type, 1, 1), false);
        assert_eq!(forwarded_to(&plan), vec![device], "{ev_type:?} went to the wrong device");
    }
}

/// An incoming SYN is consumed for rate limiting and never passed on — `emit`
/// appends one of its own, so forwarding it would mean two frames for one.
#[test]
fn a_synchronisation_event_is_not_forwarded() {
    let plan = plan_default(&raw(EventType::SYNCHRONIZATION, 0, 0), false);
    assert!(plan.is_empty());
}

/// Paused swallows the passthrough entirely. This is the whole of what pause
/// means here: makima stops being in the way, and also stops forwarding.
#[test]
fn paused_forwards_nothing() {
    let plan = plan_default(&raw(EventType::KEY, 30, 1), true);
    assert!(plan.is_empty());
}

/// A forwarded event keeps the timestamp it arrived with — that is the reason
/// `Forward` exists rather than reusing `Frame`, because libinput reads
/// timestamps for velocity and a restamped pen event is a different event.
#[test]
fn a_forwarded_event_keeps_its_own_timestamp() {
    let event = raw(EventType::ABSOLUTE, 0, 1234);
    let plan = plan_default(&event, false);

    let OutputAction::Forward { events, .. } = &plan[0] else { panic!("expected a Forward") };
    assert_eq!(events[0].0.timestamp(), event.timestamp());
}

/// …and two forwards of the same event compare equal despite different
/// timestamps, because the timestamp was never part of the decision.
#[test]
fn forwards_compare_without_their_timestamps() {
    let first = plan_default(&raw(EventType::KEY, 30, 1), false);
    std::thread::sleep(std::time::Duration::from_millis(2));
    let second = plan_default(&raw(EventType::KEY, 30, 1), false);

    assert_eq!(first, second);
}

/// A forward is not a frame, even for the same event on the same device: one
/// carries a stamp that must be kept, the other gets one when it goes out.
#[test]
fn a_forward_is_not_a_frame() {
    let event = raw(EventType::KEY, 30, 1);
    let forward = plan_default(&event, false);
    let frame = vec![OutputAction::Frame {
        device: OutputDevice::Keys,
        events: vec![Ev::from(&event)],
    }];

    assert_ne!(forward, frame);
}

// ── the stick loops ───────────────────────────────────────────────────────

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

// ── routing: which branch an event takes, and in what order ───────────────

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
        event_list: &[Key::KEY_A],
        value: 1,
        modifiers: &[],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: true,
        no_pause: false,
    });

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
        event_list: &[Key::KEY_A],
        value: 1,
        modifiers: &[],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: true,
        no_pause: true,
    });

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

// ── binding_haptic ───────────────────────────────────────────────────────────

fn click() -> crate::config::BindingHaptic {
    crate::config::BindingHaptic {
        pad: deckery_controller::HapticPad::Right,
        chain: deckery_controller::HapticChain::Single(Default::default()),
    }
}

/// A press with a haptic plays it, on the side the binding names.
#[test]
fn a_press_plays_the_binding_haptic() {
    let h = click();
    assert_eq!(
        binding_haptic(Some(&h), 1, false, false),
        Some(OutputAction::Haptic { pad: h.pad, chain: h.chain.clone() }),
    );
}

/// Only the press: a repeat or a release is not a click.
#[test]
fn repeat_and_release_play_nothing() {
    let h = click();
    assert_eq!(binding_haptic(Some(&h), 2, false, false), None, "repeat buzzed");
    assert_eq!(binding_haptic(Some(&h), 0, false, false), None, "release buzzed");
}

/// A press swallowed by pause sends nothing, so it must not feel like it did —
/// unless the binding is `no_pause` and really fires.
#[test]
fn pause_silences_it_unless_the_binding_fires_anyway() {
    let h = click();
    assert_eq!(binding_haptic(Some(&h), 1, true, false), None);
    assert!(binding_haptic(Some(&h), 1, true, true).is_some());
}

/// No haptic configured, nothing played.
#[test]
fn no_haptic_plays_nothing() {
    assert_eq!(binding_haptic(None, 1, false, false), None);
}

// ── pause keeps what is held (issue #62) ──────────────────────────────────
//
// These run whole sequences — press, pause, resume, combo, release — because
// the bug was in how the decisions add up, not in any one of them. `Reader`
// below is the reader's state plus `convert_event`'s applying of the pure
// decisions for key events, in the reader's order: `held_update`, the release
// of `emitted_outputs`, `plan_route`, the intercept, `resolve_binding`,
// `plan_emit`, and for an unbound event `paused_nonmapped` then
// `plan_nonmapped`. Pause is `plan_pause` as `pause_release_outputs` applies it;
// resume is what the IPC handler does — flip the flag and apply
// `resume_modifier_was_activated`, sending nothing.

const L1: Key = Key::BTN_TL;
const STEAM: Key = Key::BTN_MODE;
const X: Key = Key::BTN_NORTH;
const L4: Key = Key::BTN_TRIGGER_HAPPY1;
const L2: Key = Key::BTN_TL2;

/// The shape of the shipped base config, reduced to what these tests need: L1
/// is a layer key (`L1 = []` plus `L1-X`), Steam a modifier with no binding of
/// its own (`STEAM-X`), L4 a remap to Shift, L2 the right mouse button with
/// `no_pause`.
fn layered_config() -> Config {
    let mut config = make_config(
        vec![
            (ev(L1), vec![], vec![]),
            (ev(X), vec![], vec![Key::KEY_X]),
            (ev(X), vec![ev(L1)], vec![Key::KEY_LEFTCTRL, Key::KEY_C]),
            (ev(X), vec![ev(STEAM)], vec![Key::KEY_F5]),
            (ev(L4), vec![], vec![Key::KEY_LEFTSHIFT]),
            (ev(L2), vec![], vec![Key::BTN_RIGHT]),
        ],
        vec![ev(L1), ev(STEAM)],
    );
    config.bindings.no_pause.insert((ev(L2), vec![]));
    config
}

struct Reader {
    config: Config,
    modifiers: Vec<Event>,
    held_keys: Vec<Event>,
    emitted: HashMap<Event, Vec<Key>>,
    modifier_was_activated: bool,
    paused: bool,
    /// Every key frame sent, as (code, value), until `take` empties it.
    sent: Vec<(u16, i32)>,
}

impl Reader {
    fn new(config: Config) -> Self {
        Reader {
            config,
            modifiers: vec![],
            held_keys: vec![],
            emitted: HashMap::new(),
            modifier_was_activated: true,
            paused: false,
            sent: vec![],
        }
    }

    fn send(&mut self, actions: &[OutputAction]) {
        for action in actions {
            match action {
                OutputAction::Frame { events, .. } => {
                    self.sent.extend(events.iter().map(|e| (e.code, e.value)))
                }
                OutputAction::Forward { events, .. } => {
                    self.sent.extend(events.iter().map(|e| (e.0.code(), e.0.value())))
                }
                _ => {}
            }
        }
    }

    fn take(&mut self) -> Vec<(u16, i32)> {
        std::mem::take(&mut self.sent)
    }

    /// `toggle_modifiers`.
    fn toggle(&mut self, modifier: Event, value: i32) {
        if self.config.mapped_modifiers.all.contains(&modifier) {
            match value {
                1 => {
                    self.modifiers.push(modifier);
                    self.modifiers.sort();
                    self.modifiers.dedup();
                }
                0 => self.modifiers.retain(|&m| m != modifier),
                _ => {}
            }
        }
    }

    fn key(&mut self, key: Key, value: i32) {
        let event = ev(key);
        match held_update(event, value, false) {
            HeldUpdate::Add => {
                self.held_keys.push(event);
                self.held_keys.sort();
                self.held_keys.dedup();
            }
            HeldUpdate::Remove => self.held_keys.retain(|&k| k != event),
            HeldUpdate::Leave => {}
        }
        let mut released = Vec::new();
        if releases_emitted(event, value) {
            if let Some(prev) = self.emitted.remove(&event) {
                let frames: Vec<_> = prev.iter().map(|k| key_up(*k)).collect();
                self.send(&frames);
                released = prev;
            }
        }

        let config = self.config.clone();
        let mut modifiers = self.modifiers.clone();
        modifiers.sort();
        modifiers.dedup();
        let route = plan_route(&Input {
            event, value, config: &config, modifiers: &modifiers,
            gaming_mode: false, paused: self.paused,
            trigger: None, trigger_ts: None, now: Instant::now(),
        });
        match route.branch {
            Branch::ModifierIntercept { out_keys, emit, .. } => {
                if emit {
                    let frames: Vec<_> = out_keys.iter().map(|k| key_at(*k, value)).collect();
                    self.send(&frames);
                    if value == 1 {
                        self.emitted.insert(event, out_keys.clone());
                    }
                }
                self.toggle(event, value);
                return;
            }
            Branch::Resolve => {}
            other => panic!("not modelled here: {other:?}"),
        }

        match crate::resolver::resolve_binding(&config.bindings, event, &modifiers, false) {
            crate::resolver::ResolvedBinding::Keys { keys, no_pause, is_combo, is_fallback, .. } => {
                let plan = plan_emit(&EmitRequest {
                    config: &config,
                    event_list: &keys,
                    value,
                    modifiers: &modifiers,
                    release_keys: !is_combo && !is_fallback,
                    ignore_modifiers: is_combo,
                    modifier_was_activated: self.modifier_was_activated,
                    paused: self.paused,
                    no_pause,
                });
                self.modifier_was_activated = plan.modifier_was_activated;
                self.send(&skip_released(plan.frames, &released));
                for (m, v) in plan.toggles {
                    self.toggle(m, v);
                }
                // As the reader does it: after every emit but the release.
                if value != 0 {
                    self.emitted.insert(event, keys);
                }
            }
            crate::resolver::ResolvedBinding::Unbound => {
                let live = self.modifiers.clone();
                match paused_nonmapped(&config, event, &live, self.paused) {
                    Some(true) => {
                        self.toggle(event, value);
                        return;
                    }
                    Some(false) => return,
                    None => {}
                }
                let default_event = raw(EventType::KEY, key.code(), value);
                let plan = plan_nonmapped(
                    &config, event, &default_event, value, &modifiers,
                    self.modifier_was_activated,
                );
                self.modifier_was_activated = plan.modifier_was_activated;
                self.send(&plan.frames);
                for (m, v) in plan.toggles {
                    self.toggle(m, v);
                }
            }
            other => panic!("not modelled here: {other:?}"),
        }
    }

    fn press(&mut self, key: Key) {
        self.key(key, 1);
    }

    fn release(&mut self, key: Key) {
        self.key(key, 0);
    }

    /// The IPC `pause`.
    fn pause(&mut self) {
        self.paused = true;
        let plan = plan_pause(
            &self.emitted, &self.modifiers,
            &self.config.mapped_modifiers.custom, self.modifier_was_activated,
        );
        self.emitted.clear();
        self.modifier_was_activated = plan.modifier_was_activated;
        self.send(&plan.frames);
    }

    /// The IPC `resume`: the flag, and the used-mark for a held modifier.
    /// Nothing is sent.
    fn resume(&mut self) {
        self.paused = false;
        self.modifier_was_activated = resume_modifier_was_activated(
            &self.modifiers, &self.config.mapped_modifiers.custom,
            self.modifier_was_activated,
        );
    }
}

/// The codes pressed among `sent`. A tap is a press, so is a re-pressed output,
/// and neither may happen after a pause; the releases around them are not the
/// question, because a modifier's press and release send key-ups for whatever
/// the held set resolves to (`plan_nonmapped`), pause or no pause.
fn presses(sent: &[(u16, i32)]) -> Vec<u16> {
    sent.iter().filter(|&&(_, v)| v != 0).map(|&(c, _)| c).collect()
}

/// The reported bug: L1 held while the HUD opens is still L1 while it is open,
/// and after it closes L1 + X is the L1 layer's combo, not plain X. Nothing is
/// sent at resume, and letting go of L1 afterwards sends nothing either.
#[test]
fn a_layer_key_held_across_a_pause_keeps_its_layer() {
    let mut r = Reader::new(layered_config());
    r.press(L1);
    assert_eq!(r.take(), vec![], "L1 = [] sends nothing");

    r.pause();
    assert_eq!(r.modifiers, vec![ev(L1)], "the pause forgot L1");
    assert_eq!(r.held_keys, vec![ev(L1)], "the HUD would show L1 as released");
    assert!(r.emitted.is_empty(), "pause leaves nothing recorded as emitted");

    r.resume();
    assert_eq!(r.take(), vec![], "resume sent something");

    r.press(X);
    assert_eq!(
        r.take(),
        vec![(Key::KEY_LEFTCTRL.code(), 1), (Key::KEY_C.code(), 1)],
        "L1 + X after the pause has to be the L1 layer's combo",
    );
    r.release(X);
    r.take();

    r.release(L1);
    assert_eq!(r.take(), vec![], "releasing L1 after the pause sent something");
    assert!(r.modifiers.is_empty());
    assert!(r.held_keys.is_empty());
}

/// The control case from the report, which already worked and must keep
/// working: L1 pressed *during* the pause is tracked, and resolves its layer
/// after it.
#[test]
fn a_layer_key_pressed_during_a_pause_still_works() {
    let mut r = Reader::new(layered_config());
    r.pause();
    r.press(L1);
    assert_eq!(r.modifiers, vec![ev(L1)]);
    assert_eq!(r.held_keys, vec![ev(L1)]);
    r.resume();
    assert_eq!(r.take(), vec![]);

    r.press(X);
    assert_eq!(
        r.take(),
        vec![(Key::KEY_LEFTCTRL.code(), 1), (Key::KEY_C.code(), 1)],
    );
}

/// The baseline the next test is measured against: Steam held and released on
/// its own, with no pause, is a tap — the press and release of its own code.
#[test]
fn an_unused_steam_press_taps() {
    let mut r = Reader::new(layered_config());
    r.press(STEAM);
    r.release(STEAM);
    let sent = r.take();
    assert_eq!(presses(&sent), vec![STEAM.code()], "{sent:?}");
    assert!(sent.ends_with(&[(STEAM.code(), 1), (STEAM.code(), 0)]), "{sent:?}");
}

/// Steam held across the pause and used in the HUD: the combo sends nothing
/// while paused, so nothing marked Steam as used — the pause does. Releasing it
/// after the HUD closes is not a tap.
#[test]
fn a_modifier_held_across_a_pause_does_not_tap_on_release() {
    let mut r = Reader::new(layered_config());
    r.press(STEAM);
    assert!(!r.modifier_was_activated, "precondition: Steam is armed");

    r.pause();
    assert!(r.modifier_was_activated, "the pause has to count Steam as used");
    r.press(X);
    r.release(X);
    assert_eq!(presses(&r.take()), Vec::<u16>::new(), "a combo fired while paused");
    r.resume();

    r.release(STEAM);
    assert_eq!(presses(&r.take()), Vec::<u16>::new(), "Steam tapped after the pause");
    assert!(r.modifiers.is_empty());
}

/// The same with nothing pressed in between: holding Steam while the HUD is
/// open and letting go afterwards is not a tap either.
#[test]
fn a_modifier_only_held_across_a_pause_does_not_tap() {
    let mut r = Reader::new(layered_config());
    r.press(STEAM);
    r.pause();
    r.resume();
    r.release(STEAM);
    assert_eq!(presses(&r.take()), Vec::<u16>::new(), "Steam tapped after the pause");
}

/// A pause with no custom modifier held leaves the flag as it was: it says
/// something about a held modifier, and there is none.
#[test]
fn a_pause_without_a_held_modifier_leaves_the_flag_alone() {
    let config = layered_config();
    let plan = plan_pause(&HashMap::new(), &[], &config.mapped_modifiers.custom, false);
    assert!(!plan.modifier_was_activated);
    assert!(plan.frames.is_empty());
}

/// L4 → Shift held across the pause: Shift goes up at the pause and stays up —
/// resume does not press it again. Letting go of L4 afterwards sends only a
/// release, and only one: `emitted_outputs` no longer holds L4, so the reader's
/// cleanup sends nothing and only the binding's own release goes out (which is
/// for a key already up). An ordinary release sends one as well: the cleanup's,
/// with the binding's repeat of it dropped.
#[test]
fn a_remapped_key_held_across_a_pause_stays_released() {
    let shift = Key::KEY_LEFTSHIFT.code();
    let mut r = Reader::new(layered_config());
    r.press(L4);
    assert_eq!(r.take(), vec![(shift, 1)]);

    r.pause();
    assert_eq!(r.take(), vec![(shift, 0)], "the pause has to release Shift");
    assert_eq!(r.held_keys, vec![ev(L4)], "L4 is still held");

    r.resume();
    assert_eq!(r.take(), vec![], "resume pressed Shift again");

    r.release(L4);
    let sent = r.take();
    assert!(sent.iter().all(|&(_, v)| v == 0), "a release pressed something: {sent:?}");
    assert_eq!(sent, vec![(shift, 0)], "the cleanup released Shift a second time");
    assert!(r.held_keys.is_empty());

    // For comparison, the same release with no pause in between.
    let mut plain = Reader::new(layered_config());
    plain.press(L4);
    plain.take();
    plain.release(L4);
    assert_eq!(plain.take(), vec![(shift, 0)]);
}

/// Everything makima emitted goes up at the pause, whichever button emitted it
/// and whether or not its binding is `no_pause` — a drag on L2 is dropped
/// rather than left holding the right mouse button in another window.
#[test]
fn a_pause_still_releases_everything_emitted() {
    let mut r = Reader::new(layered_config());
    r.press(L4);
    r.press(L2);
    r.press(L1);
    r.take();

    r.pause();
    let mut sent = r.take();
    sent.sort();
    let mut expected = vec![(Key::KEY_LEFTSHIFT.code(), 0), (Key::BTN_RIGHT.code(), 0)];
    expected.sort();
    assert_eq!(sent, expected);
    assert!(r.emitted.is_empty());
    assert_eq!(r.modifiers, vec![ev(L1)]);
}

/// `release_emitted` on its own: one key-up per recorded output key.
#[test]
fn release_emitted_releases_every_recorded_key() {
    let mut emitted = HashMap::new();
    emitted.insert(ev(Key::BTN_SOUTH), vec![Key::KEY_LEFTCTRL, Key::KEY_C]);
    emitted.insert(ev(Key::BTN_EAST), vec![Key::KEY_A]);
    let mut codes: Vec<_> = release_emitted(&emitted)
        .iter()
        .map(|a| match a {
            OutputAction::Frame { events, .. } => (events[0].code, events[0].value),
            other => panic!("not a frame: {other:?}"),
        })
        .collect();
    codes.sort();
    let mut expected = vec![
        (Key::KEY_LEFTCTRL.code(), 0), (Key::KEY_C.code(), 0), (Key::KEY_A.code(), 0),
    ];
    expected.sort();
    assert_eq!(codes, expected);
}

/// The paused branch for unbound events, now decided in the machine: a
/// modifier is tracked, anything else only shown, and `no_pause` exempts it —
/// for the exact combo or the bare button.
#[test]
fn paused_nonmapped_tracks_modifiers_and_honours_no_pause() {
    let mut config = layered_config();
    assert_eq!(paused_nonmapped(&config, ev(STEAM), &[], false), None, "not paused");
    assert_eq!(paused_nonmapped(&config, ev(STEAM), &[], true), Some(true));
    assert_eq!(paused_nonmapped(&config, ev(Key::BTN_SOUTH), &[], true), Some(false));

    config.bindings.no_pause.insert((ev(Key::BTN_SOUTH), vec![ev(L1)]));
    assert_eq!(paused_nonmapped(&config, ev(Key::BTN_SOUTH), &[ev(L1)], true), None);
    assert_eq!(paused_nonmapped(&config, ev(Key::BTN_SOUTH), &[], true), Some(false));
    config.bindings.no_pause.insert((ev(Key::BTN_SOUTH), vec![]));
    assert_eq!(paused_nonmapped(&config, ev(Key::BTN_SOUTH), &[ev(L4)], true), None);
}

/// Steam pressed *during* the pause and released after it: the press was
/// swallowed, so the release must not tap either. The flag has to be false at
/// the pause for this to matter, which is rare — it starts true, and a pause
/// with a custom modifier held sets it — so the test puts it there directly,
/// as if an unused modifier press were pending.
#[test]
fn a_modifier_pressed_during_a_pause_does_not_tap_after_it() {
    let mut r = Reader::new(layered_config());
    r.modifier_was_activated = false;

    r.pause();
    r.press(STEAM);
    assert!(!r.modifier_was_activated, "precondition: still armed while paused");
    r.resume();
    assert!(r.modifier_was_activated, "resume has to count the held Steam as used");
    assert_eq!(r.take(), vec![], "pause or resume sent something");

    r.release(STEAM);
    assert_eq!(presses(&r.take()), Vec::<u16>::new(), "Steam tapped after the pause");
    assert!(r.modifiers.is_empty());
}

/// Resume with no custom modifier held leaves the flag as it was.
#[test]
fn a_resume_without_a_held_modifier_leaves_the_flag_alone() {
    let config = layered_config();
    let custom = &config.mapped_modifiers.custom;
    assert!(!resume_modifier_was_activated(&[], custom, false));
    assert!(resume_modifier_was_activated(&[ev(STEAM)], custom, false));
}

/// A plain remap, pressed and released: one key-down, one key-up. The reader's
/// cleanup releases what the press emitted, and the binding's own release must
/// not send the same key-up a second time. Nothing stays recorded as emitted
/// either, or the next pause would release the key yet again.
#[test]
fn a_plain_remap_release_sends_one_key_up() {
    let shift = Key::KEY_LEFTSHIFT.code();
    let mut r = Reader::new(layered_config());
    r.press(L4);
    assert_eq!(r.take(), vec![(shift, 1)]);
    r.release(L4);
    assert_eq!(r.take(), vec![(shift, 0)], "one release, two key-ups");
    assert!(r.emitted.is_empty(), "the release left L4 recorded as emitted");

    r.pause();
    assert_eq!(r.take(), vec![], "a pause after the release sent a stale key-up");
}

/// The combo while a layer key is held: L1 + X is Ctrl+C, and letting go of X
/// releases Ctrl and C once each.
#[test]
fn a_combo_release_with_a_held_modifier_sends_one_key_up_each() {
    let ctrl = Key::KEY_LEFTCTRL.code();
    let c = Key::KEY_C.code();
    let mut r = Reader::new(layered_config());
    r.press(L1);
    r.press(X);
    assert_eq!(r.take(), vec![(ctrl, 1), (c, 1)]);
    r.release(X);
    let mut sent = r.take();
    sent.sort();
    let mut expected = vec![(ctrl, 0), (c, 0)];
    expected.sort();
    assert_eq!(sent, expected, "the combo's keys were released twice");
    assert!(!r.emitted.contains_key(&ev(X)), "the release left X recorded as emitted");
    assert_eq!(r.modifiers, vec![ev(L1)], "L1 is still held");
}

/// `skip_released` on its own: a key-up for a key the cleanup already released
/// is dropped, unless the frames press it again first (a tap's release is a
/// release of its own), and everything else is left as it is.
#[test]
fn skip_released_drops_only_repeated_key_ups() {
    let a = Key::KEY_A;
    let b = Key::KEY_B;
    let frames = vec![key_up(a), key_up(b)];
    assert_eq!(skip_released(frames, &[a]), vec![key_up(b)]);

    let tap = vec![key_up(a), key_at(a, 1), key_up(a)];
    assert_eq!(skip_released(tap, &[a]), vec![key_at(a, 1), key_up(a)]);

    let untouched = vec![key_at(a, 1), key_up(b)];
    assert_eq!(skip_released(untouched.clone(), &[]), untouched);
}
