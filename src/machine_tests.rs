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
    let plan = plan_emit(&config, &[Key::KEY_A], 1, &[], true, false, true);

    assert_eq!(frames(&plan), vec![(Key::KEY_A.code(), 1)]);
    assert!(plan.toggles.is_empty());
    assert!(plan.modifier_was_activated);
}

/// Several keys are several frames, in order — one `emit` per key, which is one
/// kernel frame per key, not one frame carrying all of them.
#[test]
fn each_key_is_its_own_frame_in_order() {
    let config = make_config(vec![], vec![]);
    let plan = plan_emit(&config, &[Key::KEY_LEFTCTRL, Key::KEY_C], 1, &[], true, false, true);

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
    let plan = plan_emit(&config, &[Key::BTN_TL], 0, &[], true, false, false);

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
    let plan = plan_emit(&config, &[Key::BTN_TL], 0, &[], true, false, true);

    assert!(frames(&plan).is_empty());
}

/// Pressing a custom modifier arms it: nothing is sent, and the "was used" flag
/// is cleared so a release can still decide to tap.
#[test]
fn pressing_a_modifier_arms_it_without_sending() {
    let config = make_config(vec![], vec![ev(Key::BTN_TL)]);
    let plan = plan_emit(&config, &[Key::BTN_TL], 1, &[], true, false, true);

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
    let plan = plan_emit(&config, &[Key::KEY_TAB], 1, &[ev(Key::BTN_TL)], false, true, true);

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
    let plan = plan_emit(&config, &[Key::KEY_TAB], 1, &[ev(Key::BTN_TL)], false, true, true);

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
    let plan = plan_emit(&config, &[Key::KEY_X], 1, &[], true, false, true);

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
    let plan = plan_emit(&config, &[Key::KEY_A], 2, &[ev(Key::BTN_TL)], true, false, true);

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
    let plan = plan_emit(&config, &[Key::KEY_Z], 1, &[ev(Key::BTN_TL)], true, false, true);

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

/// The comparison is silent when the two machines agree, which is what makes a
/// line from it worth reading.
#[test]
fn comparing_identical_results_says_nothing() {
    let config = make_config(vec![], vec![]);
    let plan = plan_emit(&config, &[Key::KEY_A], 1, &[], true, false, true);
    let performed: Vec<OutputAction> = plan.frames.clone();

    // No output to assert on directly; what this pins is that agreement is the
    // quiet path and takes no special casing.
    static TALLY: Tally = Tally::new();
    compare(&TALLY, "test", &plan, &performed, plan.modifier_was_activated, "");
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
