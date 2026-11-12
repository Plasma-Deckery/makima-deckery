//! Combos, layer keys and custom modifiers: what a held modifier changes, and
//! that letting go of either side in either order leaves nothing down.

use super::*;

/// L1 + X → Ctrl+C. L1 has an empty base remap, so holding it emits nothing and
/// only enters the modifier set; the combo announces its label, and releasing
/// X releases both keys before L1 leaves the set.
#[tokio::test]
async fn a_combo_on_a_layer_key_emits_announces_and_releases() {
    let mut deck = Deck::new("copy");

    let l1 = deck.press(Key::BTN_TL).await;
    assert!(keys(&l1).is_empty());
    assert_eq!(deck.modifiers().await, vec![ev(Key::BTN_TL)]);

    let x = deck.press(Key::BTN_WEST).await;
    assert_eq!(keys(&x), vec![(Key::KEY_LEFTCTRL, 1), (Key::KEY_C, 1)]);
    assert_eq!(announced(&x), vec![Some("Copy".to_string())]);
    // Announced before the keys go out, so the overlay never trails the action.
    let announce_at = x.iter().position(|a| matches!(a, OutputAction::State(StateAction::SetLastAction(_)))).unwrap();
    let first_frame = x.iter().position(|a| matches!(a, OutputAction::Frame { .. })).unwrap();
    assert!(announce_at < first_frame);

    // One key-up each: the ledger releases what the combo recorded, and
    // nothing else does.
    let x_up = deck.release(Key::BTN_WEST).await;
    assert_eq!(sorted_keys(&x_up), vec![(Key::KEY_LEFTCTRL, 0), (Key::KEY_C, 0)]);
    assert_eq!(deck.modifiers().await, vec![ev(Key::BTN_TL)], "L1 is still held");

    let l1_up = deck.release(Key::BTN_TL).await;
    assert!(keys(&l1_up).is_empty());
    assert!(deck.modifiers().await.is_empty());
    assert!(deck.ledger_is_empty().await);
    assert!(still_down(&[l1, x, x_up, l1_up].concat()).is_empty());
}

/// L4 is Shift on its own and a layer key for L4-A, so it has to be both at
/// once. Holding it presses Shift. The combo consumes it — Shift goes up before
/// Ctrl+S goes down, or the app would see Ctrl+Shift+S — and releasing L4
/// afterwards sends nothing more: Shift is already up.
#[tokio::test]
async fn a_custom_modifier_with_its_own_output_is_released_around_a_combo() {
    let mut deck = Deck::new("save");

    let l4 = deck.press(Key::BTN_GRIPL).await;
    assert_eq!(keys(&l4), vec![(Key::KEY_LEFTSHIFT, 1)]);
    assert_eq!(deck.modifiers().await, vec![ev(Key::BTN_GRIPL)]);

    let a = deck.press(Key::BTN_SOUTH).await;
    assert_eq!(keys(&a), vec![
        (Key::KEY_LEFTSHIFT, 0),
        (Key::KEY_LEFTCTRL, 1),
        (Key::KEY_S, 1),
    ]);
    assert_eq!(announced(&a), vec![Some("Save".to_string())]);

    let a_up = deck.release(Key::BTN_SOUTH).await;
    assert_eq!(sorted_keys(&a_up), vec![(Key::KEY_LEFTCTRL, 0), (Key::KEY_S, 0)]);

    let l4_up = deck.release(Key::BTN_GRIPL).await;
    assert!(deck.modifiers().await.is_empty());
    assert!(still_down(&[l4, a, a_up, l4_up].concat()).is_empty());
    assert!(deck.ledger_is_empty().await);
}

/// L4 + Down has no combo of its own, so it falls back to the base binding with
/// Shift kept down — Shift+Down, which is how a selection is extended. The
/// fallback must not release the modifier's output.
#[tokio::test]
async fn a_fallback_keeps_the_modifier_output_held() {
    let mut deck = Deck::new("fallback");
    let mut all = deck.press(Key::BTN_GRIPL).await;

    let down = deck.press(Key::BTN_DPAD_DOWN).await;
    assert_eq!(keys(&down), vec![(Key::KEY_DOWN, 1)]);
    all.extend(down);
    assert_eq!(still_down(&all), vec![Key::KEY_LEFTSHIFT, Key::KEY_DOWN]);

    all.extend(deck.release(Key::BTN_DPAD_DOWN).await);
    assert_eq!(still_down(&all), vec![Key::KEY_LEFTSHIFT]);
    all.extend(deck.release(Key::BTN_GRIPL).await);
    assert!(still_down(&all).is_empty());
}

/// Start → F10 is pressed, then R1 joins, so on the way up Start resolves to
/// R1-Start instead. The F10 its press emitted is released, and only that: the
/// ledger knows what went down, whatever the binding says now.
#[tokio::test]
async fn a_release_clears_what_the_press_emitted_after_a_modifier_joined() {
    let mut deck = Deck::new("midhold-mod");

    let start = deck.press(Key::BTN_START).await;
    assert_eq!(keys(&start), vec![(Key::KEY_F10, 1)]);
    let r1 = deck.press(Key::BTN_TR).await;
    assert!(keys(&r1).is_empty());

    let start_up = deck.release(Key::BTN_START).await;
    assert_eq!(keys(&start_up), vec![(Key::KEY_F10, 0)]);

    let r1_up = deck.release(Key::BTN_TR).await;
    assert!(still_down(&[start, r1, start_up, r1_up].concat()).is_empty());
}

/// A pressed with no app override is Enter. The focus moves to Firefox, where A
/// is Space, and the next press re-resolves the config. Releasing A must still
/// release Enter, and must not release a Space that never went down.
#[tokio::test]
async fn a_release_clears_what_the_press_emitted_after_the_app_changed() {
    let mut deck = Deck::on_kde("midhold-app");

    let a = deck.press(Key::BTN_SOUTH).await;
    assert_eq!(keys(&a), vec![(Key::KEY_ENTER, 1)]);

    *deck.reader.active_client.lock().await =
        Client::Class("firefox".to_string(), String::new(), None);
    // Any press re-resolves; B is inherited from the base, so it is still Esc.
    let b = deck.press(Key::BTN_EAST).await;
    assert_eq!(keys(&b), vec![(Key::KEY_ESC, 1)]);

    let a_up = deck.release(Key::BTN_SOUTH).await;
    assert_eq!(keys(&a_up), vec![(Key::KEY_ENTER, 0)]);

    let b_up = deck.release(Key::BTN_EAST).await;
    assert!(still_down(&[a, b, a_up, b_up].concat()).is_empty());
}

/// The baseline for the pause tests: Steam held and released on its own is a
/// tap — the press and release of its own code.
#[tokio::test]
async fn an_unused_steam_press_taps() {
    let mut deck = Deck::new("steam-tap");
    let sent = [deck.press(Key::BTN_MODE).await, deck.release(Key::BTN_MODE).await].concat();
    assert_eq!(presses(&sent), vec![Key::BTN_MODE], "{:?}", keys(&sent));
    assert!(keys(&sent).ends_with(&[(Key::BTN_MODE, 1), (Key::BTN_MODE, 0)]), "{:?}", keys(&sent));
}

/// Holding R2 is a held left mouse button — a drag. Pressing Steam, which is a
/// layer key with no base binding of its own, must not end that drag.
///
/// Steam resolves to nothing and takes `plan_nonmapped`, which releases
/// `released_keys(modifiers)` for every modifier press. With nothing else held
/// that lookup once returned the base output of *every* binding, BTN_LEFT
/// among them; an empty modifier set now releases nothing.
#[tokio::test]
async fn pressing_steam_does_not_end_an_r2_drag() {
    let mut deck = Deck::new("steam-drag");
    let r2 = deck.press(Key::BTN_TR2).await;
    assert_eq!(keys(&r2), vec![(Key::BTN_LEFT, 1)]);

    let steam = deck.press(Key::BTN_MODE).await;
    assert!(
        !keys(&steam).contains(&(Key::BTN_LEFT, 0)),
        "Steam released the held mouse button: {:?}", keys(&steam),
    );
}
