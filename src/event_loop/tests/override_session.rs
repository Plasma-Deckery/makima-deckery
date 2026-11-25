//! The override session end to end: lines through the control socket's own
//! `answer`, the shared session, the registry's layer, and the production
//! reader pressing buttons against it.
//!
//! The harness stands in for the device loop (see `Deck::settle`): it reads
//! the files again when the session asks for a reload, and records a rebuild
//! of the device tasks, which needs a device.

use super::*;
use crate::override_session::emergency::{EMERGENCY_EXIT_COMBO, EMERGENCY_EXIT_HOLD};

/// A press and its release, and what both did.
async fn tap(deck: &mut Deck, key: Key) -> Vec<OutputAction> {
    [deck.press(key).await, deck.release(key).await].concat()
}

/// The keyboard keys among `actions` — what a remap sends, as opposed to a
/// controller button passed through untouched.
fn keyboard_keys(actions: &[OutputAction]) -> Vec<(Key, i32)> {
    keys(actions).into_iter().filter(|(k, _)| k.code() < Key::BTN_0.code()).collect()
}

async fn begin_and_clear(deck: &mut Deck) {
    assert_eq!(deck.override_cmd("override begin").await.0, "ok 1");
    assert_eq!(deck.override_cmd("override clear").await.0, "ok");
}

/// After `clear` no button does anything a binding would: no key, no
/// command, no combo — the base config's and the app's are all gone.
#[tokio::test]
async fn begin_and_clear_leave_nothing_bound() {
    let mut deck = Deck::new("override-clear");
    begin_and_clear(&mut deck).await;

    let pressed = [
        tap(&mut deck, Key::BTN_SOUTH).await,
        tap(&mut deck, Key::BTN_THUMBL).await,
        deck.press(Key::BTN_TL).await,
        tap(&mut deck, Key::BTN_WEST).await,
        deck.release(Key::BTN_TL).await,
        tap(&mut deck, Key::BTN_TR2).await,
    ].concat();
    assert!(keyboard_keys(&pressed).is_empty(), "{:?}", keyboard_keys(&pressed));
    assert!(commands(&pressed).is_empty());
    assert!(still_down(&pressed).is_empty());
}

/// A binding set piece by piece fires; the one next to it stays dead.
#[tokio::test]
async fn a_set_binding_fires() {
    let mut deck = Deck::new("override-set");
    begin_and_clear(&mut deck).await;
    let (reply, _) = deck.override_cmd(r#"override set remap.A { "keys": ["KEY_ENTER"], "label": "Enter" }"#).await;
    assert_eq!(reply, "ok");

    assert_eq!(keyboard_keys(&tap(&mut deck, Key::BTN_SOUTH).await),
        vec![(Key::KEY_ENTER, 1), (Key::KEY_ENTER, 0)]);
    assert!(keyboard_keys(&tap(&mut deck, Key::BTN_EAST).await).is_empty());
}

/// Without `clear` the layer goes over the user's config: what it sets wins,
/// the rest is the user's.
#[tokio::test]
async fn without_clear_the_layer_goes_over_the_users_bindings() {
    let mut deck = Deck::new("override-over");
    deck.override_cmd("override begin").await;
    deck.override_cmd(r#"override set remap.A ["KEY_SPACE"]"#).await;
    assert_eq!(keyboard_keys(&tap(&mut deck, Key::BTN_SOUTH).await),
        vec![(Key::KEY_SPACE, 1), (Key::KEY_SPACE, 0)]);
    assert_eq!(keyboard_keys(&tap(&mut deck, Key::BTN_EAST).await),
        vec![(Key::KEY_ESC, 1), (Key::KEY_ESC, 0)]);
}

/// All or nothing: an invalid set is answered with an error and nothing of it
/// applies — the binding set before it still fires, nothing else does.
#[tokio::test]
async fn an_invalid_set_is_rejected_and_changes_nothing() {
    let mut deck = Deck::new("override-invalid");
    begin_and_clear(&mut deck).await;
    deck.override_cmd(r#"override set remap.A ["KEY_ENTER"]"#).await;

    for line in [
        r#"override set remap.BTN_NOPE ["KEY_ESC"]"#,
        r#"override set remap.B ["KEY_NOPE"]"#,
        r#"override set remap { "B": ["KEY_ESC"], "C": ["KEY_TAB"] }"#,
        r#"override set remap.B ["KEY_ESC""#,
        r#"override set device.names ["Steam Deck"]"#,
        r#"override set trackpad.right.mode "marble""#,
        "override set remap.B",
        "override wipe",
    ] {
        let (reply, _) = deck.override_cmd(line).await;
        assert!(reply.starts_with("error "), "{line}: {reply}");
        assert!(!reply.contains('\n'), "{reply:?}");
    }

    assert_eq!(keyboard_keys(&tap(&mut deck, Key::BTN_SOUTH).await),
        vec![(Key::KEY_ENTER, 1), (Key::KEY_ENTER, 0)]);
    assert!(keyboard_keys(&tap(&mut deck, Key::BTN_EAST).await).is_empty());
    assert!(deck.device_log.is_empty(), "a rejected set rebuilt the devices");
}

/// Only one session at a time.
#[tokio::test]
async fn a_second_begin_is_rejected() {
    let mut deck = Deck::new("override-twice");
    assert_eq!(deck.override_cmd("override begin").await.0, "ok 1");
    assert_eq!(deck.override_cmd("override begin").await.0, "error a session is already open (id 1)");
    assert_eq!(deck.override_cmd("override end").await.0, "ok");
    assert_eq!(deck.override_cmd("override begin").await.0, "ok 2");
}

#[tokio::test]
async fn commands_without_a_session_are_rejected() {
    let mut deck = Deck::new("override-none");
    for line in ["override end", "override clear", r#"override set remap.A []"#] {
        assert_eq!(deck.override_cmd(line).await.0, "error no session is open", "{line}");
    }
}

/// `end` drops the layer and reads the files again.
#[tokio::test]
async fn end_brings_the_disk_config_back() {
    let mut deck = Deck::new("override-end");
    begin_and_clear(&mut deck).await;
    assert_eq!(deck.override_cmd("override end").await.0, "ok");
    assert_eq!(deck.device_log, vec![DeviceRequest::Reload]);
    assert!(!deck.session.is_active());
    assert_eq!(keyboard_keys(&tap(&mut deck, Key::BTN_SOUTH).await),
        vec![(Key::KEY_ENTER, 1), (Key::KEY_ENTER, 0)]);
}

/// Holding Steam and QAM for the full time ends the session with every
/// binding cleared, and the configs on disk are back.
#[tokio::test(start_paused = true)]
async fn the_emergency_exit_restores_the_disk_config() {
    let mut deck = Deck::new("override-emergency");
    begin_and_clear(&mut deck).await;

    let [steam, qam] = EMERGENCY_EXIT_COMBO;
    let held = [deck.press(steam).await, deck.press(qam).await].concat();
    assert!(keyboard_keys(&held).is_empty());
    tokio::time::sleep(EMERGENCY_EXIT_HOLD + std::time::Duration::from_millis(50)).await;
    assert!(!deck.session.is_active(), "holding the combo did not end the session");
    deck.release(qam).await;
    deck.release(steam).await;

    deck.settle().await;
    assert_eq!(deck.device_log, vec![DeviceRequest::Reload]);
    assert_eq!(keyboard_keys(&tap(&mut deck, Key::BTN_SOUTH).await),
        vec![(Key::KEY_ENTER, 1), (Key::KEY_ENTER, 0)]);
}

/// Letting go of either button before the time is up is no exit, and the
/// next hold starts the count from the beginning.
#[tokio::test(start_paused = true)]
async fn a_short_hold_is_no_emergency_exit() {
    let mut deck = Deck::new("override-short");
    begin_and_clear(&mut deck).await;
    let [steam, qam] = EMERGENCY_EXIT_COMBO;
    let almost = EMERGENCY_EXIT_HOLD - std::time::Duration::from_millis(500);

    deck.press(steam).await;
    deck.press(qam).await;
    tokio::time::sleep(almost).await;
    deck.release(qam).await;
    deck.press(qam).await;
    tokio::time::sleep(almost).await;
    assert!(deck.session.is_active(), "two short holds added up to an exit");

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert!(!deck.session.is_active());
}

/// Without a session the combo is just two buttons.
#[tokio::test(start_paused = true)]
async fn the_combo_does_nothing_without_a_session() {
    let mut deck = Deck::new("override-combo-idle");
    let [steam, qam] = EMERGENCY_EXIT_COMBO;
    deck.press(steam).await;
    deck.press(qam).await;
    tokio::time::sleep(EMERGENCY_EXIT_HOLD * 2).await;
    deck.settle().await;
    assert!(deck.device_log.is_empty());
}

/// A config file changed during the session is held back, and applies once
/// the session ends.
#[tokio::test]
async fn a_file_change_waits_for_the_end_of_the_session() {
    let mut deck = Deck::new("override-file");
    deck.override_cmd("override begin").await;
    deck.write_base(&BASE.replace(r#"A     = ["KEY_ENTER"]"#, r#"A     = ["KEY_SPACE"]"#));
    deck.config_files_changed().await;
    assert_eq!(keyboard_keys(&tap(&mut deck, Key::BTN_SOUTH).await),
        vec![(Key::KEY_ENTER, 1), (Key::KEY_ENTER, 0)], "the file was read during the session");

    deck.override_cmd("override end").await;
    assert_eq!(keyboard_keys(&tap(&mut deck, Key::BTN_SOUTH).await),
        vec![(Key::KEY_SPACE, 1), (Key::KEY_SPACE, 0)]);
}

/// Opening a session ends Gaming Mode, and while it is open nothing turns it
/// back on — neither a request nor the double-click, which is an ordinary
/// press of QAM for as long as the session lasts.
#[tokio::test]
async fn gaming_mode_is_refused_during_a_session() {
    let mut deck = Deck::new("override-gaming");
    deck.set_gaming_mode(true).await;
    deck.override_cmd("override begin").await;
    assert!(!*deck.reader.gaming_mode.lock().await, "begin left Gaming Mode on");

    deck.set_gaming_mode(true).await;
    assert!(!*deck.reader.gaming_mode.lock().await);

    let clicks = [tap(&mut deck, Key::BTN_BASE).await, tap(&mut deck, Key::BTN_BASE).await].concat();
    assert!(!clicks.iter().any(|a| matches!(a, OutputAction::Internal(_))), "the double-click toggled");

    deck.override_cmd("override end").await;
    deck.set_gaming_mode(true).await;
    assert!(*deck.reader.gaming_mode.lock().await, "Gaming Mode stayed blocked after the end");
}

/// A command the user's configs bind may be bound; any other is refused.
#[tokio::test]
async fn a_command_outside_the_allowlist_is_rejected() {
    let mut deck = Deck::new("override-allowlist");
    begin_and_clear(&mut deck).await;

    let (reply, _) = deck.override_cmd(r#"override set commands.Start ["rm", "-rf", "/"]"#).await;
    assert!(reply.starts_with("error ") && reply.contains("rm"), "{reply}");
    let (reply, _) = deck.override_cmd(r#"override set commands.Start ["notify-send", "bye"]"#).await;
    assert!(reply.starts_with("error "), "{reply}");
    assert!(commands(&tap(&mut deck, Key::BTN_START).await).is_empty());

    let (reply, _) = deck.override_cmd(
        r#"override set commands.Start { "run": ["deckery-hud-toggle"], "label": "Toggle HUD" }"#).await;
    assert_eq!(reply, "ok");
    assert_eq!(commands(&tap(&mut deck, Key::BTN_START).await), vec![vec!["deckery-hud-toggle".to_string()]]);
}

/// The layer is above the focused app's override: switching to Firefox,
/// whose own A is Space, does not get out from under it.
#[tokio::test]
async fn a_window_switch_does_not_escape_the_layer() {
    let mut deck = Deck::on_kde("override-window");
    begin_and_clear(&mut deck).await;
    *deck.reader.active_client.lock().await =
        Client::Class("firefox".to_string(), String::new(), None);
    assert!(keyboard_keys(&tap(&mut deck, Key::BTN_SOUTH).await).is_empty());

    deck.override_cmd(r#"override set remap.A ["KEY_ENTER"]"#).await;
    assert_eq!(keyboard_keys(&tap(&mut deck, Key::BTN_SOUTH).await),
        vec![(Key::KEY_ENTER, 1), (Key::KEY_ENTER, 0)]);
}

/// A trackpad mode is read when the device session starts, so changing it
/// asks the device loop for a rebuild — and the base config that rebuild
/// reads has the new mode, the pad's other settings kept.
#[tokio::test]
async fn a_trackpad_mode_change_rebuilds_the_device_session() {
    let mut deck = Deck::new("override-trackpad");
    deck.override_cmd("override begin").await;
    deck.override_cmd(r#"override set remap.A ["KEY_SPACE"]"#).await;
    assert!(deck.device_log.is_empty(), "a binding rebuilt the devices");

    assert_eq!(deck.override_cmd(r#"override set trackpad.right.mode "trackball""#).await.0, "ok");
    assert_eq!(deck.device_log, vec![DeviceRequest::Relaunch]);
    let base = deck.reader.registry.base_configs().remove(0);
    assert_eq!(base.trackpad.right.mode, "trackball");
    assert_eq!(base.trackpad.left.mode, "disabled");

    deck.override_cmd("override end").await;
    let base = deck.reader.registry.base_configs().remove(0);
    assert_eq!(base.trackpad.right.mode, "mt-trackpad");
}

/// The state export says a session is open and shows the layer's bindings,
/// so the HUD draws only what the tour unlocked — and no Gaming Mode trigger,
/// which is off for the session.
#[tokio::test]
async fn the_state_export_shows_the_session() {
    let mut deck = Deck::new("override-state");
    let (_, begun) = deck.override_cmd("override begin").await;
    let state = last_published(&begun).expect("begin publishes");
    assert_eq!(state["context"]["override_active"], serde_json::json!(true));
    assert_eq!(state["gaming_mode_trigger"], serde_json::Value::Null);
    assert!(state["bindings"].get("BTN_EAST").is_some());

    let (_, cleared) = deck.override_cmd("override clear").await;
    let state = last_published(&cleared).expect("clear publishes");
    assert_eq!(state["bindings"], serde_json::json!({}));

    let (_, set) = deck.override_cmd(r#"override set remap.A { "keys": ["KEY_ENTER"], "label": "Enter" }"#).await;
    let state = last_published(&set).expect("set publishes");
    let bound: Vec<&String> = state["bindings"].as_object().unwrap().keys().collect();
    assert_eq!(bound, vec!["BTN_SOUTH"]);

    let (_, ended) = deck.override_cmd("override end").await;
    let state = last_published(&ended).expect("end publishes");
    assert_eq!(state["context"]["override_active"], serde_json::json!(false));
    assert_eq!(state["gaming_mode_trigger"]["key"], serde_json::json!("BTN_BASE"));
    assert!(state["bindings"].get("BTN_EAST").is_some());
}

/// The session is in memory only: makima started again over the same files
/// has none, and the user's bindings are back.
#[tokio::test]
async fn a_restart_ends_the_session() {
    let mut deck = Deck::new("override-restart");
    begin_and_clear(&mut deck).await;
    assert!(keyboard_keys(&tap(&mut deck, Key::BTN_SOUTH).await).is_empty());

    let mut deck = deck.restart();
    assert!(!deck.session.is_active());
    assert_eq!(keyboard_keys(&tap(&mut deck, Key::BTN_SOUTH).await),
        vec![(Key::KEY_ENTER, 1), (Key::KEY_ENTER, 0)]);
    assert_eq!(deck.override_cmd("override end").await.0, "error no session is open");
}
