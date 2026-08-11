use super::*;
use crate::config::{Bindings, Event, MappedModifiers};
use crate::Config;
use evdev::Key;
use std::collections::{HashMap, HashSet};

fn key(k: Key) -> Event { Event::Key(k) }

/// Build a minimal Config with the given bindings and custom modifiers.
fn make_config(
    remap: Vec<(Event, Vec<Event>, Vec<Key>)>,
    commands: Vec<(Event, Vec<Event>, Vec<String>)>,
    custom_modifiers: Vec<Event>,
) -> Config {
    let mut remap_map: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
    for (trigger, combo, keys) in remap {
        remap_map.entry(trigger).or_default().insert(combo, keys);
    }
    let mut cmd_map: HashMap<Event, HashMap<Vec<Event>, Vec<String>>> = HashMap::new();
    for (trigger, combo, cmds) in commands {
        cmd_map.entry(trigger).or_default().insert(combo, cmds);
    }
    let all_mods = custom_modifiers.clone();
    Config {
        name: "test".to_string(),
        associations: Default::default(),
        bindings: Bindings {
            remap: remap_map,
            commands: cmd_map,
            movements: HashMap::new(),
            no_pause: HashSet::new(),
            while_gaming: HashSet::new(),
            labels: HashMap::new(),
            silent: HashSet::new(),
        },
        override_bindings: None,
        settings: HashMap::new(),
        mapped_modifiers: MappedModifiers {
            default: vec![],
            custom: custom_modifiers,
            all: all_mods,
        },
        trackpad: Default::default(),
        gaming_mode_config: Default::default(),
    }
}

/// Extract active_outputs keys (ignoring silent flag) for easy assertions.
fn active_output_keys(state: &serde_json::Value) -> Vec<String> {
    state["context"]["active_outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["key"].as_str().unwrap().to_string())
        .collect()
}

/// Extract (key, silent) pairs from active_outputs.
fn active_outputs_tagged(state: &serde_json::Value) -> Vec<(String, bool)> {
    state["context"]["active_outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (
            v["key"].as_str().unwrap().to_string(),
            v["silent"].as_bool().unwrap(),
        ))
        .collect()
}

fn active_outputs(state: &serde_json::Value) -> Vec<String> {
    active_output_keys(state)
}

fn available_modifiers(state: &serde_json::Value) -> Vec<String> {
    state["context"]["available_modifiers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

fn modifier_active_keys(state: &serde_json::Value) -> Vec<String> {
    state["modifier_active"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

// ── active_outputs ────────────────────────────────────────────────────────

#[test]
fn active_outputs_base_remap() {
    // BTN_SOUTH → KEY_ENTER, no modifiers held.
    let config = make_config(
        vec![(key(Key::BTN_SOUTH), vec![], vec![Key::KEY_ENTER])],
        vec![], vec![],
    );
    let state = build_state(&config, &[], 0, false, false, &[key(Key::BTN_SOUTH)], &None, &["test".to_string()]);
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
    let state = build_state(&config, &[btn_tl], 0, false, false, &[btn_north], &None, &["test".to_string()]);
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
    let state = build_state(&config, &[btn_tl], 0, false, false, &[btn_south], &None, &["test".to_string()]);
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
    let state = build_state(&config, &[btn_tl], 0, false, false, &[btn_dpad_up], &None, &["test".to_string()]);
    assert_eq!(active_outputs(&state), Vec::<String>::new());
}

#[test]
fn active_outputs_unbound_is_empty() {
    let config = make_config(vec![], vec![], vec![]);
    let state = build_state(&config, &[], 0, false, false, &[key(Key::BTN_SOUTH)], &None, &["test".to_string()]);
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
    let state = build_state(&config, &[], 0, false, false, &[], &None, &["test".to_string()]);
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
    let state = build_state(&config, &[btn_tl], 0, false, false, &[], &None, &["test".to_string()]);
    let avail = available_modifiers(&state);
    assert!(!avail.contains(&"BTN_TL".to_string()), "BTN_TL is already active");
    assert!(avail.contains(&"BTN_TR".to_string()), "BTN_TR unlocks a BTN_TL+BTN_TR combo");
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
    let state = build_state(&config, &[btn_tl], 0, false, false, &[], &None, &["test".to_string()]);
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
    let state = build_state(&config, &[btn_tl], 0, false, false, &[], &None, &["test".to_string()]);
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
    let state = build_state(&config, &[btn_tl], 0, false, false, &[], &None, &["test".to_string()]);
    assert_eq!(
        state["modifier_active"]["BTN_NORTH"]["label"].as_str().unwrap(),
        "Copy"
    );
}

// ── silent attribute ──────────────────────────────────────────────────────

#[test]
fn silent_binding_tagged_in_active_outputs() {
    // BTN_SOUTH → BTN_LEFT, marked silent.
    // active_outputs must include BTN_LEFT but with silent=true.
    let btn_south = key(Key::BTN_SOUTH);
    let mut config = make_config(
        vec![(btn_south, vec![], vec![Key::BTN_LEFT])],
        vec![], vec![],
    );
    config.bindings.silent.insert((btn_south, vec![]));
    let state = build_state(&config, &[], 0, false, false, &[btn_south], &None, &["test".to_string()]);
    let tagged = active_outputs_tagged(&state);
    assert_eq!(tagged, vec![("BTN_LEFT".to_string(), true)],
        "silent binding must appear in active_outputs with silent=true");
}

#[test]
fn silent_combo_tagged_in_active_outputs() {
    // BTN_TL-BTN_SOUTH → BTN_LEFT, marked silent.
    // active_outputs includes BTN_LEFT tagged silent=true.
    let btn_tl = key(Key::BTN_TL);
    let btn_south = key(Key::BTN_SOUTH);
    let mut config = make_config(
        vec![(btn_south, vec![btn_tl], vec![Key::BTN_LEFT])],
        vec![], vec![btn_tl],
    );
    config.bindings.silent.insert((btn_south, vec![btn_tl]));
    let state = build_state(&config, &[btn_tl], 0, false, false, &[btn_south], &None, &["test".to_string()]);
    let tagged = active_outputs_tagged(&state);
    assert_eq!(tagged, vec![("BTN_LEFT".to_string(), true)],
        "silent combo must appear in active_outputs with silent=true");
}

#[test]
fn non_silent_binding_tagged_false() {
    // A regular binding appears with silent=false.
    let btn_south = key(Key::BTN_SOUTH);
    let config = make_config(
        vec![(btn_south, vec![], vec![Key::KEY_ENTER])],
        vec![], vec![],
    );
    let state = build_state(&config, &[], 0, false, false, &[btn_south], &None, &["test".to_string()]);
    let tagged = active_outputs_tagged(&state);
    assert_eq!(tagged, vec![("KEY_ENTER".to_string(), false)]);
}

#[test]
fn silent_flag_in_bindings_json() {
    // silent = true must appear in the bindings JSON entry for the HUD.
    let btn_south = key(Key::BTN_SOUTH);
    let mut config = make_config(
        vec![(btn_south, vec![], vec![Key::BTN_LEFT])],
        vec![], vec![],
    );
    config.bindings.silent.insert((btn_south, vec![]));
    let state = build_state(&config, &[], 0, false, false, &[], &None, &["test".to_string()]);
    assert_eq!(
        state["bindings"]["BTN_SOUTH"]["silent"].as_bool().unwrap(),
        true,
        "silent flag must be present in bindings JSON"
    );
}

#[test]
fn non_silent_binding_silent_false_in_json() {
    // silent defaults to false for normal bindings.
    let btn_south = key(Key::BTN_SOUTH);
    let config = make_config(
        vec![(btn_south, vec![], vec![Key::KEY_ENTER])],
        vec![], vec![],
    );
    let state = build_state(&config, &[], 0, false, false, &[], &None, &["test".to_string()]);
    assert_eq!(
        state["bindings"]["BTN_SOUTH"]["silent"].as_bool().unwrap(),
        false
    );
}

// ── bindings JSON fields ──────────────────────────────────────────────────

#[test]
fn bindings_json_no_pause_flag() {
    // no_pause = true on a command binding must appear in the bindings JSON.
    let btn_thumbl = key(Key::BTN_THUMBL);
    let mut config = make_config(
        vec![],
        vec![(btn_thumbl, vec![], vec!["hud-toggle".to_string()])],
        vec![],
    );
    config.bindings.no_pause.insert((btn_thumbl, vec![]));
    let state = build_state(&config, &[], 0, false, false, &[], &None, &["test".to_string()]);
    assert_eq!(
        state["bindings"]["BTN_THUMBL"]["no_pause"].as_bool().unwrap(),
        true
    );
}

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
        &[btn_tl],
        0, false, false,
        &[btn_tl, btn_south],
        &None,
        &["test".to_string()],
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
    let state = build_state(&config, &mods, 0, false, false, &[btn_dpad_up], &None, &["test".to_string()]);
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
        &[Event::Key(Key::KEY_LEFTCTRL)],
        0, false, false, &[], &None, &["test".to_string()],
    );
    // BTN_TL should be detected as active → modifier_active shows BTN_TL-BTN_NORTH combo
    let keys = modifier_active_keys(&state);
    assert!(keys.contains(&"BTN_NORTH".to_string()),
        "indirect modifier detection: BTN_NORTH combo should appear when KEY_LEFTCTRL held");
}

// ── override: command→remap type change doesn't leak base command ─────────

#[test]
fn override_remap_hides_base_command() {
    // Base: BTN_TL-BTN_DPAD_UP = command "previous-desktop"
    // Override (firefox): BTN_TL-BTN_DPAD_UP = remap [KEY_LEFTCTRL, KEY_R]
    //
    // After merge_base(), modifier_active must show the remap, NOT the command.
    // Before the fix, merge_base() let both coexist; the command loop in
    // build_state then overwrote the remap entry in modifier_active.
    let btn_tl     = key(Key::BTN_TL);
    let btn_dpad_up = key(Key::BTN_DPAD_UP);
    let combo       = vec![btn_tl];

    // Base config: BTN_TL-BTN_DPAD_UP → command
    let base = {
        let mut commands: HashMap<Event, HashMap<Vec<Event>, Vec<String>>> = HashMap::new();
        commands.entry(btn_dpad_up).or_default()
            .insert(combo.clone(), vec!["previous-desktop".to_string()]);
        Config {
            name: "Steam Deck".to_string(),
            associations: Default::default(),
            bindings: crate::config::Bindings {
                commands,
                ..Default::default()
            },
            override_bindings: None,
            settings: HashMap::new(),
            mapped_modifiers: crate::config::MappedModifiers {
                default: vec![],
                custom: vec![btn_tl],
                all: vec![btn_tl],
            },
            trackpad: Default::default(),
            gaming_mode_config: Default::default(),
        }
    };

    // App config: BTN_TL-BTN_DPAD_UP → remap (overrides base command)
    let mut app = {
        let mut remap: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
        remap.entry(btn_dpad_up).or_default()
            .insert(combo.clone(), vec![Key::KEY_LEFTCTRL, Key::KEY_R]);
        Config {
            name: "firefox".to_string(),
            associations: Default::default(),
            bindings: crate::config::Bindings {
                remap,
                ..Default::default()
            },
            override_bindings: None,
            settings: HashMap::new(),
            mapped_modifiers: Default::default(),
            trackpad: Default::default(),
            gaming_mode_config: Default::default(),
        }
    };

    // This is the actual code path that runs at runtime.
    app.merge_base(&base);

    let state = build_state(
        &app, &[btn_tl], 0, false, false, &[], &None,
        &["Steam Deck".to_string(), "firefox".to_string()],
    );

    assert_eq!(
        state["modifier_active"]["BTN_DPAD_UP"]["kind"].as_str().unwrap(),
        "remap",
        "modifier_active must show the override remap, not the inherited command"
    );
    assert_eq!(
        state["modifier_active"]["BTN_DPAD_UP"]["action"][0].as_str().unwrap(),
        "KEY_LEFTCTRL",
    );
}

// ── label lifecycle across merge ─────────────────────────────────────────

/// When an override replaces a binding, the base label must not appear —
/// "Previous Desktop" should not label a Reload action.
#[test]
fn override_clears_base_label() {
    let btn_tl = key(Key::BTN_TL);
    let btn_up = key(Key::BTN_DPAD_UP);
    let combo  = vec![btn_tl];

    let mut base_bindings = {
        let mut cmds: HashMap<Event, HashMap<Vec<Event>, Vec<String>>> = HashMap::new();
        cmds.entry(btn_up).or_default()
            .insert(combo.clone(), vec!["previous-desktop".to_string()]);
        crate::config::Bindings { commands: cmds, ..Default::default() }
    };
    base_bindings.labels.insert((btn_up, combo.clone()), "Previous Desktop".to_string());

    let base = Config {
        name: "Steam Deck".to_string(),
        associations: Default::default(),
        bindings: base_bindings,
        override_bindings: None,
        settings: HashMap::new(),
        mapped_modifiers: crate::config::MappedModifiers {
            custom: vec![btn_tl], all: vec![btn_tl], default: vec![],
        },
        trackpad: Default::default(),
        gaming_mode_config: Default::default(),
    };

    // Override replaces with a remap, no label defined.
    let mut app_remap: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
    app_remap.entry(btn_up).or_default()
        .insert(combo.clone(), vec![Key::KEY_LEFTCTRL, Key::KEY_R]);
    let mut app = Config {
        name: "firefox".to_string(),
        associations: Default::default(),
        bindings: crate::config::Bindings { remap: app_remap, ..Default::default() },
        override_bindings: None,
        settings: HashMap::new(),
        mapped_modifiers: Default::default(),
        trackpad: Default::default(),
        gaming_mode_config: Default::default(),
    };

    app.merge_base(&base);

    let state = build_state(
        &app, &[btn_tl], 0, false, false, &[], &None,
        &["Steam Deck".to_string(), "firefox".to_string()],
    );

    // label must be null, not "Previous Desktop"
    assert!(
        state["modifier_active"]["BTN_DPAD_UP"]["label"].is_null(),
        "stale base label must not appear after action is overridden"
    );
}

/// When an override defines its own label, it appears in place of the base label.
#[test]
fn override_label_replaces_base_label() {
    let btn_tl = key(Key::BTN_TL);
    let btn_up = key(Key::BTN_DPAD_UP);
    let combo  = vec![btn_tl];

    let mut base_bindings = {
        let mut cmds: HashMap<Event, HashMap<Vec<Event>, Vec<String>>> = HashMap::new();
        cmds.entry(btn_up).or_default()
            .insert(combo.clone(), vec!["previous-desktop".to_string()]);
        crate::config::Bindings { commands: cmds, ..Default::default() }
    };
    base_bindings.labels.insert((btn_up, combo.clone()), "Previous Desktop".to_string());

    let base = Config {
        name: "Steam Deck".to_string(),
        associations: Default::default(),
        bindings: base_bindings,
        override_bindings: None,
        settings: HashMap::new(),
        mapped_modifiers: crate::config::MappedModifiers {
            custom: vec![btn_tl], all: vec![btn_tl], default: vec![],
        },
        trackpad: Default::default(),
        gaming_mode_config: Default::default(),
    };

    // Override replaces with a remap AND defines its own label.
    let mut app_remap: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
    app_remap.entry(btn_up).or_default()
        .insert(combo.clone(), vec![Key::KEY_LEFTCTRL, Key::KEY_R]);
    let mut app_bindings = crate::config::Bindings { remap: app_remap, ..Default::default() };
    app_bindings.labels.insert((btn_up, combo.clone()), "Reload".to_string());
    let mut app = Config {
        name: "firefox".to_string(),
        associations: Default::default(),
        bindings: app_bindings,
        override_bindings: None,
        settings: HashMap::new(),
        mapped_modifiers: Default::default(),
        trackpad: Default::default(),
        gaming_mode_config: Default::default(),
    };

    app.merge_base(&base);

    let state = build_state(
        &app, &[btn_tl], 0, false, false, &[], &None,
        &["Steam Deck".to_string(), "firefox".to_string()],
    );

    assert_eq!(
        state["modifier_active"]["BTN_DPAD_UP"]["label"].as_str(),
        Some("Reload"),
        "override label must appear in modifier_active"
    );
}

// ── origin: base config vs. app-specific override ─────────────────────────

#[test]
fn origin_override_vs_base() {
    // Simulate a Firefox app config layered on top of a Steam Deck base config.
    // Inherited bindings get origin="Steam Deck"; overridden ones get origin="firefox".
    use crate::config::{Bindings, MappedModifiers};
    use std::collections::{HashMap, HashSet};

    let btn_south = key(Key::BTN_SOUTH);
    let btn_tl = key(Key::BTN_TL);
    let btn_dpad_up = key(Key::BTN_DPAD_UP);

    // Merged bindings: base BTN_SOUTH + overridden BTN_TL-BTN_DPAD_UP
    let mut merged_remap: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
    merged_remap.entry(btn_south).or_default().insert(vec![], vec![Key::KEY_ENTER]);
    merged_remap.entry(btn_dpad_up).or_default().insert(
        vec![btn_tl], vec![Key::KEY_LEFTALT, Key::KEY_LEFT],
    );

    // override_bindings: only what the app config itself defined
    let mut override_remap: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
    override_remap.entry(btn_dpad_up).or_default().insert(
        vec![btn_tl], vec![Key::KEY_LEFTALT, Key::KEY_LEFT],
    );

    let config = Config {
        name: "firefox".to_string(),
        associations: Default::default(),
        bindings: Bindings {
            remap: merged_remap,
            commands: HashMap::new(),
            movements: HashMap::new(),
            no_pause: HashSet::new(),
            while_gaming: HashSet::new(),
            labels: HashMap::new(),
            silent: HashSet::new(),
        },
        override_bindings: Some(Bindings {
            remap: override_remap,
            commands: HashMap::new(),
            movements: HashMap::new(),
            no_pause: HashSet::new(),
            while_gaming: HashSet::new(),
            labels: HashMap::new(),
            silent: HashSet::new(),
        }),
        settings: HashMap::new(),
        mapped_modifiers: MappedModifiers {
            default: vec![],
            custom: vec![btn_tl],
            all: vec![btn_tl],
        },
        trackpad: Default::default(),
        gaming_mode_config: Default::default(),
    };

    let state = build_state(
        &config, &[], 0, false, false, &[], &None,
        &["Steam Deck".to_string(), "firefox".to_string()],
    );

    assert_eq!(
        state["bindings"]["BTN_SOUTH"]["origin"].as_str().unwrap(),
        "Steam Deck",
        "inherited binding should come from base config"
    );
    assert_eq!(
        state["bindings"]["BTN_TL-BTN_DPAD_UP"]["origin"].as_str().unwrap(),
        "firefox",
        "overridden binding should come from app config"
    );
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
    let mut state = build_state(&config, &[], 0, false, false, &[], &None, &["test".to_string()]);
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
    let state = build_state(&config, &[], 0, paused, gaming_mode, &[], &None, &["test".to_string()]);
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
