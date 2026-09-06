use super::*;
use crate::config_registry::ConfigSummary;

fn lifecycle_ready() -> AppLifecycle { AppLifecycle::Ready }
fn lifecycle_starting() -> AppLifecycle { AppLifecycle::Starting }
fn no_errors() -> HashMap<String, ErrorEntry> { HashMap::new() }
fn no_configs() -> Vec<ConfigSummary> { Vec::new() }

fn summary(name: &str, kind: &'static str, parent: Option<&str>, enabled: bool) -> ConfigSummary {
    ConfigSummary {
        name:    name.to_string(),
        kind,
        parent:  parent.map(str::to_string),
        enabled,
        errors:  vec![],
    }
}

#[test]
fn lifecycle_starting_serialises() {
    let json = build_json(&lifecycle_starting(), &no_errors(), &None, &no_configs());
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["lifecycle"], "starting");
    assert!(v["errors"].as_object().unwrap().is_empty());
}

#[test]
fn lifecycle_ready_serialises() {
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &no_configs());
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["lifecycle"], "ready");
}

#[test]
fn lifecycle_reinitializing_serialises() {
    let json = build_json(&AppLifecycle::Reinitializing, &no_errors(), &None, &no_configs());
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["lifecycle"], "reinitializing");
}

#[test]
fn set_error_appears_in_json() {
    let mut errors = no_errors();
    errors.insert("no_device".to_string(), ErrorEntry {
        message:  "no matching device found".to_string(),
        severity: "error",
    });
    let json = build_json(&lifecycle_ready(), &errors, &None, &no_configs());
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["errors"]["no_device"]["severity"], "error");
    assert!(v["errors"]["no_device"]["message"].as_str().unwrap().contains("device"));
}

#[test]
fn base_config_error_appears_in_json() {
    // Verify the base_config error slot — used by the tray to show the red icon
    // when the base config fails to parse.
    let mut errors = no_errors();
    errors.insert("base_config".to_string(), ErrorEntry {
        message:  "TOML error in \"Steam Deck\": expected `.`, `=`".to_string(),
        severity: "error",
    });
    let json = build_json(&lifecycle_ready(), &errors, &None, &no_configs());
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["errors"]["base_config"]["severity"], "error");
    assert!(v["errors"]["base_config"]["message"].as_str().unwrap().contains("TOML error"));
}

#[test]
fn clear_error_removes_from_json() {
    let errors = no_errors();
    let json = build_json(&lifecycle_ready(), &errors, &None, &no_configs());
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["errors"].as_object().unwrap().is_empty());
}

#[test]
fn event_state_merged_at_top_level() {
    let event_state = Some(serde_json::json!({
        "context": { "paused": false },
        "bindings": {},
    }));
    let json = build_json(&lifecycle_ready(), &no_errors(), &event_state, &no_configs());
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["context"].is_object());
    assert!(v["bindings"].is_object());
    assert_eq!(v["lifecycle"], "ready");
}

#[test]
fn event_state_none_omits_device_fields() {
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &no_configs());
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v.get("context").is_none());
    assert!(v.get("bindings").is_none());
}

#[test]
fn configs_always_present_in_json() {
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &no_configs());
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["configs"].is_array());
}

#[test]
fn loaded_configs_appear_with_enabled_flag() {
    let configs = vec![
        summary("Steam Deck", "base", None, true),
        summary("Firefox", "app", None, false),
    ];
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &configs);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let arr = v["configs"].as_array().unwrap();
    assert_eq!(arr.len(), 2);
    let deck = arr.iter().find(|e| e["name"] == "Steam Deck").unwrap();
    assert_eq!(deck["enabled"], true);
    assert_eq!(deck["status"], "ok");
    let firefox = arr.iter().find(|e| e["name"] == "Firefox").unwrap();
    assert_eq!(firefox["enabled"], false);
}

#[test]
fn kind_and_parent_are_serialised() {
    let configs = vec![
        summary("Steam Deck",  "base",   None,               true),
        summary("KDE Desktop", "module", Some("Steam Deck"), true),
        summary("Firefox",     "app",    None,               true),
    ];
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &configs);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let arr = v["configs"].as_array().unwrap();
    let by = |n: &str| arr.iter().find(|e| e["name"] == n).unwrap().clone();
    assert_eq!(by("Steam Deck")["kind"],  "base");
    assert!(by("Steam Deck")["parent"].is_null());
    assert_eq!(by("KDE Desktop")["kind"],   "module");
    assert_eq!(by("KDE Desktop")["parent"], "Steam Deck");
    assert_eq!(by("Firefox")["kind"], "app");
    assert!(by("Firefox")["parent"].is_null());
}
