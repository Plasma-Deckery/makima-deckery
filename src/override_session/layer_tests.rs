use super::*;
use crate::config::Event;
use evdev::Key;

fn key(k: Key) -> Event { Event::Key(k) }

fn json(text: &str) -> toml::Value {
    super::path::json_to_toml(&serde_json::from_str(text).unwrap()).unwrap()
}

fn config(text: &str) -> Config {
    let value: toml::Value = toml::from_str(text).unwrap();
    Config::from_toml_value(value, "Steam Deck".into(), &aliases()).unwrap().0
}

fn aliases() -> HashMap<String, String> {
    [("A", "BTN_SOUTH"), ("B", "BTN_EAST")].into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

fn base() -> Config {
    let mut c = config(r#"
        [remap]
        A = ["KEY_ENTER"]
        B = ["KEY_ESC"]
        "BTN_TL-BTN_WEST" = ["KEY_LEFTCTRL", "KEY_C"]
        [commands]
        BTN_THUMBL = ["deckery-hud-toggle"]
        [trackpad.right]
        mode = "mt-trackpad"
        click_pressure = 30000
        [trackpad.right.kde]
        tap_to_click = false
    "#);
    c.device = Some(crate::config::DeviceDeclaration {
        class: crate::config::DeviceClass::HidSteam, names: vec!["Steam Deck".into()] });
    c
}

fn layer(sets: &[(&str, &str)]) -> Layer {
    sets.iter().fold(Layer::default(), |l, (path, value)| l.with_value(path, json(value)).unwrap())
}

fn known() -> HashSet<Vec<String>> {
    allowlist::known_commands([&base()])
}

/// An empty layer changes nothing.
#[test]
fn an_empty_layer_is_the_underlay() {
    let effective = Layer::default().apply(&base()).unwrap();
    assert_eq!(effective.bindings.remap, base().bindings.remap);
    assert_eq!(effective.name, "Steam Deck");
    assert!(effective.device.is_some());
}

/// The layer wins over the underlay and keeps the rest, as an app override
/// does, and aliases resolve as in a file.
#[test]
fn a_set_binding_wins_and_the_rest_stays() {
    let effective = layer(&[("remap.A", r#"["KEY_SPACE"]"#)]).apply(&base()).unwrap();
    assert_eq!(effective.bindings.remap[&key(Key::BTN_SOUTH)][&vec![]], vec![Key::KEY_SPACE]);
    assert_eq!(effective.bindings.remap[&key(Key::BTN_EAST)][&vec![]], vec![Key::KEY_ESC]);
}

/// Cleared, nothing of the underlay's bindings is left — not even the custom
/// modifier its combo registered — and the trackpad stays.
#[test]
fn a_cleared_layer_starts_from_nothing() {
    let cleared = Layer::default().cleared();
    let effective = cleared.apply(&base()).unwrap();
    assert!(effective.bindings.remap.is_empty());
    assert!(effective.bindings.commands.is_empty());
    assert!(!effective.mapped_modifiers.all.contains(&key(Key::BTN_TL)));
    assert_eq!(effective.trackpad.right.mode, "mt-trackpad");

    let one = cleared.with_value("remap.B", json(r#"["KEY_ESC"]"#)).unwrap().apply(&base()).unwrap();
    assert_eq!(one.bindings.remap.len(), 1);
}

/// Clearing also empties what the layer itself had bound.
#[test]
fn clear_drops_the_layers_own_bindings_too() {
    let l = layer(&[("remap.A", r#"["KEY_SPACE"]"#), ("trackpad.right.mode", r#""trackball""#)]).cleared();
    let effective = l.apply(&base()).unwrap();
    assert!(effective.bindings.remap.is_empty());
    assert_eq!(effective.trackpad.right.mode, "trackball");
}

/// A trackpad mode switch keeps the rest of that pad's table.
#[test]
fn a_trackpad_mode_is_merged_into_the_pad_table() {
    let effective = layer(&[("trackpad.right.mode", r#""trackball""#)]).apply(&base()).unwrap();
    assert_eq!(effective.trackpad.right.mode, "trackball");
    assert_eq!(effective.trackpad.right.click_pressure, Some(30000));
    assert_eq!(effective.trackpad.right.kde_config["tap_to_click"].as_bool(), Some(false));
    assert_eq!(effective.trackpad.left.mode, "disabled");
}

#[test]
fn an_unknown_button_is_refused() {
    let err = layer(&[("remap.BTN_NOPE", r#"["KEY_ENTER"]"#)]).validate(&base(), &known()).unwrap_err().to_string();
    assert!(err.contains("BTN_NOPE"), "{err}");
}

#[test]
fn an_unknown_key_is_refused() {
    assert!(layer(&[("remap.A", r#"["KEY_NOPE"]"#)]).validate(&base(), &known()).is_err());
}

#[test]
fn a_section_that_is_not_a_table_is_refused() {
    assert!(layer(&[("remap", r#"["KEY_ENTER"]"#)]).validate(&base(), &known()).is_err());
}

#[test]
fn an_unknown_trackpad_mode_is_refused() {
    let err = layer(&[("trackpad.right.mode", r#""marble""#)]).validate(&base(), &known()).unwrap_err().to_string();
    assert!(err.contains("marble"), "{err}");
}

#[test]
fn an_unknown_trackpad_key_is_refused() {
    assert!(layer(&[("trackpad.middle", r#"{ "mode": "trackball" }"#)]).validate(&base(), &known()).is_err());
}

#[test]
fn a_setting_its_parser_refuses_is_refused() {
    let err = layer(&[("settings.LSTICK_DEADZONE", r#""lots""#)]).validate(&base(), &known()).unwrap_err().to_string();
    assert!(err.contains("LSTICK_DEADZONE"), "{err}");
}

#[test]
fn only_known_commands_may_be_bound() {
    assert!(layer(&[("commands.BTN_START", r#"["deckery-hud-toggle"]"#)]).validate(&base(), &known()).is_ok());
    assert!(layer(&[("commands.BTN_START", r#"["rm", "-rf", "/"]"#)]).validate(&base(), &known()).is_err());
}

#[test]
fn a_valid_layer_passes() {
    let l = layer(&[
        ("remap.A", r#"{ "keys": ["KEY_ENTER"], "label": "Enter" }"#),
        ("trackpad.right.mode", r#""trackball""#),
        ("settings.LSTICK", r#""cursor""#),
    ]).cleared().with_value("remap.B", json(r#"["KEY_ESC"]"#)).unwrap();
    assert_eq!(l.validate(&base(), &known()), Ok(()));
}

/// Switching a trackball pad to mt-trackpad keeps the trackball's keys in the
/// table, and the mt-trackpad parser takes them as another mode's, not as
/// unknown — the same as a user switching `mode` in their own file.
#[test]
fn a_mode_switch_leaves_no_unknown_keys_for_the_new_mode() {
    let mut underlay = base();
    underlay.trackpad = config(r#"
        [trackpad.right]
        mode = "trackball"
        sensitivity = 0.01
        [trackpad.right.haptic]
        on_roll = false
    "#).trackpad;
    let effective = layer(&[("trackpad.right.mode", r#""mt-trackpad""#)]).apply(&underlay).unwrap();
    assert_eq!(effective.trackpad.right.mode, "mt-trackpad");
    let (_, warnings) = crate::mt_trackpad::MtTrackpadConfig::parse(&effective.trackpad.right.handler_config);
    assert!(warnings.is_empty(), "{warnings:?}");
}
