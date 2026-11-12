//! The bindings table: its fields, labels and notes, and which config each
//! binding came from.

use super::*;

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
    let state = build_state(&config, &[], false, false, &[btn_south], &None, &["test".to_string()], &GamingModeConfig::default());
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
    let state = build_state(&config, &[btn_tl], false, false, &[btn_south], &None, &["test".to_string()], &GamingModeConfig::default());
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
    let state = build_state(&config, &[], false, false, &[btn_south], &None, &["test".to_string()], &GamingModeConfig::default());
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
    let state = build_state(&config, &[], false, false, &[], &None, &["test".to_string()], &GamingModeConfig::default());
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
    let state = build_state(&config, &[], false, false, &[], &None, &["test".to_string()], &GamingModeConfig::default());
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
    let state = build_state(&config, &[], false, false, &[], &None, &["test".to_string()], &GamingModeConfig::default());
    assert_eq!(
        state["bindings"]["BTN_THUMBL"]["no_pause"].as_bool().unwrap(),
        true
    );
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
        device: None,
        module: Default::default(),
        aliases: Default::default(),
        }
    };

    // App config: BTN_TL-BTN_DPAD_UP → remap (overrides base command)
    let mut app = {
        let mut remap: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
        remap.entry(btn_dpad_up).or_default()
            .insert(combo.clone(), vec![Key::KEY_LEFTCTRL, Key::KEY_R]);
        Config {
            name: "firefox".to_string(),
            bindings: crate::config::Bindings {
                remap,
                ..Default::default()
            },
            override_bindings: None,
            settings: HashMap::new(),
            mapped_modifiers: Default::default(),
            trackpad: Default::default(),
            gaming_mode_config: Default::default(),
        device: None,
        module: Default::default(),
        aliases: Default::default(),
        }
    };

    // This is the actual code path that runs at runtime.
    app.merge_base(&base);

    let state = build_state(
        &app, &[btn_tl], false, false, &[], &None,
        &["Steam Deck".to_string(), "firefox".to_string()],
        &GamingModeConfig::default(),
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
        bindings: base_bindings,
        override_bindings: None,
        settings: HashMap::new(),
        mapped_modifiers: crate::config::MappedModifiers {
            custom: vec![btn_tl], all: vec![btn_tl], default: vec![],
        },
        trackpad: Default::default(),
        gaming_mode_config: Default::default(),
        device: None,
        module: Default::default(),
        aliases: Default::default(),
    };

    // Override replaces with a remap, no label defined.
    let mut app_remap: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
    app_remap.entry(btn_up).or_default()
        .insert(combo.clone(), vec![Key::KEY_LEFTCTRL, Key::KEY_R]);
    let mut app = Config {
        name: "firefox".to_string(),
        bindings: crate::config::Bindings { remap: app_remap, ..Default::default() },
        override_bindings: None,
        settings: HashMap::new(),
        mapped_modifiers: Default::default(),
        trackpad: Default::default(),
        gaming_mode_config: Default::default(),
        device: None,
        module: Default::default(),
        aliases: Default::default(),
    };

    app.merge_base(&base);

    let state = build_state(
        &app, &[btn_tl], false, false, &[], &None,
        &["Steam Deck".to_string(), "firefox".to_string()],
        &GamingModeConfig::default(),
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
        bindings: base_bindings,
        override_bindings: None,
        settings: HashMap::new(),
        mapped_modifiers: crate::config::MappedModifiers {
            custom: vec![btn_tl], all: vec![btn_tl], default: vec![],
        },
        trackpad: Default::default(),
        gaming_mode_config: Default::default(),
        device: None,
        module: Default::default(),
        aliases: Default::default(),
    };

    // Override replaces with a remap AND defines its own label.
    let mut app_remap: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
    app_remap.entry(btn_up).or_default()
        .insert(combo.clone(), vec![Key::KEY_LEFTCTRL, Key::KEY_R]);
    let mut app_bindings = crate::config::Bindings { remap: app_remap, ..Default::default() };
    app_bindings.labels.insert((btn_up, combo.clone()), "Reload".to_string());
    let mut app = Config {
        name: "firefox".to_string(),
        bindings: app_bindings,
        override_bindings: None,
        settings: HashMap::new(),
        mapped_modifiers: Default::default(),
        trackpad: Default::default(),
        gaming_mode_config: Default::default(),
        device: None,
        module: Default::default(),
        aliases: Default::default(),
    };

    app.merge_base(&base);

    let state = build_state(
        &app, &[btn_tl], false, false, &[], &None,
        &["Steam Deck".to_string(), "firefox".to_string()],
        &GamingModeConfig::default(),
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
        bindings: Bindings {
            remap: merged_remap,
            commands: HashMap::new(),
            movements: HashMap::new(),
            no_pause: HashSet::new(),
            while_gaming: HashSet::new(),
            labels: HashMap::new(),
            silent: HashSet::new(),
            ..Default::default()
        },
        override_bindings: Some(Bindings {
            remap: override_remap,
            commands: HashMap::new(),
            movements: HashMap::new(),
            no_pause: HashSet::new(),
            while_gaming: HashSet::new(),
            labels: HashMap::new(),
            silent: HashSet::new(),
            ..Default::default()
        }),
        settings: HashMap::new(),
        mapped_modifiers: MappedModifiers {
            default: vec![],
            custom: vec![btn_tl],
            all: vec![btn_tl],
        },
        trackpad: Default::default(),
        gaming_mode_config: Default::default(),
        device: None,
        module: Default::default(),
        aliases: Default::default(),
    };

    let state = build_state(
        &config, &[], false, false, &[], &None,
        &["Steam Deck".to_string(), "firefox".to_string()],
        &GamingModeConfig::default(),
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

// ── Binding notes ─────────────────────────────────────────────────────────

/// A `note` reaches the HUD next to the label, both for the plain binding
/// and, once its modifier is held, in modifier_active. Bindings without one
/// carry no `note` key at all.
#[test]
fn binding_note_is_exported_in_bindings_and_modifier_active() {
    let l1 = key(Key::BTN_TL);
    let r5 = key(Key::BTN_GRIPR2);
    let a = key(Key::BTN_SOUTH);
    let mut config = make_config(
        vec![
            (r5, vec![l1], vec![Key::KEY_LEFTCTRL, Key::KEY_PAGEDOWN]),
            (a, vec![], vec![Key::KEY_ENTER]),
        ],
        vec![(key(Key::BTN_NORTH), vec![l1], vec!["true".to_string()])],
        vec![l1],
    );
    config.bindings.notes.insert((r5, vec![l1]), "halten".to_string());
    config.bindings.notes.insert((key(Key::BTN_NORTH), vec![l1]), "halten".to_string());

    let idle = build_state(&config, &[], false, false, &[], &None, &["test".to_string()], &GamingModeConfig::default());
    assert_eq!(idle["bindings"]["BTN_TL-BTN_GRIPR2"]["note"], "halten");
    assert_eq!(idle["bindings"]["BTN_TL-BTN_NORTH"]["note"], "halten");
    assert!(idle["bindings"]["BTN_SOUTH"].get("note").is_none());

    let held = build_state(&config, &[l1], false, false, &[l1], &None, &["test".to_string()], &GamingModeConfig::default());
    assert_eq!(held["modifier_active"]["BTN_GRIPR2"]["note"], "halten");
    assert_eq!(held["modifier_active"]["BTN_NORTH"]["note"], "halten");
}
