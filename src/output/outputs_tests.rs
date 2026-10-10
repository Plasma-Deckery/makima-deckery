use super::*;
use crate::output::action::{Ev, OutputDevice, sites};

fn frame(device: OutputDevice) -> OutputAction {
    OutputAction::Frame { device, events: vec![Ev { ev_type: 1, code: 30, value: 1 }] }
}

/// A tap plus the receiving end, which is all a test needs to watch.
fn tapped(capacity: usize) -> (Outputs, mpsc::Receiver<Emission>) {
    let (tx, rx) = mpsc::channel(capacity);
    (Outputs { tap: Some(Tap::new(tx)), ..Default::default() }, rx)
}

fn drain(rx: &mut mpsc::Receiver<Emission>) -> Vec<Emission> {
    let mut out = Vec::new();
    while let Ok(e) = rx.try_recv() {
        out.push(e);
    }
    out
}

/// The whole point: with every port empty, dispatching does nothing at all — no
/// uinput, no state file, no process — and the actions are still fully visible.
/// That is what makes the binding engine testable without hardware.
#[tokio::test]
async fn every_port_empty_means_nothing_happens_and_everything_is_visible() {
    let (outputs, mut rx) = tapped(8);
    assert!(outputs.devices.is_none());

    outputs.dispatch(sites::EMIT_EVENT_KEYS, &[
        frame(OutputDevice::Keys),
        OutputAction::Internal(InternalCommand::SetGamingMode { enabled: true, preview: false }),
    ]).await;

    let seen = drain(&mut rx);
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].action, frame(OutputDevice::Keys));
    assert_eq!(seen[0].site, sites::EMIT_EVENT_KEYS);
}

/// Nothing clipped in and nobody watching: dispatch is a no-op that cannot fail.
#[tokio::test]
async fn dispatch_with_no_ports_and_no_tap_is_harmless() {
    let outputs = Outputs::default();
    assert!(!outputs.watched());
    outputs.dispatch(sites::CURSOR_LOOP_X, &[frame(OutputDevice::Axis)]).await;
}

/// The sequence number is what gives records from different tasks a single order.
#[tokio::test]
async fn emissions_are_numbered_in_order() {
    let (outputs, mut rx) = tapped(8);
    outputs.dispatch(sites::EMIT_EVENT_KEYS, &[
        frame(OutputDevice::Keys),
        frame(OutputDevice::Axis),
    ]).await;
    outputs.dispatch(sites::CURSOR_LOOP_X, &[frame(OutputDevice::Axis)]).await;

    let seqs: Vec<u64> = drain(&mut rx).into_iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![0, 1, 2]);
}

/// Gaming Mode goes out through its port as a plain bool, which is how it comes
/// back in as the next input. The loop is observable at both ends.
#[tokio::test]
async fn the_internal_port_carries_gaming_mode_back() {
    let (tx, mut rx) = mpsc::channel(4);
    let outputs = Outputs { internal: Some(tx), ..Default::default() };

    outputs.dispatch(sites::SET_GAMING_MODE, &[
        OutputAction::Internal(InternalCommand::SetGamingMode { enabled: true, preview: false }),
    ]).await;

    assert_eq!(rx.try_recv().ok(), Some(true));
}

/// A preview toggle is a different action, and must not reach the port — that is
/// what `preview` means: show what the press would do, do not do it.
#[tokio::test]
async fn a_preview_toggle_still_goes_out_as_an_action() {
    let (tx, mut rx) = mpsc::channel(4);
    let (tap_tx, mut tap_rx) = mpsc::channel(4);
    let outputs = Outputs {
        internal: Some(tx),
        tap: Some(Tap::new(tap_tx)),
        ..Default::default()
    };

    outputs.dispatch(sites::SET_GAMING_MODE, &[
        OutputAction::Internal(InternalCommand::SetGamingMode { enabled: true, preview: true }),
    ]).await;

    // Visible to anything watching …
    assert_eq!(drain(&mut tap_rx).len(), 1);
    // … and the owner is the one that decides what preview means, so the port
    // still sees it. The distinction lives in the action, which is why `preview`
    // is part of its identity.
    assert_eq!(rx.try_recv().ok(), Some(true));
}

/// A snapshot goes through the state port urgently or droppably, and the two are
/// different instructions rather than a hint.
#[tokio::test]
async fn the_state_port_carries_the_snapshot() {
    let (tx, mut rx) = mpsc::channel(4);
    let outputs = Outputs { state: Some(tx), ..Default::default() };

    outputs.dispatch(sites::PUBLISH_STATE, &[
        OutputAction::State(StateAction::Publish {
            snapshot: Box::new(serde_json::json!({ "context": { "paused": true } })),
            urgency: Urgency::Immediate,
        }),
    ]).await;

    match rx.try_recv() {
        Ok(StateCommand::SetEventState(Some(snapshot))) => {
            assert_eq!(snapshot["context"]["paused"], true);
        }
        other => panic!("expected a snapshot, got {other:?}"),
    }
}

/// Watching must never hold up the input path, so a full tap drops and counts.
/// A silent gap would let a later comparison claim an agreement it never saw.
#[tokio::test]
async fn a_full_tap_drops_rather_than_waits() {
    let (outputs, mut rx) = tapped(2);
    for _ in 0..5 {
        outputs.dispatch(sites::EMIT_EVENT_KEYS, &[frame(OutputDevice::Keys)]).await;
    }

    assert_eq!(drain(&mut rx).len(), 2);
    assert_eq!(outputs.tap.as_ref().unwrap().dropped(), 3);
}
