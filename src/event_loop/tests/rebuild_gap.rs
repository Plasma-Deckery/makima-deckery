//! Commands that arrive while the device tasks are being rebuilt.
//!
//! A rebuild aborts the readers before it starts new ones. The control socket
//! used to broadcast every line to the readers, so a line in that gap reached
//! nobody, and a reader's own `paused` started at false, so a rebuild while
//! the HUD was open switched remapping back on. Here a command is sent with
//! no reader stepping its loops — straight to the socket's `handle` — and
//! the reader that the rebuild starts afterwards has to act on it.

use super::*;

/// Straight to the socket, as a client would send it during the gap.
async fn in_the_gap(deck: &Deck, line: &str) {
    deck.control_socket().handle(line, "test").await;
}

/// A pause sent during a rebuild holds in the reader the rebuild starts.
#[tokio::test]
async fn a_pause_sent_during_a_rebuild_stays() {
    let deck = Deck::new("gap-pause");
    in_the_gap(&deck, "pause").await;
    let mut deck = deck.rebuilt();
    assert!(*deck.reader.paused.lock().await, "the new reader started unpaused");
    assert!(keyboard(&tap(&mut deck, Key::BTN_SOUTH).await).is_empty(), "A fired while paused");

    deck.resume().await;
    assert_eq!(keyboard(&tap(&mut deck, Key::BTN_SOUTH).await),
        vec![(Key::KEY_ENTER, 1), (Key::KEY_ENTER, 0)]);
}

/// A rebuild while paused — the HUD open over a tour stage change — does not
/// resume. Several in a row do not either.
#[tokio::test]
async fn a_rebuild_while_paused_stays_paused() {
    let mut deck = Deck::new("gap-paused");
    deck.pause().await;
    for _ in 0..3 {
        deck = deck.rebuilt();
        // Before any of its loops ran: a reader is built paused.
        assert!(*deck.reader.paused.lock().await);
    }
    assert!(keyboard(&tap(&mut deck, Key::BTN_SOUTH).await).is_empty());
}

/// A resume sent during a rebuild holds as well.
#[tokio::test]
async fn a_resume_sent_during_a_rebuild_stays() {
    let mut deck = Deck::new("gap-resume");
    deck.pause().await;
    in_the_gap(&deck, "resume").await;
    let mut deck = deck.rebuilt();
    assert!(!*deck.reader.paused.lock().await);
    assert_eq!(keyboard(&tap(&mut deck, Key::BTN_SOUTH).await),
        vec![(Key::KEY_ENTER, 1), (Key::KEY_ENTER, 0)]);
}

/// A Gaming Mode request sent during a rebuild waits for the next reader,
/// which applies it with its announcement.
#[tokio::test]
async fn a_gaming_mode_request_during_a_rebuild_is_applied() {
    let deck = Deck::new("gap-gaming");
    in_the_gap(&deck, "gaming_mode enable").await;
    let mut deck = deck.rebuilt();
    deck.follow_switches().await;
    assert!(*deck.reader.gaming_mode.lock().await, "the request was lost");
    assert_eq!(announced(&deck.take()), vec![Some("Gaming Mode On".to_string())]);
}

/// A config switched off during a rebuild is off for the next reader: the
/// registry is the process's, and the socket switches it itself.
#[tokio::test]
async fn a_config_switch_during_a_rebuild_is_applied() {
    let mut deck = Deck::on_kde("gap-config");
    *deck.reader.active_client.lock().await =
        Client::Class("firefox".to_string(), String::new(), None);
    assert_eq!(keyboard(&tap(&mut deck, Key::BTN_SOUTH).await),
        vec![(Key::KEY_SPACE, 1), (Key::KEY_SPACE, 0)], "Firefox's own A");

    in_the_gap(&deck, "config disable firefox").await;
    let mut deck = deck.rebuilt();
    *deck.reader.active_client.lock().await =
        Client::Class("firefox".to_string(), String::new(), None);
    assert_eq!(keyboard(&tap(&mut deck, Key::BTN_SOUTH).await),
        vec![(Key::KEY_ENTER, 1), (Key::KEY_ENTER, 0)], "the module was still on");
}

/// The snapshot a pause writes leaves the analog half out, but reports the
/// analog switch as it is.
#[tokio::test]
async fn the_pause_snapshot_reports_the_analog_switch_as_it_is() {
    let mut deck = Deck::new("gap-snapshot");
    deck.control("analog-state-export on").await;
    deck.pause().await;
    let mut snapshot = None;
    while let Ok(command) = deck.state_rx.try_recv() {
        if let StateCommand::SetEventState(Some(state)) = command {
            snapshot = Some(state);
        }
    }
    let state = snapshot.expect("the pause wrote a snapshot");
    assert_eq!(state["context"]["paused"], serde_json::json!(true));
    assert_eq!(state["context"]["analog_state_export"], serde_json::json!(true));
    assert!(state["trackpads"].is_null());
}

/// A press and its release.
async fn tap(deck: &mut Deck, key: Key) -> Vec<OutputAction> {
    [deck.press(key).await, deck.release(key).await].concat()
}

/// The keyboard keys among `actions`, not a controller button passed through.
fn keyboard(actions: &[OutputAction]) -> Vec<(Key, i32)> {
    keys(actions).into_iter().filter(|(k, _)| k.code() < Key::BTN_0.code()).collect()
}
