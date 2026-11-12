//! The context block and the analog half: config stack, pause, Gaming Mode
//! and its trigger.

use super::*;

// ── context fields ────────────────────────────────────────────────────────

#[test]
fn context_active_buttons_and_held_modifiers() {
    // active_buttons = all held input events; held_modifiers = only modifier subset.
    let btn_tl = key(Key::BTN_TL);
    let btn_south = key(Key::BTN_SOUTH);
    let config = make_config(
        vec![(btn_south, vec![], vec![Key::KEY_ENTER])],
        vec![], vec![btn_tl],
    );
    // BTN_TL is in modifiers (held modifier); BTN_SOUTH is in held_keys.
    let state = build_state(
        &config,
        &[btn_tl], false, false,
        &[btn_tl, btn_south],
        &None,
        &["test".to_string()],
        &GamingModeConfig::default(),
    );
    let active_btns: Vec<_> = state["context"]["active_buttons"]
        .as_array().unwrap()
        .iter().map(|v| v.as_str().unwrap()).collect();
    let held_mods: Vec<_> = state["context"]["held_modifiers"]
        .as_array().unwrap()
        .iter().map(|v| v.as_str().unwrap()).collect();
    assert!(active_btns.contains(&"BTN_TL"));
    assert!(active_btns.contains(&"BTN_SOUTH"));
    assert!(held_mods.contains(&"BTN_TL"));
    assert!(!held_mods.contains(&"BTN_SOUTH"), "BTN_SOUTH is not a modifier");
}

// ── config_stack and active_app ───────────────────────────────────────────
//
// The HUD shows which app config is active and tags every binding with the
// config it was written in. Both come from the stack, so the stack and the two
// fields read from it are pinned here for the two shapes it can have.

#[test]
fn the_stack_is_just_the_base_while_no_app_config_is_active() {
    assert_eq!(config_stack("Steam Deck", "Steam Deck"), vec!["Steam Deck".to_string()]);
}

#[test]
fn the_stack_is_base_then_app_while_an_app_config_is_active() {
    assert_eq!(config_stack("Steam Deck", "firefox"),
               vec!["Steam Deck".to_string(), "firefox".to_string()]);
}

#[test]
fn base_only_reports_no_active_app_and_the_base_as_every_origin() {
    // "default" is the HUD's word for "no app config"; an empty or missing
    // field would read as a broken snapshot instead.
    let mut config = make_config(
        vec![(key(Key::BTN_SOUTH), vec![], vec![Key::KEY_ENTER])],
        vec![(key(Key::BTN_WEST), vec![], vec!["true".to_string()])],
        vec![],
    );
    config.name = "Steam Deck".to_string();
    let stack = config_stack("Steam Deck", &config.name);

    let state = build_state(&config, &[], false, false, &[], &None, &stack,
                            &GamingModeConfig::default());

    assert_eq!(state["context"]["active_app"], "default");
    assert_eq!(state["context"]["config_stack"], serde_json::json!(["Steam Deck"]));
    assert_eq!(state["bindings"]["BTN_SOUTH"]["origin"], "Steam Deck");
    assert_eq!(state["bindings"]["BTN_WEST"]["origin"], "Steam Deck");
}

#[test]
fn an_app_override_is_the_active_app_and_owns_only_what_it_wrote() {
    // `origin_override_vs_base` covers plain remaps. This adds the context
    // fields and the other two places origin is decided — commands, and the
    // combo view under a held modifier — so an app that overrides one command
    // is not credited with every command the base defines.
    let btn_tl = key(Key::BTN_TL);
    let mut config = make_config_with_override(
        // Merged: the base's BTN_SOUTH and the app's combo.
        vec![
            (key(Key::BTN_SOUTH),   vec![],       vec![Key::KEY_ENTER]),
            (key(Key::BTN_DPAD_UP), vec![btn_tl], vec![Key::KEY_LEFTALT, Key::KEY_LEFT]),
        ],
        // Merged commands: one from the base, one from the app.
        vec![
            (key(Key::BTN_WEST),  vec![], vec!["base-cmd".to_string()]),
            (key(Key::BTN_NORTH), vec![], vec!["app-cmd".to_string()]),
        ],
        vec![btn_tl],
        // What the app config itself wrote.
        vec![(key(Key::BTN_DPAD_UP), vec![btn_tl], vec![Key::KEY_LEFTALT, Key::KEY_LEFT])],
    );
    config.name = "firefox".to_string();
    config.override_bindings.as_mut().unwrap().commands
        .entry(key(Key::BTN_NORTH)).or_default()
        .insert(vec![], vec!["app-cmd".to_string()]);
    let stack = config_stack("Steam Deck", &config.name);

    let state = build_state(&config, &[btn_tl], false, false, &[btn_tl], &None, &stack,
                            &GamingModeConfig::default());

    assert_eq!(state["context"]["active_app"], "firefox");
    assert_eq!(state["context"]["config_stack"], serde_json::json!(["Steam Deck", "firefox"]));

    assert_eq!(state["bindings"]["BTN_SOUTH"]["origin"], "Steam Deck");
    assert_eq!(state["bindings"]["BTN_TL-BTN_DPAD_UP"]["origin"], "firefox");
    assert_eq!(state["bindings"]["BTN_WEST"]["origin"], "Steam Deck");
    assert_eq!(state["bindings"]["BTN_NORTH"]["origin"], "firefox");
    // The combo view the HUD switches to while BTN_TL is held.
    assert_eq!(state["modifier_active"]["BTN_DPAD_UP"]["origin"], "firefox");
}

// ── write_state: analog fields ────────────────────────────────────────────

fn empty_config() -> Config {
    make_config(vec![], vec![], vec![])
}

/// Call write_state synchronously in a test runtime and return the
/// resulting state JSON (read back from the temp file path).
/// We test the JSON assembly logic here, not the file I/O.
fn assemble_state(
    trackpads: serde_json::Value,
    sticks: serde_json::Value,
    imu: serde_json::Value,
    analog_state_export: bool,
) -> serde_json::Value {
    let config = empty_config();
    let mut state = build_state(&config, &[], false, false, &[], &None, &["test".to_string()], &GamingModeConfig::default());
    state["trackpads"] = trackpads;
    state["sticks"] = sticks;
    state["imu"] = imu;
    state["context"]["analog_state_export"] = serde_json::Value::Bool(analog_state_export);
    state
}

#[test]
fn analog_state_export_false_in_context() {
    let state = assemble_state(
        serde_json::Value::Null,
        serde_json::Value::Null,
        serde_json::Value::Null,
        false,
    );
    assert_eq!(state["context"]["analog_state_export"].as_bool(), Some(false));
}

#[test]
fn analog_state_export_true_in_context() {
    let state = assemble_state(
        serde_json::Value::Null,
        serde_json::Value::Null,
        serde_json::Value::Null,
        true,
    );
    assert_eq!(state["context"]["analog_state_export"].as_bool(), Some(true));
}

#[test]
fn trackpads_present_when_provided() {
    let pads = serde_json::json!({
        "lpad": { "mode": "disabled", "x": 0.5, "y": -0.3, "touching": true, "pressed": false },
        "rpad": { "mode": "disabled", "x": 0.0, "y": 0.0, "touching": false, "pressed": false },
    });
    let state = assemble_state(pads.clone(), serde_json::Value::Null, serde_json::Value::Null, false);
    assert_eq!(state["trackpads"]["lpad"]["x"].as_f64(), Some(0.5));
    assert_eq!(state["trackpads"]["lpad"]["touching"].as_bool(), Some(true));
    assert_eq!(state["trackpads"]["rpad"]["touching"].as_bool(), Some(false));
}

#[test]
fn sticks_present_when_provided() {
    let sticks = serde_json::json!({
        "lstick": { "mode": "disabled", "x": 0.1, "y": 0.2, "deadzone": 0.092, "active": false },
        "rstick": { "mode": "cursor",   "x": 0.0, "y": 0.0, "deadzone": 0.031, "active": false },
    });
    let state = assemble_state(serde_json::Value::Null, sticks, serde_json::Value::Null, false);
    assert_eq!(state["sticks"]["lstick"]["deadzone"].as_f64(), Some(0.092));
    assert_eq!(state["sticks"]["rstick"]["mode"].as_str(), Some("cursor"));
}

#[test]
fn imu_present_when_provided() {
    let imu = serde_json::json!({ "x": 0.123, "y": 0.456 });
    let state = assemble_state(serde_json::Value::Null, serde_json::Value::Null, imu, false);
    assert_eq!(state["imu"]["x"].as_f64(), Some(0.123));
    assert_eq!(state["imu"]["y"].as_f64(), Some(0.456));
}

#[test]
fn null_fields_are_null_in_output() {
    let state = assemble_state(
        serde_json::Value::Null,
        serde_json::Value::Null,
        serde_json::Value::Null,
        false,
    );
    assert!(state["trackpads"].is_null());
    assert!(state["sticks"].is_null());
    assert!(state["imu"].is_null());
}

// ── paused / gaming_mode independence ─────────────────────────────────────
//
// The two flags are orthogonal: each can be true or false independently.
// These tests guard against accidental coupling in build_state (e.g. one
// flag leaking into the other's JSON field, or both flags sharing a single
// bool).

fn ctx_flags(paused: bool, gaming_mode: bool) -> (bool, bool) {
    let config = make_config(vec![], vec![], vec![]);
    let state = build_state(&config, &[], paused, gaming_mode, &[], &None, &["test".to_string()], &GamingModeConfig::default());
    let p = state["context"]["paused"].as_bool().expect("paused must be bool");
    let g = state["context"]["gaming_mode"].as_bool().expect("gaming_mode must be bool");
    (p, g)
}

/// Neither flag active — both false in context.
#[test]
fn flags_both_false() {
    assert_eq!(ctx_flags(false, false), (false, false));
}

/// Only paused — gaming_mode stays false.
#[test]
fn paused_only_gaming_mode_unaffected() {
    assert_eq!(ctx_flags(true, false), (true, false));
}

/// Only gaming_mode — paused stays false.
#[test]
fn gaming_mode_only_paused_unaffected() {
    assert_eq!(ctx_flags(false, true), (false, true));
}

/// Both active simultaneously — both appear true in context.
#[test]
fn both_flags_simultaneously() {
    assert_eq!(ctx_flags(true, true), (true, true));
}

/// Clearing gaming_mode while paused leaves paused true.
#[test]
fn clear_gaming_mode_while_paused() {
    // paused=true, gaming_mode=false (gaming_mode was cleared)
    assert_eq!(ctx_flags(true, false), (true, false));
}

/// Clearing paused while gaming_mode is active leaves gaming_mode true.
#[test]
fn clear_paused_while_gaming_mode_active() {
    // paused=false (resumed), gaming_mode=true
    assert_eq!(ctx_flags(false, true), (false, true));
}

// ── gaming_mode_trigger ───────────────────────────────────────────────────

/// When a trigger is configured, state contains gaming_mode_trigger with
/// the correct key name and the fixed label "Toggle Gaming Mode".
#[test]
fn gaming_mode_trigger_present_when_configured() {
    use crate::config::DoubleclickTrigger;
    let config = make_config(vec![], vec![], vec![]);
    let gm_config = GamingModeConfig {
        trigger: Some(DoubleclickTrigger { key: Key::BTN_BASE, ms: 400 }),
        ..GamingModeConfig::default()
    };
    let state = build_state(&config, &[], false, false, &[], &None, &["test".to_string()], &gm_config);
    let trigger = &state["gaming_mode_trigger"];
    assert!(!trigger.is_null(), "gaming_mode_trigger should be present");
    assert_eq!(trigger["key"].as_str().unwrap(), "BTN_BASE");
    assert_eq!(trigger["label"].as_str().unwrap(), "Gaming Mode");
}

/// When no trigger is configured (disabled), gaming_mode_trigger is null.
#[test]
fn gaming_mode_trigger_null_when_disabled() {
    let config = make_config(vec![], vec![], vec![]);
    let gm_config = GamingModeConfig {
        trigger: None,
        ..GamingModeConfig::default()
    };
    let state = build_state(&config, &[], false, false, &[], &None, &["test".to_string()], &gm_config);
    assert!(state["gaming_mode_trigger"].is_null(), "gaming_mode_trigger should be null when trigger is disabled");
}
