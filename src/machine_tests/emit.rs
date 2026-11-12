//! `plan_emit` and `released_keys`: what a resolved key binding sends, and
//! what it records in the ledger.

use super::*;
use crate::ledger::{self, Ledger, Pressed};

#[test]
fn a_plain_key_is_one_frame() {
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
        paused: false,
        no_pause: false,
    }, &mut Ledger::new());

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
        event: ev(Key::BTN_SOUTH),
        event_list: &[Key::KEY_LEFTCTRL, Key::KEY_C],
        value: 1,
        modifiers: &[],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    }, &mut Ledger::new());

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
    let mut ledger = Ledger::new();
    let press = plan_emit(&arm_tl(&config), &mut ledger);
    let plan = ledger::plan_release(&mut ledger, ev(Key::BTN_SOUTH), press.modifier_was_activated);

    let sent: Vec<_> = plan.frames.iter().map(frame_code).collect();
    assert_eq!(
        sent,
        vec![(Key::BTN_TL.code(), 1), (Key::BTN_TL.code(), 0)],
        "a tap is a press followed by a release",
    );
    assert_eq!(plan.toggles, vec![(ev(Key::BTN_TL), 0)], "the release untracks it");
    assert!(plan.modifier_was_activated, "the tap counts as having used it");
}

/// The press of a base binding to `BTN_TL`, a custom modifier: armed, tracked.
fn arm_tl(config: &Config) -> EmitRequest<'_> {
    EmitRequest {
        config,
        event: ev(Key::BTN_SOUTH),
        event_list: &[Key::BTN_TL],
        value: 1,
        modifiers: &[],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    }
}

fn frame_code(action: &OutputAction) -> (u16, i32) {
    match action {
        OutputAction::Frame { events, .. } => (events[0].code, events[0].value),
        other => panic!("not a frame: {other:?}"),
    }
}

/// Once the modifier *was* used, releasing it sends nothing — otherwise every
/// combo would be followed by a stray tap.
#[test]
fn a_used_modifier_sends_nothing_on_release() {
    let config = make_config(vec![], vec![ev(Key::BTN_TL)]);
    let mut ledger = Ledger::new();
    plan_emit(&arm_tl(&config), &mut ledger);
    let plan = ledger::plan_release(&mut ledger, ev(Key::BTN_SOUTH), true);

    assert!(plan.frames.is_empty());
    assert_eq!(plan.toggles, vec![(ev(Key::BTN_TL), 0)]);
}

/// A release is the ledger's, so `plan_emit` plans nothing for one — not the
/// key-ups of what the binding resolves to now, which may never have been
/// pressed.
#[test]
fn plan_emit_plans_nothing_for_a_release() {
    let config = make_config(vec![], vec![]);
    let plan = plan_emit(&EmitRequest {
        config: &config,
        event: ev(Key::BTN_SOUTH),
        event_list: &[Key::KEY_A],
        value: 0,
        modifiers: &[],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    }, &mut Ledger::new());

    assert!(plan.frames.is_empty());
    assert!(plan.toggles.is_empty());
}

/// What a press sends down is what it records: plain keys as keys, a custom
/// modifier as armed.
#[test]
fn a_press_records_what_it_pressed() {
    let config = make_config(vec![], vec![ev(Key::BTN_TL)]);
    let mut ledger = Ledger::new();
    plan_emit(&EmitRequest {
        config: &config,
        event: ev(Key::BTN_SOUTH),
        event_list: &[Key::KEY_LEFTCTRL, Key::BTN_TL],
        value: 1,
        modifiers: &[],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    }, &mut ledger);

    assert_eq!(
        ledger[&ev(Key::BTN_SOUTH)],
        Pressed { keys: vec![Key::KEY_LEFTCTRL], armed: vec![Key::BTN_TL], tracked: true },
    );
}

/// Pressing a custom modifier arms it: nothing is sent, and the "was used" flag
/// is cleared so a release can still decide to tap.
#[test]
fn pressing_a_modifier_arms_it_without_sending() {
    let config = make_config(vec![], vec![ev(Key::BTN_TL)]);
    let plan = plan_emit(&EmitRequest {
        config: &config,
        event: ev(Key::BTN_SOUTH),
        event_list: &[Key::BTN_TL],
        value: 1,
        modifiers: &[],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    }, &mut Ledger::new());

    assert!(frames(&plan).is_empty());
    assert!(!plan.modifier_was_activated);
    assert_eq!(plan.toggles, vec![(ev(Key::BTN_TL), 1)]);
}

/// An explicit combo consumed its modifier, so the modifier's *output* key is
/// released first — `KEY_LEFTCTRL`, not the raw `BTN_TL` code — and the ledger
/// forgets it, so letting go of the modifier later does not release it again.
#[test]
fn ignore_modifiers_releases_the_output_key_not_the_input() {
    let config = make_config(
        vec![(ev(Key::BTN_TL), vec![], vec![Key::KEY_LEFTCTRL])],
        vec![ev(Key::BTN_TL)],
    );
    // As the modifier intercept records it.
    let mut ledger = Ledger::new();
    ledger::press(&mut ledger, ev(Key::BTN_TL), &[Key::KEY_LEFTCTRL], 1);
    let plan = plan_emit(&EmitRequest {
        config: &config,
        event: ev(Key::BTN_SOUTH),
        event_list: &[Key::KEY_TAB],
        value: 1,
        modifiers: &[ev(Key::BTN_TL)],
        release_keys: false,
        ignore_modifiers: true,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    }, &mut ledger);

    assert_eq!(
        frames(&plan),
        vec![(Key::KEY_LEFTCTRL.code(), 0), (Key::KEY_TAB.code(), 1)],
        "the modifier's output goes down before the combo key goes out",
    );
    assert!(!ledger.contains_key(&ev(Key::BTN_TL)), "Ctrl is still recorded as down");
}

/// With no remap behind it, a modifier releases its own code — there is nothing
/// else it could mean — but only if it is down. A custom modifier is armed,
/// not pressed, so there is nothing to release; a passed-through one is.
#[test]
fn ignore_modifiers_falls_back_to_the_modifier_code() {
    let config = make_config(vec![], vec![ev(Key::BTN_TL)]);
    let request = |ledger: &mut Ledger| plan_emit(&EmitRequest {
        config: &config,
        event: ev(Key::BTN_SOUTH),
        event_list: &[Key::KEY_TAB],
        value: 1,
        modifiers: &[ev(Key::BTN_TL)],
        release_keys: false,
        ignore_modifiers: true,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    }, ledger);

    assert_eq!(frames(&request(&mut Ledger::new())), vec![(Key::KEY_TAB.code(), 1)]);

    let mut ledger = Ledger::new();
    ledger::press(&mut ledger, ev(Key::BTN_TL), &[Key::BTN_TL], 1);
    assert_eq!(
        frames(&request(&mut ledger)),
        vec![(Key::BTN_TL.code(), 0), (Key::KEY_TAB.code(), 1)],
    );
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
        event: ev(Key::BTN_SOUTH),
        event_list: &[Key::KEY_X],
        value: 1,
        modifiers: &[],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    }, &mut Ledger::new());

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
        event: ev(Key::BTN_SOUTH),
        event_list: &[Key::KEY_A],
        value: 2,
        modifiers: &[ev(Key::BTN_TL)],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    }, &mut Ledger::new());

    assert_eq!(frames(&plan), vec![(Key::KEY_A.code(), 2)]);
    assert!(plan.toggles.is_empty());
}

/// The release branch only releases keys that are modifiers themselves. A plain
/// remap output that happens to resolve under the same modifier set stays put.
/// Both are down here, recorded by hand: which of them goes up is the question.
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
    let mut ledger = Ledger::new();
    ledger::press(&mut ledger, ev(Key::BTN_EAST), &[Key::BTN_TR, Key::KEY_ENTER], 1);
    let plan = plan_emit(&EmitRequest {
        config: &config,
        event: ev(Key::BTN_SOUTH),
        event_list: &[Key::KEY_Z],
        value: 1,
        modifiers: &[ev(Key::BTN_TL)],
        release_keys: true,
        ignore_modifiers: false,
        modifier_was_activated: true,
        paused: false,
        no_pause: false,
    }, &mut ledger);

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

/// With no modifier held there is nothing to release — not the base output of
/// every binding, which would end a held drag.
#[test]
fn released_keys_of_no_modifiers_is_empty() {
    let config = make_config(
        vec![(ev(Key::BTN_TR2), vec![], vec![Key::BTN_LEFT])],
        vec![ev(Key::BTN_TL)],
    );

    assert!(released_keys(&[], &config).is_empty());
}
