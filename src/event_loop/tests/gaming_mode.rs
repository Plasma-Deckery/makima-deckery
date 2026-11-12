//! Gaming Mode: the game reads the controller through Steam Input, so the
//! desktop bindings stand aside — except the ones marked `while_gaming`, and
//! except every release, so nothing pressed before the switch stays down.

use super::*;
use crate::output::action::InternalCommand;

/// In Gaming Mode A sends nothing and is not even highlighted. Y is
/// `while_gaming` and fires anyway.
#[tokio::test]
async fn gaming_mode_suppresses_a_press_but_not_a_while_gaming_binding() {
    let mut deck = Deck::new("gaming");
    deck.set_gaming_mode(true).await;

    let a = deck.press(Key::BTN_SOUTH).await;
    assert!(keys(&a).is_empty());
    assert!(deck.held_keys().await.is_empty());
    let a_up = deck.release(Key::BTN_SOUTH).await;

    let y = deck.press(Key::BTN_NORTH).await;
    assert_eq!(keys(&y), vec![(Key::KEY_SPACE, 1)]);
    let y_up = deck.release(Key::BTN_NORTH).await;

    assert!(still_down(&[a, a_up, y, y_up].concat()).is_empty());
}

/// Switching it on fires the haptic and says so; switching it to what it
/// already is does nothing at all.
#[tokio::test]
async fn the_setter_announces_a_change_and_ignores_a_repeat() {
    let mut deck = Deck::new("gaming-setter");
    let on = deck.set_gaming_mode(true).await;
    assert!(*deck.reader.gaming_mode.lock().await);
    assert!(on.iter().any(|a| matches!(a, OutputAction::Haptic { .. })));
    assert_eq!(announced(&on), vec![Some("Gaming Mode On".to_string())]);

    assert!(deck.set_gaming_mode(true).await.is_empty());
}

/// Gaming Mode switched on while A is held — a game launched from the desktop —
/// must not strand Enter. The guard lets every release through for this.
#[tokio::test]
async fn gaming_mode_switched_on_mid_hold_still_releases_the_key() {
    let mut deck = Deck::new("gaming-midhold");
    let a = deck.press(Key::BTN_SOUTH).await;

    deck.set_gaming_mode(true).await;
    let a_up = deck.release(Key::BTN_SOUTH).await;
    assert_eq!(keys(&a_up), vec![(Key::KEY_ENTER, 0)]);
    assert!(still_down(&[a, a_up].concat()).is_empty());
    assert!(deck.held_keys().await.is_empty());
}

/// L1 pressed in Gaming Mode emits nothing but is still tracked, so when the
/// mode goes off mid-hold the very next X is already Copy.
#[tokio::test]
async fn a_modifier_held_through_gaming_mode_is_still_tracked() {
    let mut deck = Deck::new("gaming-mod");
    deck.set_gaming_mode(true).await;

    let l1 = deck.press(Key::BTN_TL).await;
    assert!(keys(&l1).is_empty());
    assert_eq!(deck.modifiers().await, vec![ev(Key::BTN_TL)]);

    deck.set_gaming_mode(false).await;
    let x = deck.press(Key::BTN_WEST).await;
    assert_eq!(keys(&x), vec![(Key::KEY_LEFTCTRL, 1), (Key::KEY_C, 1)]);
}

/// A double-click on QAM toggles Gaming Mode. The first press is still QAM to
/// the desktop; the second is consumed by the toggle and goes no further. The
/// mode change leaves through the internal port — empty here, so the reader's
/// own flag is only flipped when that port loops it back, which is the point.
#[tokio::test]
async fn a_double_click_on_qam_toggles_gaming_mode() {
    let mut deck = Deck::new("double-click");
    let first = deck.feed([report(Key::BTN_BASE, 1), report(Key::BTN_BASE, 0)].concat()).await;
    assert_eq!(keys(&first), vec![(Key::BTN_BASE, 1), (Key::BTN_BASE, 0)]);

    let second = deck.press(Key::BTN_BASE).await;
    assert!(keys(&second).is_empty());
    assert!(second.contains(&OutputAction::Internal(
        InternalCommand::SetGamingMode { enabled: true, preview: false },
    )));
    assert_eq!(announced(&second), vec![Some("Gaming Mode On".to_string())]);
    assert!(!*deck.reader.gaming_mode.lock().await);
}

/// The same double-click while paused only previews the toggle: pause and
/// Gaming Mode are orthogonal, so the trigger stays reachable, but a paused
/// reader does not change modes.
#[tokio::test]
async fn a_double_click_while_paused_is_a_preview() {
    let mut deck = Deck::new("double-click-paused");
    deck.pause().await;
    deck.feed([report(Key::BTN_BASE, 1), report(Key::BTN_BASE, 0)].concat()).await;
    let second = deck.press(Key::BTN_BASE).await;
    assert!(second.contains(&OutputAction::Internal(
        InternalCommand::SetGamingMode { enabled: true, preview: true },
    )));
}
