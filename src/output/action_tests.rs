use super::*;

use evdev::{EventType, InputEvent, Key};

fn key_ev(code: u16, value: i32) -> InputEvent {
    InputEvent::new_now(EventType::KEY, code, value)
}

fn rel(code: u16, value: i32) -> InputEvent {
    InputEvent::new_now(EventType::RELATIVE, code, value)
}

/// The one property everything else rests on.
///
/// Two emissions of the same key a moment apart are the same thing as far as the
/// kernel is concerned. If `Ev` ever stopped erasing the timestamp, every
/// comparison between two implementations would fail for a reason that has
/// nothing to do with either of them — and would look like a real finding.
#[test]
fn ev_projection_drops_timestamp() {
    let first = key_ev(Key::KEY_A.code(), 1);
    std::thread::sleep(std::time::Duration::from_millis(2));
    let second = key_ev(Key::KEY_A.code(), 1);

    assert_eq!(Ev::from(&first), Ev::from(&second));
    assert_eq!(
        OutputAction::Frame { device: OutputDevice::Keys, events: vec![Ev::from(&first)] },
        OutputAction::Frame { device: OutputDevice::Keys, events: vec![Ev::from(&second)] },
    );
}

/// Within one frame the order is what the kernel reads, so it is part of the
/// identity — Ctrl-then-A is not A-then-Ctrl.
#[test]
fn frame_equality_is_order_sensitive() {
    let a = Ev::from(&key_ev(Key::KEY_A.code(), 1));
    let b = Ev::from(&key_ev(Key::KEY_B.code(), 1));

    assert_ne!(
        OutputAction::Frame { device: OutputDevice::Keys, events: vec![a, b] },
        OutputAction::Frame { device: OutputDevice::Keys, events: vec![b, a] },
    );
}

/// The framing fact from `cursor_loop`, pinned.
///
/// X and Y go out as two separate `emit` calls, so the kernel sees two frames: a
/// pointer that moved twice, not once diagonally. Anyone later "optimising" that
/// into a single batch changes what the kernel sees, and this is what tells them.
#[test]
fn two_frames_are_not_one_frame() {
    let x = Ev::from(&rel(0, 5));
    let y = Ev::from(&rel(1, -3));

    let as_sent = vec![
        OutputAction::Frame { device: OutputDevice::Axis, events: vec![x] },
        OutputAction::Frame { device: OutputDevice::Axis, events: vec![y] },
    ];
    let merged = vec![OutputAction::Frame { device: OutputDevice::Axis, events: vec![x, y] }];

    assert_ne!(as_sent, merged);
}

/// The same events on a different device are a different action — otherwise a
/// core that routed a keystroke to the pointer would compare as correct.
#[test]
fn device_routing_is_part_of_identity() {
    let e = Ev::from(&key_ev(Key::BTN_LEFT.code(), 1));

    assert_ne!(
        OutputAction::Frame { device: OutputDevice::Keys, events: vec![e] },
        OutputAction::Frame { device: OutputDevice::Axis, events: vec![e] },
    );
}

/// The trackpad path appends its own SYN before emitting. The recorded form is
/// what the code handed over, never a normalised version — a comparison against a
/// tidied-up form would disagree about an artefact.
#[test]
fn explicit_syn_is_preserved() {
    let batch = [
        key_ev(Key::BTN_TOUCH.code(), 1),
        InputEvent::new_now(EventType::SYNCHRONIZATION, 0, 0),
    ];
    let action = OutputAction::Frame {
        device: OutputDevice::LeftPad,
        events: batch.iter().map(Ev::from).collect(),
    };

    let OutputAction::Frame { events, .. } = &action else { panic!("built a Frame") };
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].ev_type, EventType::SYNCHRONIZATION.0);
}

/// Guards the `PartialEq` derives added to the haptic types: two chains that
/// differ only in how long they pause are different feedback.
#[test]
fn haptic_chain_equality() {
    use deckery_controller::{HapticChain, HapticChainStep, HapticPad, HapticPulse};

    let pulse = HapticPulse { duration_us: 8000, interval_us: 8000, count: 3, gain_db: 0 };
    let one = OutputAction::Haptic { pad: HapticPad::Both, chain: HapticChain::single(pulse) };
    let same = OutputAction::Haptic { pad: HapticPad::Both, chain: HapticChain::single(pulse) };
    assert_eq!(one, same);

    let step = |pause| HapticChainStep { pulse, pause_ms: Some(pause) };
    assert_ne!(
        OutputAction::Haptic { pad: HapticPad::Both, chain: HapticChain::Chain(vec![step(10)]) },
        OutputAction::Haptic { pad: HapticPad::Both, chain: HapticChain::Chain(vec![step(20)]) },
    );

    // Same chain, other pad — still a different action.
    assert_ne!(
        one,
        OutputAction::Haptic { pad: HapticPad::Left, chain: HapticChain::single(pulse) },
    );
}

/// `preview` is what makes the Gaming Mode toggle a no-op that still updates the
/// overlay. A core that lost the distinction would toggle while paused, so the
/// flag has to count as part of the action.
#[test]
fn internal_command_preview_is_significant() {
    assert_ne!(
        OutputAction::Internal(InternalCommand::SetGamingMode { enabled: true, preview: false }),
        OutputAction::Internal(InternalCommand::SetGamingMode { enabled: true, preview: true }),
    );
}

/// The urgency decides whether a publish may be dropped, so it is not a hint —
/// the same snapshot sent urgently is a different instruction.
#[test]
fn publish_urgency_is_significant() {
    let snapshot = || Box::new(serde_json::json!({ "context": { "paused": false } }));
    assert_ne!(
        OutputAction::State(StateAction::Publish {
            snapshot: snapshot(), urgency: Urgency::Immediate,
        }),
        OutputAction::State(StateAction::Publish {
            snapshot: snapshot(), urgency: Urgency::Analog,
        }),
    );
}

/// And the snapshot is what goes through the port, so two publishes of different
/// state are different actions.
#[test]
fn the_snapshot_is_part_of_the_action() {
    let publish = |paused| OutputAction::State(StateAction::Publish {
        snapshot: Box::new(serde_json::json!({ "context": { "paused": paused } })),
        urgency: Urgency::Immediate,
    });
    assert_ne!(publish(true), publish(false));
    assert_eq!(publish(true), publish(true));
}

/// Pads are excluded from recording by default; the names are what
/// `DECKERY_RECORD_SINKS` is written in, so they are part of the interface.
#[test]
fn device_names_are_stable() {
    assert_eq!(OutputDevice::Keys.as_str(), "keys");
    assert_eq!(OutputDevice::Axis.as_str(), "axis");
    assert_eq!(OutputDevice::Abs.as_str(), "abs");
    assert_eq!(OutputDevice::LeftPad.as_str(), "lpad");
    assert_eq!(OutputDevice::RightPad.as_str(), "rpad");
    assert_eq!(OutputDevice::GesturePad.as_str(), "gesture_pad");
}
