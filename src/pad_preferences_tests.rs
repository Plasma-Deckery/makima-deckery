use super::*;
use crate::config::TrackpadSideConfig;

fn table(text: &str) -> PadPreferences {
    let value: toml::Value = toml::from_str(text).unwrap();
    value.as_table().unwrap().clone()
}

fn pad(mode: &str) -> TrackpadSideConfig {
    let mut t = toml::value::Table::new();
    t.insert("mode".into(), toml::Value::String(mode.into()));
    t.insert("click_pressure".into(), toml::Value::Integer(30000));
    TrackpadSideConfig { mode: mode.into(), handler_config: toml::Value::Table(t), ..Default::default() }
}

#[test]
fn a_chosen_mode_replaces_only_the_mode() {
    let choice = PadChoice::from_table(&table("[right]\nmode = \"trackball\""));
    assert_eq!(choice.right.as_deref(), Some("trackball"));
    let mut trackpad = TrackpadConfig { right: pad("mt-trackpad"), left: pad("disabled"), ..Default::default() };
    choice.apply(&mut trackpad);
    assert_eq!(trackpad.right.mode, "trackball");
    assert_eq!(trackpad.right.handler_config["mode"].as_str(), Some("trackball"));
    assert_eq!(trackpad.right.handler_config["click_pressure"].as_integer(), Some(30000));
    assert_eq!(trackpad.left.mode, "disabled", "the other pad is untouched");
}

#[test]
fn an_unknown_or_mistyped_mode_is_ignored() {
    let choice = PadChoice::from_table(&table("[right]\nmode = \"marble\"\n[left]\nmode = 3"));
    assert_eq!(choice, PadChoice::default());
}

#[test]
fn no_choice_changes_nothing() {
    let mut trackpad = TrackpadConfig { right: pad("mt-trackpad"), ..Default::default() };
    PadChoice::from_table(&PadPreferences::new()).apply(&mut trackpad);
    assert_eq!(trackpad.right.mode, "mt-trackpad");
}
