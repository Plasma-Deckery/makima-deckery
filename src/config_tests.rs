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
        bindings,
        override_bindings: None,
        settings: HashMap::new(),
        mapped_modifiers: Default::default(),
        trackpad: Default::default(),
        gaming_mode_config: Default::default(),
        device: None,
        module: Default::default(),
        module_includes: Vec::new(),
        aliases: HashMap::new(),
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
        hints: HashMap::new(),
        settings: HashMap::new(),
        trackpad: RawTrackpadConfig::default(),
        gaming_mode: None,
        device: None,
        module: Default::default(),
        modules: Default::default(),
    };
    let (bindings, _, _) = parse_raw_config(raw, &HashMap::new());
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
        hints: HashMap::new(),
        settings: HashMap::new(),
        trackpad: RawTrackpadConfig::default(),
        gaming_mode: None,
        device: None,
        module: Default::default(),
        modules: Default::default(),
    };
    let (bindings, _, _) = parse_raw_config(raw, &HashMap::new());
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

#[test]
fn empty_modifier_list_parses_to_nothing() {
    // `CUSTOM_MODIFIERS = ""` is how a config states "I declare none" while
    // still documenting that the setting exists. It must not be read as one
    // unnamed modifier.
    let mut settings = HashMap::new();
    settings.insert("CUSTOM_MODIFIERS".to_string(), String::new());
    assert!(parse_modifiers(&settings, "CUSTOM_MODIFIERS", &HashMap::new()).is_empty());
}

#[test]
fn modifier_list_ignores_stray_separators() {
    let mut settings = HashMap::new();
    settings.insert("CUSTOM_MODIFIERS".to_string(), "BTN_TL--BTN_TR-".to_string());
    assert_eq!(
        parse_modifiers(&settings, "CUSTOM_MODIFIERS", &HashMap::new()),
        vec![key(Key::BTN_TL), key(Key::BTN_TR)]
    );
}

// ── Device aliases ────────────────────────────────────────────────────────

fn deck_aliases() -> HashMap<String, String> {
    [("L1", "BTN_TL"), ("A", "BTN_SOUTH"), ("X", "BTN_NORTH")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn raw_with_remap(remap: HashMap<String, RemapValue>) -> RawConfig {
    RawConfig {
        remap,
        commands: HashMap::new(),
        movements: HashMap::new(),
        hints: HashMap::new(),
        settings: HashMap::new(),
        trackpad: RawTrackpadConfig::default(),
        gaming_mode: None,
        device: None,
        module: Default::default(),
        modules: Default::default(),
    }
}

fn simple_remap(input: &str) -> HashMap<String, RemapValue> {
    let mut remap = HashMap::new();
    remap.insert(input.to_string(), RemapValue::Simple(vec![Key::KEY_C]));
    remap
}

/// `L1-A` must land on exactly the same trigger and modifier as `BTN_TL-BTN_SOUTH`.
/// Aliases exist because evdev rejects `BTN_A`/`BTN_X` outright, so the kernel
/// names are the only spellings the parser knows without them.
#[test]
fn aliases_resolve_in_trigger_and_modifier() {
    let (bindings, _, mods) =
        parse_raw_config(raw_with_remap(simple_remap("L1-A")), &deck_aliases());

    let combos = bindings.remap.get(&key(Key::BTN_SOUTH))
        .expect("alias A must resolve to BTN_SOUTH as the trigger");
    assert!(
        combos.contains_key(&vec![key(Key::BTN_TL)]),
        "alias L1 must resolve to BTN_TL as the modifier"
    );
    assert!(mods.custom.contains(&key(Key::BTN_TL)));
}

/// Kernel names keep working next to aliases — resolution falls through for
/// anything the alias table does not name.
#[test]
fn unaliased_names_pass_through() {
    let (bindings, _, _) =
        parse_raw_config(raw_with_remap(simple_remap("BTN_TL-BTN_WEST")), &deck_aliases());

    assert!(
        bindings.remap.get(&key(Key::BTN_WEST))
            .is_some_and(|c| c.contains_key(&vec![key(Key::BTN_TL)])),
        "kernel names must still parse when an alias table is in effect"
    );
}

/// The alias table is read from the base config's `[device]` block before any
/// file is fully parsed; without that pre-pass a module could not use aliases.
#[test]
fn read_aliases_picks_up_device_table() {
    let path = std::env::temp_dir().join("deckery_alias_probe.toml");
    std::fs::write(&path, r#"
[device]
class = "hid-steam"
names = ["Steam Deck"]
aliases = { L1 = "BTN_TL", A = "BTN_SOUTH" }

[remap]
L1-A = ["KEY_C"]
"#).unwrap();

    let aliases = Config::read_aliases(path.to_str().unwrap());
    assert_eq!(aliases.get("L1").map(String::as_str), Some("BTN_TL"));
    assert_eq!(aliases.get("A").map(String::as_str), Some("BTN_SOUTH"));

    let _ = std::fs::remove_file(&path);
}

/// A combo whose modifier does not resolve must be dropped whole. Keeping the
/// trigger would turn `L1-A` into a bare `A` binding — not a missing binding
/// but a wrong one, silently hijacking the unmodified button.
#[test]
fn combo_with_unresolvable_modifier_is_dropped_entirely() {
    let broken: HashMap<String, String> =
        [("L1".to_string(), "BTN_TYPO".to_string())].into_iter().collect();

    let (bindings, _, _) =
        parse_raw_config(raw_with_remap(simple_remap("L1-BTN_SOUTH")), &broken);

    assert!(
        bindings.remap.is_empty(),
        "binding must be dropped, not demoted to an unmodified BTN_SOUTH: {:?}",
        bindings.remap
    );
}

/// An unknown trigger with a valid modifier is dropped as before.
#[test]
fn combo_with_unresolvable_trigger_is_dropped() {
    let (bindings, _, _) =
        parse_raw_config(raw_with_remap(simple_remap("L1-NOPE")), &deck_aliases());
    assert!(bindings.remap.is_empty());
}

/// `event_from_name` answers the same question as `parse_event_name` without
/// logging — the alias table is validated with it before any binding is read.
#[test]
fn event_from_name_is_silent_and_agrees_with_parse_event_name() {
    assert_eq!(event_from_name("BTN_TL"), Some(key(Key::BTN_TL)));
    assert!(event_from_name("BTN_TYPO").is_none());
    assert!(event_from_name("").is_none());
}

/// The gaming_mode trigger resolves aliases too. It parses through `Key::from_str`
/// rather than the binding path, and an unknown name there only warns — an alias
/// that worked everywhere else would silently disable Gaming Mode.
#[test]
fn aliases_resolve_in_gaming_mode_trigger() {
    let aliases: HashMap<String, String> =
        [("QAM".to_string(), "BTN_BASE".to_string())].into_iter().collect();
    let raw = RawGamingModeConfig {
        trigger: Some(RawDoubleclickTrigger { key: "QAM".to_string(), ms: Some(400) }),
        ..Default::default()
    };

    let parsed = GamingModeConfig::from_raw(Some(raw), &aliases);
    assert_eq!(parsed.trigger.map(|t| t.key), Some(Key::BTN_BASE));
}

/// A file with no `[device]` block contributes no aliases rather than failing.
#[test]
fn read_aliases_tolerates_module_files() {
    let path = std::env::temp_dir().join("deckery_alias_module_probe.toml");
    std::fs::write(&path, "[module]\nrequires_compositor = \"KDE\"\n").unwrap();

    assert!(Config::read_aliases(path.to_str().unwrap()).is_empty());

    let _ = std::fs::remove_file(&path);
}


// ── Hints ─────────────────────────────────────────────────────────────────
//
// Hints are display-only labels for button combinations that are not bindings.
// See docs/hints.md. The invariants below are the ones a reviewer should check.

/// Build a config whose base remaps mirror the live Steam Deck layout closely
/// enough for hint resolution: R5 emits Ctrl, R4 emits Alt, DPad emits arrows.
fn config_with_hints(hints: Vec<(&str, &str)>) -> Config {
    let mut bindings = make_remap_bindings(vec![
        (key(Key::BTN_GRIPR2), vec![], vec![Key::KEY_LEFTCTRL]),
        (key(Key::BTN_GRIPR),  vec![], vec![Key::KEY_LEFTALT]),
        (key(Key::BTN_GRIPL),  vec![], vec![Key::KEY_LEFTSHIFT]),
        (key(Key::BTN_DPAD_UP),    vec![], vec![Key::KEY_UP]),
        (key(Key::BTN_DPAD_DOWN),  vec![], vec![Key::KEY_DOWN]),
        (key(Key::BTN_DPAD_LEFT),  vec![], vec![Key::KEY_LEFT]),
    ]);
    bindings.hints = hints.into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    config_with("Steam Deck", bindings)
}

/// The label a hint resolved to, or None. Keeps assertions about *what is shown*
/// free of the record that also carries where the line was written.
fn hint_label(config: &Config, trigger: Event, combo: Vec<Event>) -> Option<&str> {
    config.bindings.hints_resolved.get(&(trigger, combo)).map(|h| h.label.as_str())
}

/// The core case: a hint written in output space lands on the buttons that
/// actually emit those keys. KEY_LEFTCTRL → R5, KEY_UP → DPad Up.
#[test]
fn hint_resolves_output_keys_to_the_buttons_that_emit_them() {
    let mut config = config_with_hints(vec![("KEY_LEFTCTRL-KEY_UP", "Jump to Top")]);
    let warnings = config.resolve_hints();

    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert_eq!(
        hint_label(&config, key(Key::BTN_DPAD_UP), vec![key(Key::BTN_GRIPR2)]),
        Some("Jump to Top"),
    );
}

/// Hints are output space only. Naming a button — as a kernel name or through a
/// device alias — asserts a mapping instead of describing a shortcut, for a
/// combination no application can observe. Both spellings are refused.
#[test]
fn hint_naming_a_button_is_refused() {
    for spelling in ["BTN_DPAD_UP", "Up", "KEY_LEFTCTRL-BTN_DPAD_UP"] {
        let mut config = config_with_hints(vec![(spelling, "Jump to Top")]);
        let warnings = config.resolve_hints();

        assert!(config.bindings.hints_resolved.is_empty(), "{spelling} was accepted");
        assert!(
            warnings.iter().any(|w| w.contains("output space")),
            "{spelling} must say why, got: {warnings:?}",
        );
    }
}

/// Modifiers must match exactly, like real combos: Ctrl-Left must not survive
/// a second modifier being held. Verified here on the resolved shape — the
/// runtime comparison lives in state_export and uses the same length rule.
#[test]
fn hint_with_two_modifiers_resolves_to_both_buttons() {
    let mut config = config_with_hints(vec![
        ("KEY_LEFTCTRL-KEY_LEFTSHIFT-KEY_LEFT", "Select Word Left"),
    ]);
    config.resolve_hints();

    let mut combo = vec![key(Key::BTN_GRIPR2), key(Key::BTN_GRIPL)];
    combo.sort();
    assert_eq!(
        hint_label(&config, key(Key::BTN_DPAD_LEFT), combo),
        Some("Select Word Left"),
    );
    assert_eq!(config.bindings.hints_resolved.len(), 1);
}

/// Two buttons emitting the same key is real — R2 and R3 both send BTN_LEFT in
/// the live base config. The hint appears on both, and the ambiguity is reported.
#[test]
fn hint_on_ambiguous_key_lands_on_every_emitter_and_warns() {
    let mut bindings = make_remap_bindings(vec![
        (key(Key::BTN_GRIPR2), vec![], vec![Key::KEY_LEFTCTRL]),
        (key(Key::BTN_TR2),    vec![], vec![Key::KEY_ENTER]),
        (key(Key::BTN_SOUTH),  vec![], vec![Key::KEY_ENTER]),
    ]);
    bindings.hints = [("KEY_LEFTCTRL-KEY_ENTER".to_string(), "Send".to_string())]
        .into_iter().collect();
    let mut config = config_with("Steam Deck", bindings);

    let warnings = config.resolve_hints();

    assert_eq!(config.bindings.hints_resolved.len(), 2);
    assert!(warnings.iter().any(|w| w.contains("2 buttons")), "got: {warnings:?}");
}

/// The mirror case of the one above: two *lines* reaching one button, because
/// that button sends both keys. Only one label fits, so the loser is dropped —
/// but it must be reported, and the winner must be the same on every run rather
/// than whatever the HashMap happens to yield first.
#[test]
fn two_hints_on_the_same_button_keep_one_and_warn() {
    let mut bindings = make_remap_bindings(vec![
        (key(Key::BTN_NORTH), vec![], vec![Key::KEY_SPACE, Key::KEY_X]),
    ]);
    bindings.hints = [
        ("KEY_SPACE".to_string(), "Play/Pause".to_string()),
        ("KEY_X".to_string(),     "Close".to_string()),
    ].into_iter().collect();
    let mut config = config_with("Steam Deck", bindings);

    let warnings = config.resolve_hints();

    assert_eq!(config.bindings.hints_resolved.len(), 1);
    assert_eq!(
        hint_label(&config, key(Key::BTN_NORTH), vec![]),
        Some("Play/Pause"),
        "the lexicographically first raw key must win, every time",
    );
    assert!(
        warnings.iter().any(|w| w.contains("same button")),
        "collision must be reported, got: {warnings:?}",
    );
}

/// A hint no button can satisfy is reported instead of vanishing. KEY_C is only
/// ever produced by a combo, so Ctrl+Shift+C genuinely cannot be displayed.
#[test]
fn hint_without_an_emitting_button_warns_and_is_dropped() {
    let mut config = config_with_hints(vec![("KEY_LEFTCTRL-KEY_C", "Copy Last Response")]);
    let warnings = config.resolve_hints();

    assert!(config.bindings.hints_resolved.is_empty());
    assert!(warnings.iter().any(|w| w.contains("can never be shown")), "got: {warnings:?}");
}

/// Invariant 4: a real binding on the same (trigger, combo) suppresses the hint.
/// A typo in [hints] must never mask something that actually fires.
#[test]
fn real_binding_wins_over_hint_on_the_same_combo() {
    let mut config = config_with_hints(vec![("KEY_LEFTCTRL-KEY_UP", "Jump to Top")]);
    // A real combo already occupies R5 + DPad Up.
    config.bindings.remap
        .entry(key(Key::BTN_DPAD_UP)).or_default()
        .insert(vec![key(Key::BTN_GRIPR2)], vec![Key::KEY_PAGEUP]);

    config.resolve_hints();

    assert!(config.bindings.hints_resolved.is_empty());
}

/// Invariant 1: nothing from [hints] may enter MappedModifiers. This is the
/// whole reason hints exist — a registered modifier would hit the Custom
/// Modifier Intercept in event_reader and break L1-R5.
#[test]
fn hints_never_register_a_modifier() {
    let raw = RawConfig {
        remap: [("BTN_GRIPR2".to_string(), RemapValue::Simple(vec![Key::KEY_LEFTCTRL]))]
            .into_iter().collect(),
        hints: [("KEY_LEFTCTRL-KEY_UP".to_string(), "Jump to Top".to_string())]
            .into_iter().collect(),
        ..raw_with_remap(HashMap::new())
    };

    let (_, _, mapped) = parse_raw_config(raw, &HashMap::new());

    assert!(mapped.custom.is_empty(), "hints leaked into custom modifiers: {:?}", mapped.custom);
    assert!(!mapped.all.contains(&key(Key::BTN_GRIPR2)));
}

/// An app override wins over a base hint for the same key, exactly like labels.
#[test]
fn app_hint_overrides_base_hint() {
    let mut base = config_with_hints(vec![("KEY_LEFTCTRL-KEY_UP", "Jump to Top")]);
    base.resolve_hints();

    let mut app = config_with("Claude Desktop", Bindings {
        hints: [("KEY_LEFTCTRL-KEY_UP".to_string(), "Scroll to First Message".to_string())]
            .into_iter().collect(),
        ..Default::default()
    });
    app.merge_base(&base);
    app.resolve_hints();

    assert_eq!(
        hint_label(&app, key(Key::BTN_DPAD_UP), vec![key(Key::BTN_GRIPR2)]),
        Some("Scroll to First Message"),
    );
}

/// Resolution must follow the merged remap table, not the base's. If an app
/// moves Ctrl from R5 to R4, the hint has to move with it.
#[test]
fn hint_follows_a_remapped_modifier_after_merge() {
    let mut base = config_with_hints(vec![("KEY_LEFTCTRL-KEY_UP", "Jump to Top")]);
    base.resolve_hints();

    let mut app = config_with("Remapper", make_remap_bindings(vec![
        (key(Key::BTN_GRIPR2), vec![], vec![Key::KEY_LEFTMETA]),
        (key(Key::BTN_GRIPR),  vec![], vec![Key::KEY_LEFTCTRL]),
    ]));
    app.merge_base(&base);
    app.resolve_hints();

    assert_eq!(
        hint_label(&app, key(Key::BTN_DPAD_UP), vec![key(Key::BTN_GRIPR)]),
        Some("Jump to Top"),
        "hint did not follow Ctrl onto R4",
    );
}

/// Resolution is idempotent — resolve() runs on every button press.
#[test]
fn resolve_hints_is_idempotent() {
    let mut config = config_with_hints(vec![("KEY_LEFTCTRL-KEY_UP", "Jump to Top")]);
    config.resolve_hints();
    let first = config.bindings.hints_resolved.clone();
    config.resolve_hints();

    assert_eq!(first, config.bindings.hints_resolved);
}

/// A hint with no modifier is the second legal form: it relabels a plain
/// button. It resolves to an empty combo, which is what routes it into
/// `bindings` rather than `modifier_active`.
#[test]
fn hint_without_a_modifier_relabels_a_plain_button() {
    let mut config = config_with_hints(vec![("KEY_UP", "Scroll Up")]);
    let warnings = config.resolve_hints();

    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert_eq!(
        hint_label(&config, key(Key::BTN_DPAD_UP), vec![]),
        Some("Scroll Up"),
    );
}

/// The base remap the hint resolved *through* must not count as a competing
/// binding — otherwise the modifier-less form could never apply to anything.
#[test]
fn modifier_less_hint_survives_the_base_remap_it_resolved_through() {
    let mut config = config_with_hints(vec![("KEY_UP", "Scroll Up")]);
    config.resolve_hints();

    assert!(config.bindings.remap[&key(Key::BTN_DPAD_UP)].contains_key(&vec![]),
        "precondition: the button carries a base remap");
    assert!(config.bindings.hints_resolved.contains_key(&(key(Key::BTN_DPAD_UP), vec![])));
}

/// A label written on the binding itself outranks a modifier-less hint — the
/// same "the real thing wins" rule that applies to combo hints.
#[test]
fn written_label_wins_over_a_modifier_less_hint() {
    let mut config = config_with_hints(vec![("KEY_UP", "Scroll Up")]);
    config.bindings.labels.insert((key(Key::BTN_DPAD_UP), vec![]), "Up".to_string());
    config.resolve_hints();

    assert!(config.bindings.hints_resolved.is_empty());
}
