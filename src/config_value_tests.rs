//! A config parsed from a TOML value instead of a file, and the two helpers
//! the override session builds on: the cleared config and the trackpad table
//! read back in source form.

use super::*;
use evdev::Key;

fn value(text: &str) -> toml::Value {
    toml::from_str(text).unwrap()
}

fn key(k: Key) -> Event { Event::Key(k) }

/// A value goes through the same deserialisation as a file: the bindings land
/// where a file's would.
#[test]
fn a_value_parses_like_a_file() {
    let (config, problems) = Config::from_toml_value(
        value(r#"
            [remap]
            BTN_SOUTH = ["KEY_ENTER"]
            [commands]
            BTN_THUMBL = ["deckery-hud-toggle"]
        "#),
        "override".into(), &HashMap::new(),
    ).unwrap();
    assert!(problems.is_empty());
    assert_eq!(config.bindings.remap[&key(Key::BTN_SOUTH)][&vec![]], vec![Key::KEY_ENTER]);
    assert_eq!(config.bindings.commands[&key(Key::BTN_THUMBL)][&vec![]], vec!["deckery-hud-toggle"]);
}

/// The file's own rules hold: an unknown section is an error, not ignored.
#[test]
fn an_unknown_section_is_an_error() {
    let result = Config::from_toml_value(value("[remapp]\nBTN_SOUTH = []"), "o".into(), &HashMap::new());
    assert!(result.is_err());
}

/// An unknown key name in the output is a deserialisation error too.
#[test]
fn an_unknown_output_key_is_an_error() {
    let result = Config::from_toml_value(
        value(r#"remap = { BTN_SOUTH = ["KEY_NOPE"] }"#), "o".into(), &HashMap::new());
    assert!(result.is_err());
}

/// What a file only warns about is reported back, binding by binding.
#[test]
fn a_skipped_binding_is_reported() {
    let (config, problems) = Config::from_toml_value(
        value(r#"
            [remap]
            BTN_NOPE = ["KEY_ENTER"]
            BTN_SOUTH = ["KEY_ENTER"]
        "#),
        "o".into(), &HashMap::new(),
    ).unwrap();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("BTN_NOPE"));
    assert!(config.bindings.remap.contains_key(&key(Key::BTN_SOUTH)));
}

/// A movement that is no movement used to panic the whole process. It is now
/// skipped and reported like an unknown button.
#[test]
fn an_invalid_movement_is_reported_not_a_panic() {
    let (config, problems) = Config::from_toml_value(
        value(r#"movements = { BTN_SOUTH = "CURSOR_SIDEWAYS" }"#), "o".into(), &HashMap::new(),
    ).unwrap();
    assert_eq!(problems.len(), 1);
    assert!(config.bindings.movements.is_empty());
}

/// Clearing drops every binding and the modifiers they registered, and keeps
/// the settings, the trackpads and the modifiers the settings declare.
#[test]
fn without_bindings_keeps_what_is_not_a_binding() {
    let (config, _) = Config::from_toml_value(
        value(r#"
            [remap]
            BTN_TL-BTN_WEST = ["KEY_C"]
            BTN_SOUTH = ["KEY_ENTER"]
            [settings]
            CUSTOM_MODIFIERS = "BTN_GRIPL"
            [trackpad.right]
            mode = "trackball"
        "#),
        "o".into(), &HashMap::new(),
    ).unwrap();
    assert!(config.mapped_modifiers.custom.contains(&key(Key::BTN_TL)));

    let cleared = config.without_bindings();
    assert!(cleared.bindings.remap.is_empty());
    assert_eq!(cleared.mapped_modifiers.custom, vec![key(Key::BTN_GRIPL)]);
    assert!(cleared.mapped_modifiers.all.contains(&key(Key::KEY_LEFTCTRL)));
    assert!(!cleared.mapped_modifiers.all.contains(&key(Key::BTN_TL)));
    assert_eq!(cleared.settings.get("CUSTOM_MODIFIERS").map(String::as_str), Some("BTN_GRIPL"));
    assert_eq!(cleared.trackpad.right.mode, "trackball");
}

/// The trackpad table read back parses to the same trackpad config.
#[test]
fn the_trackpad_table_round_trips() {
    let (config, _) = Config::from_toml_value(
        value(r#"
            [trackpad]
            combined_gesture_device = true
            [trackpad.right]
            mode = "mt-trackpad"
            click_pressure = 30000
            [trackpad.right.kde]
            tap_to_click = false
            [trackpad.gestures.kde]
            natural_scroll = true
        "#),
        "o".into(), &HashMap::new(),
    ).unwrap();
    let raw: RawTrackpadConfig = config.trackpad.to_raw_value().try_into().unwrap();
    let back = TrackpadConfig::from_raw(raw, &mut Vec::new());
    assert_eq!(back.right.mode, "mt-trackpad");
    assert_eq!(back.right.click_pressure, Some(30000));
    assert_eq!(back.right.kde_config, config.trackpad.right.kde_config);
    assert_eq!(back.left.mode, "disabled");
    assert!(back.combined_gesture_device);
    assert_eq!(back.gesture_kde_config, config.trackpad.gesture_kde_config);
}

/// A pad's `click` switch in a file: on unless written false.
#[test]
fn the_pad_click_switch_defaults_to_on() {
    let (config, _) = Config::from_toml_value(
        value("[trackpad.right]\nmode = \"trackball\"\nclick = false\n[trackpad.left]\nmode = \"mt-trackpad\""),
        "o".into(), &HashMap::new(),
    ).unwrap();
    assert!(!config.trackpad.right.click);
    assert!(config.trackpad.left.click);
    assert!(TrackpadSideConfig::default().click);
}
