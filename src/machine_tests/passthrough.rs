//! `plan_default`: passing an event straight through.

use super::*;

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
