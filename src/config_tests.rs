use super::*;
use evdev::Key;

fn key(k: Key) -> Event { Event::Key(k) }

fn make_remap_bindings(entries: Vec<(Event, Vec<Event>, Vec<Key>)>) -> Bindings {
    let mut remap: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
    for (trigger, combo, keys) in entries {
        remap.entry(trigger).or_default().insert(combo, keys);
    }
    Bindings { remap, ..Default::default() }
}

fn make_command_bindings(entries: Vec<(Event, Vec<Event>, Vec<String>)>) -> Bindings {
    let mut commands: HashMap<Event, HashMap<Vec<Event>, Vec<String>>> = HashMap::new();
    for (trigger, combo, cmds) in entries {
        commands.entry(trigger).or_default().insert(combo, cmds);
    }
    Bindings { commands, ..Default::default() }
}

fn config_with(name: &str, bindings: Bindings) -> Config {
    Config {
        name: name.to_string(),
        associations: Default::default(),
        bindings,
        override_bindings: None,
        settings: HashMap::new(),
        mapped_modifiers: Default::default(),
        trackpad: Default::default(),
        gaming_mode_config: Default::default(),
    }
}

// ── merge_base: cross-type override prevention ────────────────────────────

/// Firefox overrides BTN_TL-BTN_DPAD_UP from command (base) → remap (override).
/// After merge, the base command must NOT appear — only the override remap.
#[test]
fn merge_base_command_to_remap_override() {
    let btn_tl = key(Key::BTN_TL);
    let btn_up = key(Key::BTN_DPAD_UP);
    let combo  = vec![btn_tl];

    let base = config_with("Steam Deck", make_command_bindings(vec![
        (btn_up, combo.clone(), vec!["previous-desktop".to_string()]),
    ]));
    let mut app = config_with("firefox", make_remap_bindings(vec![
        (btn_up, combo.clone(), vec![Key::KEY_LEFTCTRL, Key::KEY_R]),
    ]));

    app.merge_base(&base);

    assert!(
        app.bindings.remap.get(&btn_up).and_then(|m| m.get(&combo)).is_some(),
        "override remap must survive merge"
    );
    assert!(
        app.bindings.commands.get(&btn_up).and_then(|m| m.get(&combo)).is_none(),
        "base command must not leak when override defines a remap for the same combo"
    );
}

/// Symmetric: override defines a command where base had a remap.
#[test]
fn merge_base_remap_to_command_override() {
    let btn_tl = key(Key::BTN_TL);
    let btn_up = key(Key::BTN_DPAD_UP);
    let combo  = vec![btn_tl];

    let base = config_with("Steam Deck", make_remap_bindings(vec![
        (btn_up, combo.clone(), vec![Key::KEY_UP]),
    ]));
    let mut app = config_with("myapp", make_command_bindings(vec![
        (btn_up, combo.clone(), vec!["do-something".to_string()]),
    ]));

    app.merge_base(&base);

    assert!(
        app.bindings.commands.get(&btn_up).and_then(|m| m.get(&combo)).is_some(),
        "override command must survive merge"
    );
    assert!(
        app.bindings.remap.get(&btn_up).and_then(|m| m.get(&combo)).is_none(),
        "base remap must not leak when override defines a command for the same combo"
    );
}

/// parse_event_name returns Some for valid names and None for unknown ones.
#[test]
fn parse_event_name_known_and_unknown() {
    assert!(parse_event_name("BTN_TL").is_some(), "BTN_TL must be recognised");
    assert!(parse_event_name("BTN_GRIPR2").is_some(), "BTN_GRIPR2 (Steam Deck back paddle) must be recognised");
    assert!(parse_event_name("BTN_TOTALLY_FAKE").is_none(), "unknown name must return None");
    assert!(parse_event_name("").is_none(), "empty string must return None");
}

/// Unrelated combos from the base must still be inherited normally.
#[test]
fn merge_base_unrelated_combos_inherited() {
    let btn_tl   = key(Key::BTN_TL);
    let btn_up   = key(Key::BTN_DPAD_UP);
    let btn_down = key(Key::BTN_DPAD_DOWN);

    let base = config_with("Steam Deck", make_command_bindings(vec![
        (btn_up,   vec![btn_tl], vec!["prev-desktop".to_string()]),
        (btn_down, vec![btn_tl], vec!["next-desktop".to_string()]),
    ]));
    // Override only changes btn_up; btn_down must be inherited unchanged.
    let mut app = config_with("firefox", make_remap_bindings(vec![
        (btn_up, vec![btn_tl], vec![Key::KEY_LEFTCTRL, Key::KEY_R]),
    ]));

    app.merge_base(&base);

    assert!(
        app.bindings.commands
            .get(&btn_down).and_then(|m| m.get(&vec![btn_tl])).is_some(),
        "unrelated base command (btn_down) must be inherited"
    );
}

// ── while_gaming flag parsing ─────────────────────────────────────────────
//
// TOML can only be deserialized from a complete key=value document, so we
// use a thin `#[derive(Deserialize)]` wrapper around each value type.

#[derive(serde::Deserialize)]
struct RemapWrapper { val: RemapValue }
#[derive(serde::Deserialize)]
struct CommandWrapper { val: CommandValue }

/// `RemapValue::WithAttrs` with `while_gaming = true` must deserialise
/// correctly from TOML inline-table syntax.
#[test]
fn remap_with_attrs_while_gaming_deserializes() {
    let w: RemapWrapper = toml::from_str(
        r#"val = { keys = ["KEY_A"], while_gaming = true }"#
    ).expect("should parse RemapValue with while_gaming");
    match w.val {
        RemapValue::WithAttrs { while_gaming, .. } => {
            assert!(while_gaming, "while_gaming flag must be true");
        }
        RemapValue::Simple(_) => panic!("expected WithAttrs, got Simple"),
    }
}

/// `RemapValue::WithAttrs` without `while_gaming` must default to false.
#[test]
fn remap_with_attrs_while_gaming_defaults_false() {
    let w: RemapWrapper = toml::from_str(
        r#"val = { keys = ["KEY_A"] }"#
    ).expect("should parse RemapValue without while_gaming");
    match w.val {
        RemapValue::WithAttrs { while_gaming, .. } => {
            assert!(!while_gaming, "omitted while_gaming must default to false");
        }
        RemapValue::Simple(_) => panic!("expected WithAttrs, got Simple"),
    }
}

/// `CommandValue::WithAttrs` with `while_gaming = true` must deserialise
/// correctly from TOML inline-table syntax.
#[test]
fn command_with_attrs_while_gaming_deserializes() {
    let w: CommandWrapper = toml::from_str(
        r#"val = { run = ["echo hi"], while_gaming = true }"#
    ).expect("should parse CommandValue with while_gaming");
    match w.val {
        CommandValue::WithAttrs { while_gaming, .. } => {
            assert!(while_gaming, "while_gaming flag must be true");
        }
        CommandValue::Simple(_) => panic!("expected WithAttrs, got Simple"),
    }
}

/// `CommandValue::WithAttrs` without `while_gaming` must default to false.
#[test]
fn command_with_attrs_while_gaming_defaults_false() {
    let w: CommandWrapper = toml::from_str(
        r#"val = { run = ["echo hi"] }"#
    ).expect("should parse CommandValue without while_gaming");
    match w.val {
        CommandValue::WithAttrs { while_gaming, .. } => {
            assert!(!while_gaming, "omitted while_gaming must default to false");
        }
        CommandValue::Simple(_) => panic!("expected WithAttrs, got Simple"),
    }
}

/// After `parse_raw_config`, a remap with `while_gaming = true` must appear
/// in `bindings.while_gaming`, while a plain remap must not.
#[test]
fn parse_raw_config_populates_while_gaming_for_remap() {
    let mut remap: HashMap<String, RemapValue> = HashMap::new();
    // BTN_SOUTH → KEY_A with while_gaming = true
    remap.insert(
        "BTN_SOUTH".to_string(),
        RemapValue::WithAttrs {
            keys: vec![Key::KEY_A],
            no_pause: false,
            while_gaming: true,
            label: None,
            silent: false,
        },
    );
    // BTN_EAST → KEY_B without while_gaming
    remap.insert(
        "BTN_EAST".to_string(),
        RemapValue::WithAttrs {
            keys: vec![Key::KEY_B],
            no_pause: false,
            while_gaming: false,
            label: None,
            silent: false,
        },
    );
    let raw = RawConfig {
        remap,
        commands: HashMap::new(),
        movements: HashMap::new(),
        settings: HashMap::new(),
        trackpad: RawTrackpadConfig::default(),
        gaming_mode: None,
    };
    let (bindings, _, _) = parse_raw_config(raw);
    let btn_south = Event::Key(Key::BTN_SOUTH);
    let btn_east  = Event::Key(Key::BTN_EAST);
    assert!(
        bindings.while_gaming.contains(&(btn_south, vec![])),
        "BTN_SOUTH with while_gaming=true must be in bindings.while_gaming"
    );
    assert!(
        !bindings.while_gaming.contains(&(btn_east, vec![])),
        "BTN_EAST without while_gaming must NOT be in bindings.while_gaming"
    );
}

/// After `parse_raw_config`, a command with `while_gaming = true` must also
/// appear in `bindings.while_gaming`.
#[test]
fn parse_raw_config_populates_while_gaming_for_command() {
    let mut commands: HashMap<String, CommandValue> = HashMap::new();
    commands.insert(
        "BTN_MODE".to_string(),
        CommandValue::WithAttrs {
            run: vec!["echo gaming".to_string()],
            no_pause: false,
            while_gaming: true,
            label: None,
            silent: false,
        },
    );
    let raw = RawConfig {
        remap: HashMap::new(),
        commands,
        movements: HashMap::new(),
        settings: HashMap::new(),
        trackpad: RawTrackpadConfig::default(),
        gaming_mode: None,
    };
    let (bindings, _, _) = parse_raw_config(raw);
    let btn_mode = Event::Key(Key::BTN_MODE);
    assert!(
        bindings.while_gaming.contains(&(btn_mode, vec![])),
        "BTN_MODE command with while_gaming=true must be in bindings.while_gaming"
    );
}

/// `while_gaming` entries survive a `merge_base`: entries from the override
/// config are kept, and base entries for unrelated triggers are also kept.
#[test]
fn merge_base_while_gaming_entries_survive() {
    let btn_south = Event::Key(Key::BTN_SOUTH);
    let btn_east  = Event::Key(Key::BTN_EAST);

    let mut base_bindings = Bindings::default();
    base_bindings.while_gaming.insert((btn_east, vec![]));
    base_bindings.remap.entry(btn_east).or_default()
        .insert(vec![], vec![Key::KEY_B]);

    let mut app_bindings = Bindings::default();
    app_bindings.while_gaming.insert((btn_south, vec![]));
    app_bindings.remap.entry(btn_south).or_default()
        .insert(vec![], vec![Key::KEY_A]);

    let base = config_with("base", base_bindings);
    let mut app = config_with("app", app_bindings);
    app.merge_base(&base);

    assert!(
        app.bindings.while_gaming.contains(&(btn_south, vec![])),
        "app's own while_gaming entry must survive merge"
    );
    assert!(
        app.bindings.while_gaming.contains(&(btn_east, vec![])),
        "base's while_gaming entry must be inherited through merge"
    );
}
