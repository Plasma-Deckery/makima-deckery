//! The analog part of the state across a rebuild of the device tasks.
//!
//! The HUD sends `analog-state-export on` once, when a pad or stick comes on
//! screen, and `off` once it is gone; nothing in between. Every config reload
//! and every override `set` of a device section tears the readers down and
//! starts new ones, so the flag has to outlive them: the device session owns
//! it, and a reader started by a rebuild is handed the same one. When each
//! reader kept its own, starting at off, the pads' positions stopped reaching
//! the state after the first rebuild and the HUD's dots only moved on the
//! writes a button or a touch forces — they jumped.

use super::*;
use crate::output::action::Urgency;

/// The right pad's x in every analog snapshot among `actions`.
fn analog_rpad_x(actions: &[OutputAction]) -> Vec<f64> {
    actions.iter().filter_map(|a| match a {
        OutputAction::State(StateAction::Publish { snapshot, urgency: Urgency::Analog }) =>
            snapshot["trackpads"]["right"]["x"].as_f64(),
        _ => None,
    }).collect()
}

/// The pad moving to `x`, as the router reports it: the position goes into the
/// pad state, then an analog write is asked for (`state_write_loop`).
async fn move_rpad(deck: &mut Deck, x: i32) -> Vec<OutputAction> {
    *deck.reader.rpad.position.lock().await = (x, 0);
    deck.reader.write_state_analog().await;
    deck.take()
}

#[tokio::test]
async fn the_export_survives_a_rebuild() {
    let switches = crate::process_switches::Switches::default();
    let flag = switches.analog.clone();
    let mut before = Deck::in_session("analog-before", switches.clone());
    before.control("analog-state-export on").await;
    assert_eq!(analog_rpad_x(&move_rpad(&mut before, 16384).await).len(), 1);
    drop(before);

    // The rebuild: a new reader, the same session, no new command.
    let mut after = Deck::in_session("analog-after", switches.clone());
    let xs = analog_rpad_x(&move_rpad(&mut after, -16384).await);
    assert_eq!(xs.len(), 1, "the pad's position must keep reaching the state");
    assert!(xs[0] < 0.0, "{xs:?}");

    after.control("analog-state-export off").await;
    assert!(analog_rpad_x(&move_rpad(&mut after, 0).await).is_empty());
    assert!(!flag.is_on(), "off is the session's too");
}

/// The analog switch is the control socket's, not a reader's: it takes
/// effect with no reader listening.
#[tokio::test]
async fn the_switch_needs_no_reader() {
    let switches = crate::process_switches::Switches::default();
    let flag = switches.analog.clone();
    let deck = Deck::in_session("analog-socket", switches.clone());
    // Straight to the socket: no reader loop is stepped.
    assert_eq!(deck.control_socket().handle("analog-state-export on", "test").await, None);
    assert!(flag.is_on());
}

/// The tour as it ran on the Deck: one combined `set trackpad {…}` per stage,
/// the gesture device switched off and on, a step back and forward again,
/// bursts of sets that each rebuild, and the HUD's `on` arriving while a
/// rebuild is under way — the old readers gone, the new one not started.
/// After every step the switch is on and the pad's position reaches the state.
#[tokio::test]
async fn the_export_survives_the_tour_with_on_sent_mid_rebuild() {
    let switches = crate::process_switches::Switches::default();
    let flag = switches.analog.clone();
    let mut deck = Deck::in_session("analog-tour", switches.clone());

    let stage = |mode: &str, gestures: bool, click: bool| format!(
        r#"override set trackpad {{ "combined_gesture_device": {gestures}, "right": {{ "mode": "{mode}", "click": {click} }}, "gestures": {{ "click": {click} }} }}"#);
    let stage2 = stage("trackball", false, true);
    let stage3 = stage("mt-trackpad", true, false);
    let stage4 = stage("mt-trackpad", true, true);
    let mut lines: Vec<String> = vec!["override begin".into(), "override clear".into()];
    lines.extend([stage2.clone(), stage3.clone(), stage2.clone(), stage3.clone()]);  // 2 → 3 → back → 3
    for _ in 0..4 {
        lines.extend([stage4.clone(), stage3.clone()]);                              // a burst
    }
    lines.push("override end".into());

    let mut on_sent = false;
    for line in &lines {
        let (reply, _) = deck.override_cmd(line).await;
        assert!(reply.starts_with("ok"), "{line}: {reply}");
        if !deck.device_log.is_empty() {
            deck.device_log.clear();
            if !on_sent {
                // In the middle of the rebuild no reader is listening: a
                // line broadcast now would reach nobody. It has to be
                // carried out by the socket itself.
                deck.control_socket().handle("analog-state-export on", "test").await;
                on_sent = true;
            }
            deck = deck.rebuilt();
        }
        if on_sent {
            assert!(flag.is_on(), "after {line}: the switch went off");
            let actions = move_rpad(&mut deck, 8192).await;
            assert_eq!(analog_rpad_x(&actions).len(), 1, "after {line}: the pad's position stopped reaching the state");
            assert_eq!(last_published(&actions).unwrap()["context"]["analog_state_export"],
                serde_json::json!(true), "after {line}");
        }
    }
    assert!(on_sent);
    assert!(flag.is_on());
}

/// The HUD's `off` and the `on` right behind it, around and inside a rebuild:
/// the switch ends up as the last line said. The socket takes one connection
/// at a time and carries the switch out before it accepts the next, and no
/// reader is involved, so their order is the order they take effect in.
#[tokio::test]
async fn off_then_on_ends_on_whatever_the_rebuild_timing() {
    let switches = crate::process_switches::Switches::default();
    let flag = switches.analog.clone();
    let mut deck = Deck::in_session("analog-off-on", switches.clone());
    async fn send(deck: &Deck, line: &str) {
        deck.control_socket().handle(line, "test").await;
    }
    send(&deck, "analog-state-export on").await;
    deck.override_cmd("override begin").await;

    for (i, set) in [r#""trackball""#, r#""mt-trackpad""#, r#""trackball""#].iter().enumerate() {
        deck.override_cmd(&format!("override set trackpad.right.mode {set}")).await;
        deck.device_log.clear();
        match i {
            // Both before the rebuild.
            0 => { send(&deck, "analog-state-export off").await; send(&deck, "analog-state-export on").await; deck = deck.rebuilt(); }
            // Split by it.
            1 => { send(&deck, "analog-state-export off").await; deck = deck.rebuilt(); send(&deck, "analog-state-export on").await; }
            // Both after it.
            _ => { deck = deck.rebuilt(); send(&deck, "analog-state-export off").await; send(&deck, "analog-state-export on").await; }
        }
        assert!(flag.is_on(), "round {i}: off then on did not end on");
        assert_eq!(analog_rpad_x(&move_rpad(&mut deck, 4096).await).len(), 1, "round {i}");
    }
}
