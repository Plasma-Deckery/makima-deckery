//! Pause and resume, through the real IPC loop.
//!
//! A pause hands the controller to something else — the auth daemon for PIN
//! entry, the HUD — so every output key makima holds goes up at once, and
//! bindings send nothing until the resume. What the user is still holding is
//! kept (issue #62): a layer key held while the HUD opens is still that layer
//! key when it closes, and a modifier held across is not a tap when it is let
//! go. These run whole sequences — press, pause, resume, combo, release —
//! because the bug was in how the decisions add up, not in any one of them.

use super::*;

const L1: Key = Key::BTN_TL;
const STEAM: Key = Key::BTN_MODE;
const X: Key = Key::BTN_WEST;
const A: Key = Key::BTN_SOUTH;
const L2: Key = Key::BTN_TL2;
const L4: Key = Key::BTN_GRIPL;

// ── what a pause lets through ─────────────────────────────────────────────────

/// Paused, A sends nothing, but R2 — the left mouse button, marked `no_pause`
/// so the overlay stays clickable — still fires.
#[tokio::test]
async fn a_paused_binding_sends_nothing_and_a_no_pause_binding_fires() {
    let mut deck = Deck::new("paused");
    deck.pause().await;

    let a = deck.press(A).await;
    assert!(keys(&a).is_empty());
    let a_up = deck.release(A).await;

    let r2 = deck.press(Key::BTN_TR2).await;
    assert_eq!(keys(&r2), vec![(Key::BTN_LEFT, 1)]);
    let r2_up = deck.release(Key::BTN_TR2).await;
    assert_eq!(keys(&r2_up), vec![(Key::BTN_LEFT, 0)]);

    assert!(still_down(&[a, a_up, r2, r2_up].concat()).is_empty());
}

/// The same split for commands: the HUD toggle is `no_pause` and runs, the
/// other one does not. Both still announce, because the overlay shows what a
/// press would do even while it is paused.
#[tokio::test]
async fn a_paused_command_is_announced_but_only_a_no_pause_one_runs() {
    let mut deck = Deck::new("paused-cmd");
    deck.pause().await;

    let r3 = deck.press(Key::BTN_THUMBR).await;
    assert!(commands(&r3).is_empty());
    assert_eq!(announced(&r3), vec![Some("Notify".to_string())]);
    deck.release(Key::BTN_THUMBR).await;

    let l3 = deck.press(Key::BTN_THUMBL).await;
    assert_eq!(commands(&l3), vec![vec!["deckery-hud-toggle".to_string()]]);
    assert_eq!(announced(&l3), vec![Some("Toggle HUD".to_string())]);
}

/// A resume sends nothing, and after it A is Enter again.
#[tokio::test]
async fn after_a_resume_bindings_fire_again() {
    let mut deck = Deck::new("resumed");
    deck.pause().await;
    deck.press(A).await;
    deck.release(A).await;
    assert!(keys(&deck.resume().await).is_empty(), "resume sent something");

    let a = [deck.press(A).await, deck.release(A).await].concat();
    assert_eq!(keys(&a), vec![(Key::KEY_ENTER, 1), (Key::KEY_ENTER, 0)]);
    assert!(!*deck.reader.paused.lock().await);
}

// ── outputs go up, held input stays ───────────────────────────────────────────

/// A pause while A is held lifts Enter at the pause itself, not at the release:
/// the auth daemon must not see a key held through PIN entry. The release
/// during the pause then sends nothing more.
#[tokio::test]
async fn a_pause_mid_hold_releases_the_key_at_once() {
    let mut deck = Deck::new("pause-midhold");
    let a = deck.press(A).await;
    assert_eq!(keys(&a), vec![(Key::KEY_ENTER, 1)]);

    let pause = deck.pause().await;
    assert_eq!(keys(&pause), vec![(Key::KEY_ENTER, 0)]);
    assert_eq!(deck.held_keys().await, vec![ev(A)], "A is still held");

    let a_up = deck.release(A).await;
    assert!(keys(&a_up).is_empty(), "Enter went up a second time");
    assert!(deck.held_keys().await.is_empty());
    assert!(still_down(&[a, pause, a_up].concat()).is_empty());
}

/// A → Enter held across the pause: Enter goes up at the pause and stays up —
/// resume does not press it again, and letting go of A afterwards sends
/// nothing, because the ledger forgot Enter when the pause lifted it.
#[tokio::test]
async fn a_remapped_key_held_across_a_pause_stays_released() {
    let mut deck = Deck::new("remap-across");
    assert_eq!(keys(&deck.press(A).await), vec![(Key::KEY_ENTER, 1)]);

    assert_eq!(keys(&deck.pause().await), vec![(Key::KEY_ENTER, 0)], "the pause has to release Enter");
    assert_eq!(deck.held_keys().await, vec![ev(A)], "A is still held");

    assert!(keys(&deck.resume().await).is_empty(), "resume pressed Enter again");

    assert!(keys(&deck.release(A).await).is_empty(), "Enter went up a second time");
    assert!(deck.held_keys().await.is_empty());
}

/// L4 is Shift and a custom modifier at once. Held across the pause, its Shift
/// goes up at the pause and stays up, L4 stays in the modifier set, and letting
/// go of it afterwards neither releases Shift again nor taps.
#[tokio::test]
async fn a_modifier_with_its_own_output_held_across_a_pause() {
    let mut deck = Deck::new("l4-across");
    assert_eq!(keys(&deck.press(L4).await), vec![(Key::KEY_LEFTSHIFT, 1)]);

    assert_eq!(keys(&deck.pause().await), vec![(Key::KEY_LEFTSHIFT, 0)]);
    assert_eq!(deck.modifiers().await, vec![ev(L4)], "the pause forgot L4");
    assert!(keys(&deck.resume().await).is_empty(), "resume pressed Shift again");

    let l4_up = deck.release(L4).await;
    assert!(keys(&l4_up).is_empty(), "{:?}", keys(&l4_up));
    assert!(deck.modifiers().await.is_empty());
    assert!(deck.ledger_is_empty().await);
}

/// Everything makima emitted goes up at the pause, whichever button emitted it
/// and whether or not its binding is `no_pause` — a drag on L2 is dropped
/// rather than left holding the right mouse button in another window. L1 emits
/// nothing and stays in the modifier set.
#[tokio::test]
async fn a_pause_still_releases_everything_emitted() {
    let mut deck = Deck::new("release-all");
    deck.press(A).await;
    deck.press(L2).await;
    deck.press(L1).await;

    let pause = deck.pause().await;
    assert_eq!(sorted_keys(&pause), vec![(Key::KEY_ENTER, 0), (Key::BTN_RIGHT, 0)]);
    assert!(deck.ledger_is_empty().await);
    assert_eq!(deck.modifiers().await, vec![ev(L1)]);
}

// ── layer keys across a pause (issue #62) ─────────────────────────────────────

/// The reported bug: L1 held while the HUD opens is still L1 while it is open,
/// and after it closes L1 + X is the L1 layer's combo, not plain X. Nothing is
/// sent at the pause or the resume, and letting go of L1 afterwards sends
/// nothing either.
#[tokio::test]
async fn a_layer_key_held_across_a_pause_keeps_its_layer() {
    let mut deck = Deck::new("layer-across");
    assert!(keys(&deck.press(L1).await).is_empty(), "L1 = [] sends nothing");

    assert!(keys(&deck.pause().await).is_empty());
    assert_eq!(deck.modifiers().await, vec![ev(L1)], "the pause forgot L1");
    assert_eq!(deck.held_keys().await, vec![ev(L1)], "the HUD would show L1 as released");
    assert!(deck.ledger_is_empty().await, "pause leaves nothing recorded as emitted");

    assert!(keys(&deck.resume().await).is_empty(), "resume sent something");

    assert_eq!(
        keys(&deck.press(X).await),
        vec![(Key::KEY_LEFTCTRL, 1), (Key::KEY_C, 1)],
        "L1 + X after the pause has to be the L1 layer's combo",
    );
    deck.release(X).await;

    assert!(keys(&deck.release(L1).await).is_empty(), "releasing L1 after the pause sent something");
    assert!(deck.modifiers().await.is_empty());
    assert!(deck.held_keys().await.is_empty());
}

/// The control case from the report, which already worked and must keep
/// working: L1 pressed *during* the pause is tracked, and resolves its layer
/// after it.
#[tokio::test]
async fn a_layer_key_pressed_during_a_pause_still_works() {
    let mut deck = Deck::new("layer-during");
    deck.pause().await;
    deck.press(L1).await;
    assert_eq!(deck.modifiers().await, vec![ev(L1)]);
    assert_eq!(deck.held_keys().await, vec![ev(L1)]);
    assert!(keys(&deck.resume().await).is_empty());

    assert_eq!(keys(&deck.press(X).await), vec![(Key::KEY_LEFTCTRL, 1), (Key::KEY_C, 1)]);
}

// ── a modifier held across a pause is not a tap ───────────────────────────────

/// Steam held across the pause and used in the HUD: the combo sends nothing
/// while paused, so nothing marked Steam as used — the pause does. Releasing it
/// after the HUD closes is not a tap. (`an_unused_steam_press_taps` is the
/// baseline.)
#[tokio::test]
async fn a_modifier_held_across_a_pause_does_not_tap_on_release() {
    let mut deck = Deck::new("steam-across");
    deck.press(STEAM).await;
    assert!(!deck.modifier_was_activated().await, "precondition: Steam is armed");

    deck.pause().await;
    assert!(deck.modifier_was_activated().await, "the pause has to count Steam as used");
    let combo = [deck.press(X).await, deck.release(X).await].concat();
    assert!(presses(&combo).is_empty(), "a combo fired while paused");
    deck.resume().await;

    assert!(presses(&deck.release(STEAM).await).is_empty(), "Steam tapped after the pause");
    assert!(deck.modifiers().await.is_empty());
}

/// The same with nothing pressed in between: holding Steam while the HUD is
/// open and letting go afterwards is not a tap either.
#[tokio::test]
async fn a_modifier_only_held_across_a_pause_does_not_tap() {
    let mut deck = Deck::new("steam-held");
    let mut sent = deck.press(STEAM).await;
    sent.extend(deck.pause().await);
    sent.extend(deck.resume().await);
    sent.extend(deck.release(STEAM).await);
    assert!(presses(&sent).is_empty(), "Steam tapped after the pause: {:?}", keys(&sent));
}

/// Steam pressed *during* the pause and released after it: the press was
/// swallowed, so the release must not tap either. The flag has to be false at
/// the pause for this to matter, which is rare — it starts true, and a pause
/// with a custom modifier held sets it — so the test puts it there directly,
/// as if an unused modifier press were pending.
#[tokio::test]
async fn a_modifier_pressed_during_a_pause_does_not_tap_after_it() {
    let mut deck = Deck::new("steam-during");
    *deck.reader.modifier_was_activated.lock().await = false;

    let mut sent = deck.pause().await;
    sent.extend(deck.press(STEAM).await);
    assert!(!deck.modifier_was_activated().await, "precondition: still armed while paused");
    sent.extend(deck.resume().await);
    assert!(deck.modifier_was_activated().await, "resume has to count the held Steam as used");
    assert!(keys(&sent).is_empty(), "pause or resume sent something: {:?}", keys(&sent));

    assert!(presses(&deck.release(STEAM).await).is_empty(), "Steam tapped after the pause");
    assert!(deck.modifiers().await.is_empty());
}
