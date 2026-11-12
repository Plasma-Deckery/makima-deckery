//! What the snapshot says about modifiers: the active outputs, which
//! modifiers are available, and the combo view under a held one.

use super::*;

// ── active_outputs ────────────────────────────────────────────────────────

#[test]
fn active_outputs_base_remap() {
    // BTN_SOUTH → KEY_ENTER, no modifiers held.
    let config = make_config(
        vec![(key(Key::BTN_SOUTH), vec![], vec![Key::KEY_ENTER])],
        vec![], vec![],
    );
    let state = build_state(&config, &[], false, false, &[key(Key::BTN_SOUTH)], &None, &["test".to_string()], &GamingModeConfig::default());
    assert_eq!(active_outputs(&state), vec!["KEY_ENTER"]);
}

#[test]
fn active_outputs_combo_remap() {
    // BTN_TL-BTN_NORTH → [KEY_LEFTCTRL, KEY_C]. BTN_TL held as modifier.
    let btn_tl = key(Key::BTN_TL);
    let btn_north = key(Key::BTN_NORTH);
    let config = make_config(
        vec![(btn_north, vec![btn_tl], vec![Key::KEY_LEFTCTRL, Key::KEY_C])],
        vec![], vec![btn_tl],
    );
    // modifiers = [BTN_TL], held_keys = [BTN_NORTH]
    let state = build_state(&config, &[btn_tl], false, false, &[btn_north], &None, &["test".to_string()], &GamingModeConfig::default());
    // KEY_LEFTCTRL sorts before KEY_C
    assert_eq!(active_outputs(&state), vec!["KEY_LEFTCTRL", "KEY_C"]);
}

#[test]
fn active_outputs_fallback_remap() {
    // BTN_SOUTH has base remap KEY_ENTER but no BTN_TL combo.
    // Holding BTN_TL should fall back to KEY_ENTER (the Ctrl+Enter scenario).
    let btn_tl = key(Key::BTN_TL);
    let btn_south = key(Key::BTN_SOUTH);
    let config = make_config(
        vec![(btn_south, vec![], vec![Key::KEY_ENTER])],
        vec![], vec![btn_tl],
    );
    let state = build_state(&config, &[btn_tl], false, false, &[btn_south], &None, &["test".to_string()], &GamingModeConfig::default());
    assert_eq!(active_outputs(&state), vec!["KEY_ENTER"]);
}

#[test]
fn active_outputs_command_suppresses_remap() {
    // BTN_DPAD_UP has base remap KEY_UP, but BTN_TL-BTN_DPAD_UP is a command.
    // When BTN_TL held + BTN_DPAD_UP pressed, no key output (command takes over).
    let btn_tl = key(Key::BTN_TL);
    let btn_dpad_up = key(Key::BTN_DPAD_UP);
    let config = make_config(
        vec![(btn_dpad_up, vec![], vec![Key::KEY_UP])],
        vec![(btn_dpad_up, vec![btn_tl], vec!["previous-desktop".to_string()])],
        vec![btn_tl],
    );
    let state = build_state(&config, &[btn_tl], false, false, &[btn_dpad_up], &None, &["test".to_string()], &GamingModeConfig::default());
    assert_eq!(active_outputs(&state), Vec::<String>::new());
}

#[test]
fn active_outputs_unbound_is_empty() {
    let config = make_config(vec![], vec![], vec![]);
    let state = build_state(&config, &[], false, false, &[key(Key::BTN_SOUTH)], &None, &["test".to_string()], &GamingModeConfig::default());
    assert_eq!(active_outputs(&state), Vec::<String>::new());
}

// ── available_modifiers ───────────────────────────────────────────────────

#[test]
fn available_modifiers_shows_both_when_none_held() {
    // Config has BTN_TL and BTN_TR combos. With no mods held, both should appear.
    let btn_tl = key(Key::BTN_TL);
    let btn_tr = key(Key::BTN_TR);
    let btn_north = key(Key::BTN_NORTH);
    let config = make_config(
        vec![
            (btn_north, vec![btn_tl], vec![Key::KEY_LEFTCTRL, Key::KEY_C]),
            (btn_north, vec![btn_tr], vec![Key::KEY_F5]),
        ],
        vec![], vec![btn_tl, btn_tr],
    );
    let state = build_state(&config, &[], false, false, &[], &None, &["test".to_string()], &GamingModeConfig::default());
    let mut avail = available_modifiers(&state);
    avail.sort();
    assert!(avail.contains(&"BTN_TL".to_string()));
    assert!(avail.contains(&"BTN_TR".to_string()));
}

#[test]
fn available_modifiers_filters_satisfied() {
    // BTN_TL held. BTN_TL-only combos are now active (satisfied).
    // BTN_TR has a BTN_TL-BTN_TR combo → should still appear as available.
    let btn_tl = key(Key::BTN_TL);
    let btn_tr = key(Key::BTN_TR);
    let btn_north = key(Key::BTN_NORTH);
    let btn_south = key(Key::BTN_SOUTH);
    let config = make_config(
        vec![
            (btn_north, vec![btn_tl], vec![Key::KEY_LEFTCTRL, Key::KEY_C]),
            (btn_south, vec![btn_tl, btn_tr], vec![Key::KEY_F1]),
        ],
        vec![], vec![btn_tl, btn_tr],
    );
    // BTN_TL is active_input_mod → filtered out of available_modifiers.
    // BTN_TR qualifies because BTN_TL-BTN_TR-BTN_SOUTH exists and BTN_TL ⊆ that combo.
    let state = build_state(&config, &[btn_tl], false, false, &[], &None, &["test".to_string()], &GamingModeConfig::default());
    let avail = available_modifiers(&state);
    assert!(!avail.contains(&"BTN_TL".to_string()), "BTN_TL is already active");
    assert!(avail.contains(&"BTN_TR".to_string()), "BTN_TR unlocks a BTN_TL+BTN_TR combo");
}

// ── available_modifiers: has_app_combos ──────────────────────────────────────

#[test]
fn has_app_combos_false_when_no_override() {
    // No override_bindings → has_app_combos must be false for all modifiers.
    let btn_tl = key(Key::BTN_TL);
    let btn_north = key(Key::BTN_NORTH);
    let config = make_config(
        vec![(btn_north, vec![btn_tl], vec![Key::KEY_F5])],
        vec![], vec![btn_tl],
    );
    let state = build_state(&config, &[], false, false, &[], &None, &["test".to_string()], &GamingModeConfig::default());
    assert!(!avail_mod_has_app_combos(&state, "BTN_TL"),
        "no override_bindings → has_app_combos must be false");
}

#[test]
fn has_app_combos_true_when_override_has_qualifying_combo() {
    // override_bindings has a BTN_TL combo → has_app_combos must be true for BTN_TL.
    let btn_tl = key(Key::BTN_TL);
    let btn_tr = key(Key::BTN_TR);
    let btn_north = key(Key::BTN_NORTH);
    let btn_south = key(Key::BTN_SOUTH);
    // Base: BTN_TL has a base combo (BTN_NORTH+BTN_TL), BTN_TR has a base combo.
    // Override: only BTN_TL has an app-specific combo (BTN_SOUTH+BTN_TL).
    let config = make_config_with_override(
        vec![
            (btn_north, vec![btn_tl], vec![Key::KEY_F5]),
            (btn_south, vec![btn_tl], vec![Key::KEY_F6]),
            (btn_north, vec![btn_tr], vec![Key::KEY_F7]),
        ],
        vec![], vec![btn_tl, btn_tr],
        vec![(btn_south, vec![btn_tl], vec![Key::KEY_F6])], // app-specific
    );
    let state = build_state(&config, &[], false, false, &[], &None, &["test".to_string(), "firefox".to_string()], &GamingModeConfig::default());
    assert!(avail_mod_has_app_combos(&state, "BTN_TL"),
        "BTN_TL has an app-specific combo → has_app_combos must be true");
    assert!(!avail_mod_has_app_combos(&state, "BTN_TR"),
        "BTN_TR has no app-specific combo → has_app_combos must be false");
}

#[test]
fn has_app_combos_false_when_override_has_no_qualifying_combo() {
    // override_bindings exists but contains only BTN_TR combos, not BTN_TL.
    // BTN_TL must still report has_app_combos: false.
    let btn_tl = key(Key::BTN_TL);
    let btn_tr = key(Key::BTN_TR);
    let btn_north = key(Key::BTN_NORTH);
    let btn_south = key(Key::BTN_SOUTH);
    let config = make_config_with_override(
        vec![
            (btn_north, vec![btn_tl], vec![Key::KEY_F5]),
            (btn_south, vec![btn_tr], vec![Key::KEY_F6]),
        ],
        vec![], vec![btn_tl, btn_tr],
        vec![(btn_south, vec![btn_tr], vec![Key::KEY_F6])], // only BTN_TR is app-specific
    );
    let state = build_state(&config, &[], false, false, &[], &None, &["test".to_string(), "firefox".to_string()], &GamingModeConfig::default());
    assert!(!avail_mod_has_app_combos(&state, "BTN_TL"),
        "override has no BTN_TL combo → has_app_combos must be false");
    assert!(avail_mod_has_app_combos(&state, "BTN_TR"),
        "override has a BTN_TR combo → has_app_combos must be true");
}

// ── modifier_active ───────────────────────────────────────────────────────

#[test]
fn modifier_active_exact_match_only() {
    // BTN_TL held. Should show BTN_TL combos but NOT BTN_TL-BTN_TR combos.
    let btn_tl = key(Key::BTN_TL);
    let btn_tr = key(Key::BTN_TR);
    let btn_north = key(Key::BTN_NORTH);
    let btn_south = key(Key::BTN_SOUTH);
    let config = make_config(
        vec![
            (btn_north, vec![btn_tl], vec![Key::KEY_LEFTCTRL, Key::KEY_C]),
            (btn_south, vec![btn_tl, btn_tr], vec![Key::KEY_F1]),
        ],
        vec![], vec![btn_tl, btn_tr],
    );
    let state = build_state(&config, &[btn_tl], false, false, &[], &None, &["test".to_string()], &GamingModeConfig::default());
    let keys = modifier_active_keys(&state);
    assert!(keys.contains(&"BTN_NORTH".to_string()), "BTN_TL-BTN_NORTH should appear");
    assert!(!keys.contains(&"BTN_SOUTH".to_string()), "BTN_TL-BTN_TR-BTN_SOUTH must not leak in");
}

#[test]
fn modifier_active_includes_commands() {
    // Commands under a modifier should appear in modifier_active, not just remaps.
    let btn_tl = key(Key::BTN_TL);
    let btn_dpad_up = key(Key::BTN_DPAD_UP);
    let config = make_config(
        vec![],
        vec![(btn_dpad_up, vec![btn_tl], vec!["previous-desktop".to_string()])],
        vec![btn_tl],
    );
    let state = build_state(&config, &[btn_tl], false, false, &[], &None, &["test".to_string()], &GamingModeConfig::default());
    let keys = modifier_active_keys(&state);
    assert!(keys.contains(&"BTN_DPAD_UP".to_string()));
    assert_eq!(
        state["modifier_active"]["BTN_DPAD_UP"]["kind"].as_str().unwrap(),
        "command"
    );
}

#[test]
fn modifier_active_label_propagated() {
    // label set on a combo binding should appear in modifier_active.
    let btn_tl = key(Key::BTN_TL);
    let btn_north = key(Key::BTN_NORTH);
    let mut config = make_config(
        vec![(btn_north, vec![btn_tl], vec![Key::KEY_LEFTCTRL, Key::KEY_C])],
        vec![], vec![btn_tl],
    );
    config.bindings.labels.insert((btn_north, vec![btn_tl]), "Copy".to_string());
    let state = build_state(&config, &[btn_tl], false, false, &[], &None, &["test".to_string()], &GamingModeConfig::default());
    assert_eq!(
        state["modifier_active"]["BTN_NORTH"]["label"].as_str().unwrap(),
        "Copy"
    );
}

// ── active_outputs: multi-modifier combo ──────────────────────────────────

#[test]
fn active_outputs_multi_modifier_combo() {
    // L1+R1+DPad_Up combo should produce its own output, not fall back to base.
    let btn_tl = key(Key::BTN_TL);
    let btn_tr = key(Key::BTN_TR);
    let btn_dpad_up = key(Key::BTN_DPAD_UP);
    let config = make_config(
        vec![
            (btn_dpad_up, vec![], vec![Key::KEY_UP]),
            (btn_dpad_up, vec![btn_tl, btn_tr], vec![Key::KEY_F1]),
        ],
        vec![], vec![btn_tl, btn_tr],
    );
    // Sort the modifier combo the same way resolve_binding expects it.
    let mut mods = vec![btn_tl, btn_tr];
    mods.sort();
    let state = build_state(&config, &mods, false, false, &[btn_dpad_up], &None, &["test".to_string()], &GamingModeConfig::default());
    assert_eq!(active_outputs(&state), vec!["KEY_F1"]);
}

// ── indirect modifier detection ───────────────────────────────────────────

#[test]
fn indirect_modifier_match_via_output_key() {
    // BTN_TL is a custom modifier whose base remap is KEY_LEFTCTRL.
    // If modifiers contains Event::Key(KEY_LEFTCTRL) instead of BTN_TL directly,
    // BTN_TL should still be detected as an active modifier for combo resolution.
    let btn_tl = key(Key::BTN_TL);
    let btn_north = key(Key::BTN_NORTH);
    let config = make_config(
        vec![
            (btn_tl, vec![], vec![Key::KEY_LEFTCTRL]),  // BTN_TL → KEY_LEFTCTRL
            (btn_north, vec![btn_tl], vec![Key::KEY_LEFTCTRL, Key::KEY_C]),
        ],
        vec![], vec![btn_tl],
    );
    // modifiers contains KEY_LEFTCTRL (the output key), not BTN_TL
    let state = build_state(
        &config,
        &[Event::Key(Key::KEY_LEFTCTRL)], false, false, &[], &None, &["test".to_string()],
        &GamingModeConfig::default(),
    );
    // BTN_TL should be detected as active → modifier_active shows BTN_TL-BTN_NORTH combo
    let keys = modifier_active_keys(&state);
    assert!(keys.contains(&"BTN_NORTH".to_string()),
        "indirect modifier detection: BTN_NORTH combo should appear when KEY_LEFTCTRL held");
}
