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
    let flag = Arc::new(Mutex::new(false));
    let mut before = Deck::in_session("analog-before", flag.clone());
    before.ipc("analog-state-export on").await;
    assert_eq!(analog_rpad_x(&move_rpad(&mut before, 16384).await).len(), 1);
    drop(before);

    // The rebuild: a new reader, the same session, no new command.
    let mut after = Deck::in_session("analog-after", flag.clone());
    let xs = analog_rpad_x(&move_rpad(&mut after, -16384).await);
    assert_eq!(xs.len(), 1, "the pad's position must keep reaching the state");
    assert!(xs[0] < 0.0, "{xs:?}");

    after.ipc("analog-state-export off").await;
    assert!(analog_rpad_x(&move_rpad(&mut after, 0).await).is_empty());
    assert!(!*flag.lock().await, "off is the session's too");
}
