use super::*;
use std::collections::HashMap;

fn config(text: &str) -> Config {
    let value: toml::Value = toml::from_str(text).unwrap();
    Config::from_toml_value(value, "c".into(), &HashMap::new()).unwrap().0
}

#[test]
fn the_known_commands_are_every_bound_command_line() {
    let a = config(r#"commands = { BTN_THUMBL = ["deckery-hud-toggle"], "BTN_TL-BTN_SOUTH" = ["notify-send", "hi"] }"#);
    let b = config(r#"commands = { BTN_THUMBR = { run = ["brightnessctl", "set", "+10%"] } }"#);
    let known = known_commands([&a, &b]);
    assert_eq!(known.len(), 3);
    assert!(known.contains(&vec!["brightnessctl".to_string(), "set".into(), "+10%".into()]));
}

#[test]
fn a_known_command_passes_and_an_unknown_one_does_not() {
    let known = known_commands([&config(r#"commands = { BTN_THUMBL = ["notify-send", "hello"] }"#)]);
    assert!(check(&config(r#"commands = { BTN_START = ["notify-send", "hello"] }"#), &known).is_ok());
    let err = check(&config(r#"commands = { BTN_START = ["notify-send", "bye"] }"#), &known).unwrap_err();
    assert!(err.contains("bye"), "{err}");
}

#[test]
fn no_commands_always_pass() {
    assert!(check(&config(r#"remap = { BTN_SOUTH = ["KEY_ENTER"] }"#), &HashSet::new()).is_ok());
}
