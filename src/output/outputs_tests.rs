use super::*;
use crate::output::action::{Ev, OutputDevice};

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

    outputs.dispatch(&[
        frame(OutputDevice::Keys),
        OutputAction::Internal(InternalCommand::SetGamingMode { enabled: true, preview: false }),
    ]).await;

    let seen = drain(&mut rx);
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].action, frame(OutputDevice::Keys));
}

/// Nothing clipped in and nobody watching: dispatch is a no-op that cannot fail.
#[tokio::test]
async fn dispatch_with_no_ports_and_no_tap_is_harmless() {
    let outputs = Outputs::default();
    assert!(!outputs.watched());
    outputs.dispatch(&[frame(OutputDevice::Axis)]).await;
}

/// The sequence number is what gives records from different tasks a single order.
#[tokio::test]
async fn emissions_are_numbered_in_order() {
    let (outputs, mut rx) = tapped(8);
    outputs.dispatch(&[
        frame(OutputDevice::Keys),
        frame(OutputDevice::Axis),
    ]).await;
    outputs.dispatch(&[frame(OutputDevice::Axis)]).await;

    let seqs: Vec<u64> = drain(&mut rx).into_iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![0, 1, 2]);
}

/// Gaming Mode goes out through its port as a plain bool, which is how it comes
/// back in as the next input. The loop is observable at both ends.
#[tokio::test]
async fn the_internal_port_carries_gaming_mode_back() {
    let (tx, mut rx) = mpsc::channel(4);
    let outputs = Outputs { internal: Some(tx), ..Default::default() };

    outputs.dispatch(&[
        OutputAction::Internal(InternalCommand::SetGamingMode { enabled: true, preview: false }),
    ]).await;

    assert_eq!(rx.try_recv().ok(), Some(true));
}

/// A preview toggle is a different action, and must not reach the port — that is
/// what `preview` means: show what the press would do, do not do it.
#[tokio::test]
async fn a_preview_toggle_goes_out_as_an_action_but_not_to_the_port() {
    let (tx, mut rx) = mpsc::channel(4);
    let (tap_tx, mut tap_rx) = mpsc::channel(4);
    let outputs = Outputs {
        internal: Some(tx),
        tap: Some(Tap::new(tap_tx)),
        ..Default::default()
    };

    outputs.dispatch(&[
        OutputAction::Internal(InternalCommand::SetGamingMode { enabled: true, preview: true }),
    ]).await;

    // Visible to anything watching …
    assert_eq!(drain(&mut tap_rx).len(), 1);
    // … but not sent on: the port carries a bare bool into
    // `set_gaming_mode(_, paused = false)`, so whatever reaches it switches the
    // mode. A double-click on the trigger while paused used to do exactly that.
    assert!(rx.try_recv().is_err());
}

/// A snapshot goes through the state port urgently or droppably, and the two are
/// different instructions rather than a hint.
#[tokio::test]
async fn the_state_port_carries_the_snapshot() {
    let (tx, mut rx) = mpsc::channel(4);
    let outputs = Outputs { state: Some(tx), ..Default::default() };

    outputs.dispatch(&[
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
        outputs.dispatch(&[frame(OutputDevice::Keys)]).await;
    }

    assert_eq!(drain(&mut rx).len(), 2);
    assert_eq!(outputs.tap.as_ref().unwrap().dropped(), 3);
}

fn haptic() -> OutputAction {
    OutputAction::Haptic {
        pad: deckery_controller::HapticPad::Right,
        chain: deckery_controller::HapticChain::default(),
    }
}

/// A backlog of haptic chains must not hold up motion: with the haptic queue
/// full, dispatching a tick's haptic alongside its motion returns at once,
/// everything after the haptic still goes out, and the tick is dropped.
#[tokio::test]
async fn a_full_haptic_queue_does_not_block_motion() {
    let (haptic_tx, mut haptic_rx) = mpsc::channel(1);
    haptic_tx.try_send(HapticRequest {
        pad: deckery_controller::HapticPad::Right,
        chain: deckery_controller::HapticChain::default(),
    }).unwrap();
    let (internal_tx, mut internal_rx) = mpsc::channel(4);
    let (tap_tx, mut tap_rx) = mpsc::channel(16);
    let port = HapticPort::new(haptic_tx);
    let outputs = Outputs {
        haptics: Some(port.clone()),
        internal: Some(internal_tx),
        tap: Some(Tap::new(tap_tx)),
        ..Default::default()
    };

    let motion = [
        haptic(),
        frame(OutputDevice::Axis),
        OutputAction::Internal(InternalCommand::SetGamingMode { enabled: true, preview: false }),
    ];
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        outputs.dispatch(&motion).await;
        outputs.dispatch(&motion).await;
    })
    .await
    .expect("dispatch waited for room in the haptic queue");

    assert_eq!(port.dropped(), 2);
    assert_eq!(drain(&mut tap_rx).len(), 6);
    assert_eq!(internal_rx.try_recv(), Ok(true));
    assert_eq!(internal_rx.try_recv(), Ok(true));
    // Only the request that was already queued is there.
    assert!(haptic_rx.try_recv().is_ok());
    assert!(haptic_rx.try_recv().is_err());
}

/// With room in the queue the haptic is delivered as before.
#[tokio::test]
async fn a_haptic_with_room_is_delivered() {
    let (haptic_tx, mut haptic_rx) = mpsc::channel(4);
    let port = HapticPort::new(haptic_tx);
    let outputs = Outputs { haptics: Some(port.clone()), ..Default::default() };

    outputs.dispatch(&[haptic()]).await;

    assert_eq!(port.dropped(), 0);
    let got = haptic_rx.try_recv().expect("haptic not delivered");
    assert_eq!(got.pad, deckery_controller::HapticPad::Right);
}
