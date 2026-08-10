use super::*;
use evdev::Key;
use std::collections::HashMap;
use tokio;

/// Simulates the button lifecycle: press (store output), then release (retrieve + clear).
/// Verifies the emitted_outputs map is correctly populated and drained.
#[tokio::test]
async fn emitted_outputs_stores_and_releases_on_key_up() {
    let emitted_outputs: Arc<Mutex<HashMap<Event, Vec<Key>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let button = Event::Key(Key::BTN_START);

    // ── Press: base remap F10 ──
    {
        let mut eo = emitted_outputs.lock().await;
        eo.insert(button, vec![Key::KEY_F10]);
    }

    // ── Key-up: should find F10 and remove the entry ──
    {
        let mut eo = emitted_outputs.lock().await;
        let released = eo.remove(&button);
        assert!(released.is_some(), "entry must exist for held button on release");
        assert_eq!(released.unwrap(), vec![Key::KEY_F10]);
        assert!(eo.is_empty());
    }
}

/// When a combo fires mid-hold and overwrites the stored output, releasing the
/// button must release the NEW output, not the stale base remap (stored output
/// was already replaced when the combo fired its own store_emitted_outputs call).
#[tokio::test]
async fn emitted_outputs_overwritten_by_combo_releases_combo_keys() {
    let emitted_outputs: Arc<Mutex<HashMap<Event, Vec<Key>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let button = Event::Key(Key::BTN_START);

    // ── Press: base remap F10 ──
    {
        let mut eo = emitted_outputs.lock().await;
        eo.insert(button, vec![Key::KEY_F10]);
    }

    // ── Modifier activated mid-hold → combo overwrites output ──
    {
        let mut eo = emitted_outputs.lock().await;
        eo.insert(button, vec![Key::KEY_LEFTALT, Key::KEY_LEFTCTRL, Key::KEY_Z]);
    }

    // ── Key-up: should find the combo keys (last stored) ──
    {
        let mut eo = emitted_outputs.lock().await;
        let released = eo.remove(&button);
        assert!(released.is_some());
        assert_eq!(
            released.unwrap(),
            vec![Key::KEY_LEFTALT, Key::KEY_LEFTCTRL, Key::KEY_Z]
        );
        assert!(eo.is_empty());
    }
}

/// A button pressed in combo context (modifier already held, base remap never
/// emitted) has no entry in emitted_outputs before the combo stores its keys.
#[tokio::test]
async fn emitted_outputs_empty_before_first_store() {
    let emitted_outputs: Arc<Mutex<HashMap<Event, Vec<Key>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let button = Event::Key(Key::BTN_START);

    let released = emitted_outputs.lock().await.remove(&button);
    assert!(released.is_none(), "no entry exists before first insertion");
}

/// Non-key events (axes, scroll) are not tracked in emitted_outputs.
#[tokio::test]
async fn emitted_outputs_ignores_axis_events() {
    let emitted_outputs: Arc<Mutex<HashMap<Event, Vec<Key>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Axis events are not inserted (only Key events go through the tracking)
    let axis = Event::Axis(Axis::BTN_DPAD_UP);
    assert!(!matches!(axis, Event::Key(_)));

    // Verify the map is untouched for non-Key events
    let eo = emitted_outputs.lock().await;
    assert!(eo.is_empty());
}

/// Multiple held buttons store their outputs independently.
#[tokio::test]
async fn emitted_outputs_tracks_multiple_buttons_independently() {
    let emitted_outputs: Arc<Mutex<HashMap<Event, Vec<Key>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let start = Event::Key(Key::BTN_START);
    let north = Event::Key(Key::BTN_NORTH);

    // Press Start → F10
    emitted_outputs.lock().await.insert(start, vec![Key::KEY_F10]);
    // Press X → Backspace
    emitted_outputs.lock().await.insert(north, vec![Key::KEY_BACKSPACE]);

    // Release Start
    {
        let mut eo = emitted_outputs.lock().await;
        let released = eo.remove(&start);
        assert_eq!(released, Some(vec![Key::KEY_F10]));
        // X entry still present
        assert!(eo.contains_key(&north));
    }

    // Release X
    {
        let mut eo = emitted_outputs.lock().await;
        let released = eo.remove(&north);
        assert_eq!(released, Some(vec![Key::KEY_BACKSPACE]));
        assert!(eo.is_empty());
    }
}

// ── gaming_mode_suppresses ────────────────────────────────────────────────

/// Key-up (value=0) must NEVER be suppressed regardless of while_gaming.
/// Without this, releasing a held button in Gaming Mode leaves it stuck.
#[test]
fn key_up_never_suppressed() {
    assert!(!gaming_mode_suppresses(0, false),
        "key-up without while_gaming must not be suppressed");
    assert!(!gaming_mode_suppresses(0, true),
        "key-up with while_gaming must not be suppressed either");
}

/// Key-down of a normal (non-while_gaming) binding must be suppressed.
#[test]
fn key_down_suppressed_without_while_gaming() {
    assert!(gaming_mode_suppresses(1, false),
        "key-down without while_gaming must be suppressed");
}

/// Key-repeat (value=2) of a normal binding must also be suppressed.
#[test]
fn key_repeat_suppressed_without_while_gaming() {
    assert!(gaming_mode_suppresses(2, false),
        "key-repeat without while_gaming must be suppressed");
}

/// while_gaming bindings must pass through on both press and repeat.
#[test]
fn while_gaming_binding_not_suppressed() {
    assert!(!gaming_mode_suppresses(1, true),
        "key-down with while_gaming=true must NOT be suppressed");
    assert!(!gaming_mode_suppresses(2, true),
        "key-repeat with while_gaming=true must NOT be suppressed");
}

// ── gaming_mode_tracks_modifier ───────────────────────────────────────────

/// A mapped modifier (e.g. BTN_TL) must be tracked even in Gaming Mode
/// so combo state is consistent when Gaming Mode is disabled mid-hold.
#[test]
fn modifier_event_is_tracked_in_gaming_mode() {
    let btn_tl = Event::Key(Key::BTN_TL);
    let mapped = vec![btn_tl, Event::Key(Key::BTN_TR)];
    assert!(gaming_mode_tracks_modifier(&btn_tl, &mapped),
        "BTN_TL in mapped_modifiers must return true");
}

/// A non-modifier button must NOT be tracked as a modifier.
#[test]
fn non_modifier_event_not_tracked_in_gaming_mode() {
    let btn_south = Event::Key(Key::BTN_SOUTH);
    let mapped = vec![Event::Key(Key::BTN_TL), Event::Key(Key::BTN_TR)];
    assert!(!gaming_mode_tracks_modifier(&btn_south, &mapped),
        "BTN_SOUTH not in mapped_modifiers must return false");
}

/// Empty mapped_modifiers list → nothing is ever tracked.
#[test]
fn no_mapped_modifiers_nothing_tracked() {
    let btn_tl = Event::Key(Key::BTN_TL);
    assert!(!gaming_mode_tracks_modifier(&btn_tl, &[]),
        "empty mapped_modifiers must never match");
}

// ── stick_loop_active ─────────────────────────────────────────────────────

/// Normal operating state — not paused, not gaming: stick loop runs.
#[test]
fn stick_loop_active_normally() {
    assert!(stick_loop_active(false, false, false));
}

/// Gaming Mode active → stick loop must be suppressed.
#[test]
fn stick_loop_suppressed_in_gaming_mode() {
    assert!(!stick_loop_active(false, false, true),
        "gaming_mode=true must suppress stick loop");
    // Even cursor_when_paused cannot override gaming_mode suppression.
    assert!(!stick_loop_active(false, true, true),
        "cursor_when_paused does not override gaming_mode");
}

/// Paused without cursor_when_paused → stick loop suppressed.
#[test]
fn stick_loop_suppressed_when_paused() {
    assert!(!stick_loop_active(true, false, false),
        "paused=true without cursor_when_paused must suppress stick loop");
}

/// cursor_when_paused restores stick loop while paused, but not in Gaming Mode.
#[test]
fn stick_loop_cursor_when_paused_overrides_pause_but_not_gaming_mode() {
    assert!(stick_loop_active(true, true, false),
        "cursor_when_paused must allow stick loop while paused");
    assert!(!stick_loop_active(true, true, true),
        "cursor_when_paused must NOT allow stick loop in Gaming Mode");
}

/// Both paused and gaming_mode — stick loop suppressed.
#[test]
fn stick_loop_suppressed_when_both_paused_and_gaming_mode() {
    assert!(!stick_loop_active(true, false, true));
    assert!(!stick_loop_active(true, true, true));
}
