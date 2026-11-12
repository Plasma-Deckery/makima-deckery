//! Plain remaps and commands: one button, one binding, nothing held around it.

use super::*;

/// A → Enter, pressed and released: one key-down, one key-up. The key-up comes
/// from the ledger, which released what the press recorded; nothing else sends
/// one, and nothing stays recorded for the next pause to release again.
#[tokio::test]
async fn a_plain_remap_presses_and_releases_its_key() {
    let mut deck = Deck::new("plain");

    let down = deck.press(Key::BTN_SOUTH).await;
    assert_eq!(keys(&down), vec![(Key::KEY_ENTER, 1)]);
    // A base binding is a held key, not an event: it does not announce.
    assert!(announced(&down).is_empty());
    assert_eq!(deck.held_keys().await, vec![ev(Key::BTN_SOUTH)]);

    let up = deck.release(Key::BTN_SOUTH).await;
    assert_eq!(keys(&up), vec![(Key::KEY_ENTER, 0)], "one release, one key-up");
    assert!(deck.held_keys().await.is_empty());
    assert!(deck.ledger_is_empty().await, "the release left A recorded as emitted");

    let pause = deck.pause().await;
    assert!(keys(&pause).is_empty(), "a pause after the release sent a stale key-up");
}

/// A command binding runs on the press, announces its label, and its release
/// sends nothing. The command port is empty here, so the action is seen on
/// the tap and nothing is spawned.
#[tokio::test]
async fn a_command_runs_once_on_the_press() {
    let mut deck = Deck::new("command");

    let down = deck.press(Key::BTN_THUMBR).await;
    assert_eq!(commands(&down), vec![vec!["notify-send".to_string(), "hello".to_string()]]);
    assert_eq!(announced(&down), vec![Some("Notify".to_string())]);
    assert!(keys(&down).is_empty());

    let up = deck.release(Key::BTN_THUMBR).await;
    assert!(commands(&up).is_empty());
    assert!(keys(&up).is_empty());
}

/// A pressed while no app override matches is Enter. The focus moves to
/// Firefox, where A is Space: the next press of A resolves the override.
#[tokio::test]
async fn an_app_override_applies_from_the_next_press() {
    let mut deck = Deck::on_kde("override");
    let enter = [deck.press(Key::BTN_SOUTH).await, deck.release(Key::BTN_SOUTH).await].concat();
    assert_eq!(keys(&enter), vec![(Key::KEY_ENTER, 1), (Key::KEY_ENTER, 0)]);

    *deck.reader.active_client.lock().await =
        Client::Class("firefox".to_string(), String::new(), None);
    let space = [deck.press(Key::BTN_SOUTH).await, deck.release(Key::BTN_SOUTH).await].concat();
    assert_eq!(keys(&space), vec![(Key::KEY_SPACE, 1), (Key::KEY_SPACE, 0)]);
}
