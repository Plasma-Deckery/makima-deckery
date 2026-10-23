//! The exact shape of a pad frame.
//!
//! These frames are what libinput reads to decide whether something was a tap,
//! a drag, a two-finger scroll or a pinch, and the rules are the kernel's
//! multi-touch protocol rather than anything this project chose. Before issue
//! #57 none of it was checkable without a Deck: the frame was built inside a
//! `VirtualDevices` lock and handed straight to `/dev/uinput`.

use super::*;

/// The events of the single frame a planner returns.
fn events_of(actions: &[OutputAction]) -> Vec<Ev> {
    assert_eq!(actions.len(), 1, "one emit() call is one kernel frame, never two");
    match &actions[0] {
        OutputAction::Frame { events, .. } => events.clone(),
        other => panic!("expected a Frame, got {other:?}"),
    }
}

fn device_of(actions: &[OutputAction]) -> OutputDevice {
    match &actions[0] {
        OutputAction::Frame { device, .. } => *device,
        other => panic!("expected a Frame, got {other:?}"),
    }
}

fn value_of(events: &[Ev], ev_type: EventType, code: u16) -> Option<i32> {
    events.iter()
        .find(|e| e.ev_type == ev_type.0 && e.code == code)
        .map(|e| e.value)
}

// ── One pad ───────────────────────────────────────────────────────────────────

#[test]
fn a_touching_frame_carries_both_the_mt_slot_and_the_compat_pair() {
    let events = events_of(&plan_pad_frame(OutputDevice::RightPad, 500, 300, true, None));

    assert_eq!(value_of(&events, EventType::ABSOLUTE, abs::MT_TRACKING_ID), Some(1));
    assert_eq!(value_of(&events, EventType::ABSOLUTE, abs::MT_POSITION_X), Some(500));
    assert_eq!(value_of(&events, EventType::ABSOLUTE, abs::X), Some(500),
        "a reader that does not speak MT still needs ABS_X");
    assert_eq!(value_of(&events, EventType::KEY, Key::BTN_TOUCH.code()), Some(1));
    assert_eq!(value_of(&events, EventType::KEY, Key::BTN_TOOL_FINGER.code()), Some(1));
}

#[test]
fn y_is_inverted_in_both_the_mt_and_the_compat_axis() {
    let events = events_of(&plan_pad_frame(OutputDevice::LeftPad, 0, 300, true, None));
    assert_eq!(value_of(&events, EventType::ABSOLUTE, abs::MT_POSITION_Y), Some(-300),
        "the Deck counts Y upward, libinput downward");
    assert_eq!(value_of(&events, EventType::ABSOLUTE, abs::Y), Some(-300));
}

#[test]
fn a_lift_frame_releases_the_slot_and_sends_no_position() {
    let events = events_of(&plan_pad_frame(OutputDevice::RightPad, 500, 300, false, None));

    assert_eq!(value_of(&events, EventType::ABSOLUTE, abs::MT_TRACKING_ID), Some(SLOT_LIFTED));
    assert_eq!(value_of(&events, EventType::ABSOLUTE, abs::MT_POSITION_X), None,
        "a lifted slot has no position, and sending one would be a phantom touch");
    assert_eq!(value_of(&events, EventType::KEY, Key::BTN_TOUCH.code()), Some(0));
}

#[test]
fn a_frame_that_says_nothing_about_the_button_does_not_release_it() {
    let silent = events_of(&plan_pad_frame(OutputDevice::RightPad, 0, 0, true, None));
    assert_eq!(value_of(&silent, EventType::KEY, Key::BTN_LEFT.code()), None);

    let released = events_of(&plan_pad_frame(OutputDevice::RightPad, 0, 0, true, Some(false)));
    assert_eq!(value_of(&released, EventType::KEY, Key::BTN_LEFT.code()), Some(0),
        "Some(false) is an explicit release and must be sent");

    let pressed = events_of(&plan_pad_frame(OutputDevice::RightPad, 0, 0, true, Some(true)));
    assert_eq!(value_of(&pressed, EventType::KEY, Key::BTN_LEFT.code()), Some(1));
}

#[test]
fn the_explicit_syn_is_last() {
    let events = events_of(&plan_pad_frame(OutputDevice::RightPad, 0, 0, true, Some(true)));
    let last = events.last().unwrap();
    assert_eq!((last.ev_type, last.code), (EventType::SYNCHRONIZATION.0, 0),
        "emit() appends its own SYN per call, so this one must survive the move \
         into an action or the kernel sees a different frame");
}

#[test]
fn the_pad_decides_its_own_device() {
    assert_eq!(device_of(&plan_pad_frame(OutputDevice::LeftPad, 0, 0, true, None)),
               OutputDevice::LeftPad);
    assert_eq!(PadState::new(true).device(), OutputDevice::LeftPad);
    assert_eq!(PadState::new(false).device(), OutputDevice::RightPad);
}

// ── Both pads as one gesture ──────────────────────────────────────────────────

#[test]
fn each_pad_maps_into_its_own_half_of_the_combined_x_space() {
    let events = events_of(&plan_gesture_frame(0, 0, 0, 0, true, true, None));

    // Slot 0 is the left pad, slot 1 the right. The positions arrive in slot
    // order, so reading them in order reads them per pad.
    let xs: Vec<i32> = events.iter()
        .filter(|e| e.ev_type == EventType::ABSOLUTE.0 && e.code == abs::MT_POSITION_X)
        .map(|e| e.value)
        .collect();

    assert_eq!(xs, vec![-AXIS_MAX / 2, AXIS_MAX / 2],
        "a centred finger on each pad must sit either side of zero, or pinch \
         distance would not grow as the fingers separate");
}

#[test]
fn two_fingers_report_doubletap_and_one_reports_finger() {
    let both = events_of(&plan_gesture_frame(0, 0, 0, 0, true, true, None));
    assert_eq!(value_of(&both, EventType::KEY, Key::BTN_TOOL_DOUBLETAP.code()), Some(1));
    assert_eq!(value_of(&both, EventType::KEY, Key::BTN_TOOL_FINGER.code()), Some(0),
        "two fingers are not one finger");

    let one = events_of(&plan_gesture_frame(0, 0, 0, 0, true, false, None));
    assert_eq!(value_of(&one, EventType::KEY, Key::BTN_TOOL_DOUBLETAP.code()), Some(0));
    assert_eq!(value_of(&one, EventType::KEY, Key::BTN_TOOL_FINGER.code()), Some(1));
    assert_eq!(value_of(&one, EventType::KEY, Key::BTN_TOUCH.code()), Some(1));
}

#[test]
fn ending_a_gesture_lifts_both_slots() {
    let events = events_of(&plan_gesture_frame(0, 0, 0, 0, false, false, Some(false)));

    let slots: Vec<i32> = events.iter()
        .filter(|e| e.ev_type == EventType::ABSOLUTE.0 && e.code == abs::MT_SLOT)
        .map(|e| e.value)
        .collect();
    let ids: Vec<i32> = events.iter()
        .filter(|e| e.ev_type == EventType::ABSOLUTE.0 && e.code == abs::MT_TRACKING_ID)
        .map(|e| e.value)
        .collect();

    assert_eq!(slots, vec![0, 1], "both slots must be addressed");
    assert_eq!(ids, vec![SLOT_LIFTED, SLOT_LIFTED], "and both must be released");
    assert_eq!(value_of(&events, EventType::KEY, Key::BTN_TOUCH.code()), Some(0));
    assert_eq!(value_of(&events, EventType::KEY, Key::BTN_TOOL_DOUBLETAP.code()), Some(0));
    assert_eq!(value_of(&events, EventType::KEY, Key::BTN_TOOL_FINGER.code()), Some(0));
}

#[test]
fn a_lifted_slot_in_a_gesture_keeps_the_other_ones_position() {
    // The left finger lifts mid-gesture while the right stays down.
    let events = events_of(&plan_gesture_frame(0, 0, 1000, 500, false, true, None));

    let ids: Vec<i32> = events.iter()
        .filter(|e| e.ev_type == EventType::ABSOLUTE.0 && e.code == abs::MT_TRACKING_ID)
        .map(|e| e.value)
        .collect();
    assert_eq!(ids, vec![SLOT_LIFTED, 2], "slot 0 lifts, slot 1 keeps tracking id 2");
    assert_eq!(value_of(&events, EventType::ABSOLUTE, abs::MT_POSITION_Y), Some(-500),
        "the surviving slot still reports where it is");
}

#[test]
fn a_gesture_frame_goes_to_the_gesture_device() {
    assert_eq!(device_of(&plan_gesture_frame(0, 0, 0, 0, true, true, None)),
               OutputDevice::GesturePad);
}
