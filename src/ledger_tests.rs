use super::*;

fn ev(k: Key) -> Event {
    Event::Key(k)
}

fn codes(frames: &[OutputAction]) -> Vec<(u16, i32)> {
    frames
        .iter()
        .map(|a| match a {
            OutputAction::Frame { events, .. } => (events[0].code, events[0].value),
            other => panic!("not a frame: {other:?}"),
        })
        .collect()
}

fn keys(keys: &[Key]) -> Pressed {
    Pressed { keys: keys.to_vec(), ..Default::default() }
}

/// A release lifts exactly what its press recorded, and the entry is gone.
#[test]
fn a_release_lifts_what_its_press_recorded() {
    let mut ledger = Ledger::new();
    record(&mut ledger, ev(Key::BTN_SOUTH), keys(&[Key::KEY_LEFTCTRL, Key::KEY_C]));
    let plan = plan_release(&mut ledger, ev(Key::BTN_SOUTH), true);
    assert_eq!(codes(&plan.frames), vec![(Key::KEY_LEFTCTRL.code(), 0), (Key::KEY_C.code(), 0)]);
    assert!(plan.toggles.is_empty());
    assert!(ledger.is_empty());
}

/// Nothing recorded, nothing released — whatever the button resolves to now.
#[test]
fn a_release_without_a_press_sends_nothing() {
    let mut ledger = Ledger::new();
    let plan = plan_release(&mut ledger, ev(Key::BTN_SOUTH), false);
    assert!(plan.frames.is_empty());
    assert!(!plan.modifier_was_activated, "nothing to tap, nothing used");
}

/// No ref-counting: two buttons on one key, the first release lifts it, and
/// the second release sends nothing, because a key that is up is recorded
/// nowhere.
#[test]
fn a_shared_key_goes_up_once() {
    let mut ledger = Ledger::new();
    record(&mut ledger, ev(Key::BTN_SOUTH), keys(&[Key::KEY_LEFTSHIFT]));
    record(&mut ledger, ev(Key::BTN_EAST), keys(&[Key::KEY_LEFTSHIFT]));
    let first = plan_release(&mut ledger, ev(Key::BTN_SOUTH), true);
    assert_eq!(codes(&first.frames), vec![(Key::KEY_LEFTSHIFT.code(), 0)]);
    let second = plan_release(&mut ledger, ev(Key::BTN_EAST), true);
    assert!(second.frames.is_empty());
    assert!(ledger.is_empty());
}

/// A cleanup releases only what is down, and forgets it.
#[test]
fn release_keys_lifts_only_what_is_down() {
    let mut ledger = Ledger::new();
    record(&mut ledger, ev(Key::BTN_SOUTH), keys(&[Key::KEY_A, Key::KEY_B]));
    let frames = release_keys(&mut ledger, &[Key::KEY_B, Key::KEY_Z]);
    assert_eq!(codes(&frames), vec![(Key::KEY_B.code(), 0)]);
    assert_eq!(ledger[&ev(Key::BTN_SOUTH)].keys, vec![Key::KEY_A]);
}

/// An armed modifier released unused taps and is untracked; used, it is only
/// untracked. A combo's armed modifier was never tracked, so it is not
/// untracked either.
#[test]
fn an_armed_modifier_taps_unless_used() {
    let tl = Key::BTN_TL;
    let armed = Pressed { keys: vec![], armed: vec![tl], tracked: true };

    let mut ledger = Ledger::new();
    record(&mut ledger, ev(Key::BTN_SOUTH), armed.clone());
    let plan = plan_release(&mut ledger, ev(Key::BTN_SOUTH), false);
    assert_eq!(codes(&plan.frames), vec![(tl.code(), 1), (tl.code(), 0)]);
    assert_eq!(plan.toggles, vec![(ev(tl), 0)]);
    assert!(plan.modifier_was_activated, "the tap counts as having used it");

    record(&mut ledger, ev(Key::BTN_SOUTH), armed);
    let plan = plan_release(&mut ledger, ev(Key::BTN_SOUTH), true);
    assert!(plan.frames.is_empty());
    assert_eq!(plan.toggles, vec![(ev(tl), 0)]);

    record(&mut ledger, ev(Key::BTN_SOUTH), Pressed { keys: vec![], armed: vec![tl], tracked: false });
    let plan = plan_release(&mut ledger, ev(Key::BTN_SOUTH), true);
    assert!(plan.toggles.is_empty());
}

/// `release_all`, the pause: every key down goes up, and an armed modifier
/// stays recorded for its button's release to untrack.
#[test]
fn release_all_lifts_every_key_and_keeps_armed_modifiers() {
    let mut ledger = Ledger::new();
    record(&mut ledger, ev(Key::BTN_SOUTH), keys(&[Key::KEY_LEFTCTRL, Key::KEY_C]));
    record(&mut ledger, ev(Key::BTN_EAST), keys(&[Key::KEY_A]));
    record(
        &mut ledger,
        ev(Key::BTN_WEST),
        Pressed { keys: vec![Key::KEY_B], armed: vec![Key::BTN_TL], tracked: true },
    );
    let mut sent = codes(&release_all(&mut ledger));
    sent.sort();
    let mut expected = vec![
        (Key::KEY_LEFTCTRL.code(), 0), (Key::KEY_C.code(), 0),
        (Key::KEY_A.code(), 0), (Key::KEY_B.code(), 0),
    ];
    expected.sort();
    assert_eq!(sent, expected);
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[&ev(Key::BTN_WEST)].armed, vec![Key::BTN_TL]);
    assert!(ledger[&ev(Key::BTN_WEST)].keys.is_empty());
}

/// `press`: frames at the value, recorded only on the press itself.
#[test]
fn press_records_only_the_press() {
    let mut ledger = Ledger::new();
    assert_eq!(codes(&press(&mut ledger, ev(Key::BTN_SOUTH), &[Key::KEY_A], 2)), vec![(Key::KEY_A.code(), 2)]);
    assert!(ledger.is_empty(), "a repeat is not a press");
    press(&mut ledger, ev(Key::BTN_SOUTH), &[Key::KEY_A], 1);
    assert_eq!(ledger[&ev(Key::BTN_SOUTH)].keys, vec![Key::KEY_A]);
}
