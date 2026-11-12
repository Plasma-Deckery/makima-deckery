//! One owner for every key-up: the ledger.
//!
//! A release sends exactly what its press recorded, whatever the binding
//! resolves to by then and whatever happened in between — a modifier let go
//! first, a pause, a passthrough. These are the cases where asking the binding
//! instead would leave a key down or send one for a key that never went down.

use super::*;

/// L1 + X is Ctrl+C. Letting go of L1 first changes what X resolves to (plain
/// Backspace), but the release of X has to release what was pressed — Ctrl and
/// C — and nothing else: no key-up for a Backspace that never went down.
#[tokio::test]
async fn a_resolution_change_mid_hold_sends_no_stray_key_up() {
    let mut deck = Deck::new("resolution-change");
    deck.press(Key::BTN_TL).await;
    let x = deck.press(Key::BTN_WEST).await;
    assert_eq!(keys(&x), vec![(Key::KEY_LEFTCTRL, 1), (Key::KEY_C, 1)]);

    let l1_up = deck.release(Key::BTN_TL).await;
    assert!(keys(&l1_up).is_empty());
    let x_up = deck.release(Key::BTN_WEST).await;
    assert_eq!(sorted_keys(&x_up), vec![(Key::KEY_LEFTCTRL, 0), (Key::KEY_C, 0)]);
}

/// A Hold binding fires while a modifier is held: L1 + R4 is Tab. Letting go of
/// L1 first and R4 second must still release Tab: with L1 gone R4 no longer
/// resolves to the Hold, so only what the press recorded can say which key is
/// down.
#[tokio::test]
async fn a_hold_binding_released_after_its_modifier_goes_up() {
    let mut deck = Deck::new("hold");
    deck.press(Key::BTN_TL).await;
    assert_eq!(keys(&deck.press(Key::BTN_GRIPR).await), vec![(Key::KEY_TAB, 1)], "precondition: the Hold fired");
    deck.release(Key::BTN_TL).await;

    assert_eq!(keys(&deck.release(Key::BTN_GRIPR).await), vec![(Key::KEY_TAB, 0)], "Tab stayed down");
    assert!(deck.ledger_is_empty().await);
}

/// A button with no binding is passed through, and a pause releases it like
/// anything else makima holds down. Its release during the pause, and the
/// resume, send nothing more.
#[tokio::test]
async fn a_passthrough_key_held_across_a_pause_goes_up_once() {
    let select = Key::BTN_SELECT;
    let mut deck = Deck::new("passthrough");
    assert_eq!(keys(&deck.press(select).await), vec![(select, 1)], "precondition: passed through");

    assert_eq!(keys(&deck.pause().await), vec![(select, 0)], "the pause left it down");
    assert!(keys(&deck.release(select).await).is_empty(), "the release during the pause sent something");
    assert!(keys(&deck.resume().await).is_empty(), "resume sent something");

    // Without a pause: one key-down, one key-up.
    let plain = [deck.press(select).await, deck.release(select).await].concat();
    assert_eq!(keys(&plain), vec![(select, 1), (select, 0)]);
    assert!(deck.ledger_is_empty().await);
}

/// Issue #63: a key bound to an axis edge — a stick direction here, a trigger
/// or a hat alike — is held down like any button's, so a pause has to release
/// it too. The stick goes through the real decoding, which remembers the
/// direction held from one report to the next.
#[tokio::test]
async fn an_axis_bound_key_held_across_a_pause_goes_up() {
    let mut deck = Deck::new("stick-across");
    assert_eq!(keys(&deck.feed(lstick_y(-32767)).await), vec![(Key::KEY_UP, 1)]);

    assert_eq!(keys(&deck.pause().await), vec![(Key::KEY_UP, 0)], "the pause left Up down");
    let mut after = deck.feed(lstick_y(0)).await;
    after.extend(deck.resume().await);
    assert!(keys(&after).is_empty(), "Up went up twice");

    // Without a pause: one key-up at the return to centre.
    let plain = [deck.feed(lstick_y(-32767)).await, deck.feed(lstick_y(0)).await].concat();
    assert_eq!(keys(&plain), vec![(Key::KEY_UP, 1), (Key::KEY_UP, 0)]);
    assert!(deck.ledger_is_empty().await);
}

/// A reconnect means edges were lost: whatever makima holds goes up, and the
/// held input is forgotten too, because it can no longer be trusted. The next
/// edges rebuild it.
#[tokio::test]
async fn a_reconnect_releases_everything_and_forgets_the_held_input() {
    let mut deck = Deck::new("reconnect");
    deck.press(Key::BTN_TL).await;
    deck.press(Key::BTN_SOUTH).await;

    let reconnect = deck.signal(ControllerEvent::Reconnected).await;
    assert_eq!(keys(&reconnect), vec![(Key::KEY_ENTER, 0)]);
    assert!(deck.ledger_is_empty().await);
    assert!(deck.modifiers().await.is_empty());
    assert!(deck.held_keys().await.is_empty());
}
