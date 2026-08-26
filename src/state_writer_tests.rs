use super::*;

fn lifecycle_ready() -> AppLifecycle { AppLifecycle::Ready }
fn lifecycle_starting() -> AppLifecycle { AppLifecycle::Starting }
fn no_errors() -> HashMap<String, ErrorEntry> { HashMap::new() }

#[test]
fn lifecycle_starting_serialises() {
    let json = build_json(&lifecycle_starting(), &no_errors(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["lifecycle"], "starting");
    assert!(v["errors"].as_object().unwrap().is_empty());
}

#[test]
fn lifecycle_ready_serialises() {
    let json = build_json(&lifecycle_ready(), &no_errors(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["lifecycle"], "ready");
}

#[test]
fn set_error_appears_in_json() {
    let mut errors = no_errors();
    errors.insert("no_device".to_string(), ErrorEntry {
        message:  "no matching device found".to_string(),
        severity: "error",
    });
    let json = build_json(&lifecycle_ready(), &errors, &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["errors"]["no_device"]["severity"], "error");
    assert!(v["errors"]["no_device"]["message"].as_str().unwrap().contains("device"));
}

#[test]
fn clear_error_removes_from_json() {
    let errors = no_errors(); // error was cleared before build_json call
    let json = build_json(&lifecycle_ready(), &errors, &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["errors"].as_object().unwrap().is_empty());
}

#[test]
fn event_state_merged_at_top_level() {
    let event_state = Some(serde_json::json!({
        "context": { "paused": false },
        "bindings": {},
    }));
    let json = build_json(&lifecycle_ready(), &no_errors(), &event_state);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    // event_state fields must be at top level, alongside lifecycle/errors
    assert!(v["context"].is_object());
    assert!(v["bindings"].is_object());
    assert_eq!(v["lifecycle"], "ready"); // lifecycle still present
}

#[test]
fn event_state_none_omits_device_fields() {
    let json = build_json(&lifecycle_ready(), &no_errors(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v.get("context").is_none());
    assert!(v.get("bindings").is_none());
}
