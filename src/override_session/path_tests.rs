use super::*;

fn doc() -> toml::value::Table {
    Default::default()
}

#[test]
fn a_path_into_a_section_is_accepted() {
    assert_eq!(segments("remap.BTN_SOUTH").unwrap(), vec!["remap", "BTN_SOUTH"]);
    assert_eq!(segments("trackpad.right.mode").unwrap(), vec!["trackpad", "right", "mode"]);
    assert_eq!(segments("remap.L1-X").unwrap(), vec!["remap", "L1-X"]);
}

#[test]
fn a_path_outside_the_sections_is_refused() {
    for path in ["device", "gaming_mode.trigger", "module", "remap..A", ".remap", "remap."] {
        assert!(segments(path).is_err(), "{path:?}");
    }
}

#[test]
fn json_becomes_the_matching_toml() {
    let json: serde_json::Value = serde_json::from_str(
        r#"{ "keys": ["KEY_ENTER"], "no_pause": true, "count": 3, "gain": 0.5 }"#).unwrap();
    let toml = json_to_toml(&json).unwrap();
    assert_eq!(toml["keys"], toml::Value::Array(vec![toml::Value::String("KEY_ENTER".into())]));
    assert_eq!(toml["no_pause"], toml::Value::Boolean(true));
    assert_eq!(toml["count"], toml::Value::Integer(3));
    assert_eq!(toml["gain"], toml::Value::Float(0.5));
}

#[test]
fn null_is_refused() {
    assert!(json_to_toml(&serde_json::json!({ "a": null })).is_err());
}

/// Setting a path creates the tables on the way and replaces what was there.
#[test]
fn insert_creates_and_replaces() {
    let mut d = doc();
    insert(&mut d, &["remap", "BTN_SOUTH"], toml::Value::Array(vec![])).unwrap();
    insert(&mut d, &["remap", "BTN_EAST"], toml::Value::Array(vec![])).unwrap();
    assert_eq!(d["remap"].as_table().unwrap().len(), 2);

    insert(&mut d, &["remap"], toml::Value::Table(Default::default())).unwrap();
    assert!(d["remap"].as_table().unwrap().is_empty());
}

#[test]
fn insert_through_a_value_is_refused() {
    let mut d = doc();
    insert(&mut d, &["remap", "BTN_SOUTH"], toml::Value::Array(vec![])).unwrap();
    let err = insert(&mut d, &["remap", "BTN_SOUTH", "keys"], toml::Value::Array(vec![])).unwrap_err();
    assert!(err.contains("remap.BTN_SOUTH"), "{err}");
}

#[test]
fn deep_merge_merges_tables_and_replaces_values() {
    let mut under: toml::Value = toml::from_str(
        "[right]\nmode = \"mt-trackpad\"\nclick_pressure = 30000\n[right.kde]\ntap_to_click = false").unwrap();
    let over: toml::Value = toml::from_str("[right]\nmode = \"trackball\"").unwrap();
    deep_merge(&mut under, &over);
    assert_eq!(under["right"]["mode"].as_str(), Some("trackball"));
    assert_eq!(under["right"]["click_pressure"].as_integer(), Some(30000));
    assert_eq!(under["right"]["kde"]["tap_to_click"].as_bool(), Some(false));
}
