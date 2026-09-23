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
        exclusive_group: None,
        enabled,
        errors:  vec![],
    }
}

#[test]
fn lifecycle_starting_serialises() {
    let json = build_json(&lifecycle_starting(), &no_errors(), &None, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["lifecycle"], "starting");
    assert!(v["errors"].as_object().unwrap().is_empty());
}

#[test]
fn lifecycle_ready_serialises() {
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["lifecycle"], "ready");
}

#[test]
fn lifecycle_reinitializing_serialises() {
    let json = build_json(&AppLifecycle::Reinitializing, &no_errors(), &None, &no_configs(), &None);
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
    let json = build_json(&lifecycle_ready(), &errors, &None, &no_configs(), &None);
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
    let json = build_json(&lifecycle_ready(), &errors, &None, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["errors"]["base_config"]["severity"], "error");
    assert!(v["errors"]["base_config"]["message"].as_str().unwrap().contains("TOML error"));
}

#[test]
fn clear_error_removes_from_json() {
    let errors = no_errors();
    let json = build_json(&lifecycle_ready(), &errors, &None, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["errors"].as_object().unwrap().is_empty());
}

#[test]
fn event_state_merged_at_top_level() {
    let event_state = Some(serde_json::json!({
        "context": { "paused": false },
        "bindings": {},
    }));
    let json = build_json(&lifecycle_ready(), &no_errors(), &event_state, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["context"].is_object());
    assert!(v["bindings"].is_object());
    assert_eq!(v["lifecycle"], "ready");
}

#[test]
fn event_state_none_omits_device_fields() {
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v.get("context").is_none());
    assert!(v.get("bindings").is_none());
}

#[test]
fn configs_always_present_in_json() {
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["configs"].is_array());
}

#[test]
fn loaded_configs_appear_with_enabled_flag() {
    let configs = vec![
        summary("Steam Deck", "base", None, true),
        summary("Firefox", "app", None, false),
    ];
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &configs, &None);
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
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &configs, &None);
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

#[test]
fn config_roots_are_null_until_reported() {
    // The tray keys its two folder items off this field, and an item that opens
    // a guessed path is worse than no item at all.
    let json = build_json(&lifecycle_starting(), &no_errors(), &None, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["config_roots"].is_null());
}

#[test]
fn config_roots_are_serialised() {
    let roots = Some(crate::config_registry::ConfigRoots {
        system: std::path::PathBuf::from("/usr/share/deckery/configs"),
        user:   std::path::PathBuf::from("/home/u/.config/deckery"),
    });
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &no_configs(), &roots);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["config_roots"]["system"], "/usr/share/deckery/configs");
    assert_eq!(v["config_roots"]["user"],   "/home/u/.config/deckery");
}

#[test]
fn identical_state_serialises_identically() {
    // flush() skips the write when the document matches the last one, which is
    // what keeps a key press that changed nothing observable off the disk. That
    // only works while build_json is deterministic — in particular the errors
    // map, which is a HashMap here and would serialise in a different order on
    // every call if serde_json were built with `preserve_order`.
    let mut errors = HashMap::new();
    for id in ["zulu", "alpha", "mike", "bravo"] {
        errors.insert(id.to_string(), ErrorEntry {
            message:  format!("{id} failed"),
            severity: "error",
        });
    }
    let configs = vec![
        summary("Steam Deck", "base", None, true),
        summary("KDE Desktop", "module", Some("Steam Deck"), true),
    ];

    let first  = build_json(&lifecycle_ready(), &errors, &None, &configs, &None);
    let second = build_json(&lifecycle_ready(), &errors, &None, &configs, &None);
    assert_eq!(first, second);
}

#[test]
fn a_changed_field_changes_the_document() {
    // The other half of the dedup contract: a real change must not be skipped.
    let before = build_json(&lifecycle_ready(), &no_errors(), &None,
                            &vec![summary("KDE Desktop", "module", None, true)], &None);
    let after  = build_json(&lifecycle_ready(), &no_errors(), &None,
                            &vec![summary("KDE Desktop", "module", None, false)], &None);
    assert_ne!(before, after);
}

// ── Where the file goes, and that it actually gets there ──────────────────────

#[test]
fn the_runtime_directory_is_preferred() {
    assert_eq!(state_path_in(Some("/run/user/1000")),
               std::path::PathBuf::from("/run/user/1000/makima-state.json"));
}

#[test]
fn without_a_runtime_directory_tmp_is_the_fallback() {
    // A bare TTY or a container started without one. /tmp is worse, but a
    // readable state file beats none.
    assert_eq!(state_path_in(None),
               std::path::PathBuf::from("/tmp/makima-state.json"));
    assert_eq!(state_path_in(Some("")),
               std::path::PathBuf::from("/tmp/makima-state.json"));
}

/// A throwaway directory to write state into.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn flush_writes_the_document_to_the_given_path() {
    // Until the path was a parameter this could only be verified by running
    // makima and looking — see makima-deckery#42.
    let dir = scratch("deckery-flush-writes");
    let path = dir.join("makima-state.json");
    let mut written = String::new();

    flush(&AppLifecycle::Ready, &HashMap::new(), &None, &[], &None, &path, &mut written);

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(doc["lifecycle"], "ready");
    assert!(!written.is_empty(), "the written cache must hold what went to disk");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flush_leaves_no_temporary_file_behind() {
    // The write goes to a sibling and is renamed, so a reader never sees a
    // half-written document. The sibling must not survive the rename.
    let dir = scratch("deckery-flush-tmp");
    let path = dir.join("makima-state.json");
    let mut written = String::new();

    flush(&AppLifecycle::Ready, &HashMap::new(), &None, &[], &None, &path, &mut written);

    let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n != "makima-state.json")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flush_recreates_a_file_that_was_swept_away() {
    // Unchanged content is not written again — but only while last time's file
    // is still there. Without the existence check a swept runtime directory
    // would leave the tray reading nothing for as long as the state is calm.
    let dir = scratch("deckery-flush-recreate");
    let path = dir.join("makima-state.json");
    let mut written = String::new();

    flush(&AppLifecycle::Ready, &HashMap::new(), &None, &[], &None, &path, &mut written);
    std::fs::remove_file(&path).unwrap();
    flush(&AppLifecycle::Ready, &HashMap::new(), &None, &[], &None, &path, &mut written);

    assert!(path.exists(), "an unchanged document must still restore a missing file");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flush_skips_an_unchanged_document() {
    // State is reported on every key press; most presses change nothing a
    // reader could see, and rewriting the same bytes is disk traffic on the
    // input path.
    let dir = scratch("deckery-flush-skip");
    let path = dir.join("makima-state.json");
    let mut written = String::new();

    flush(&AppLifecycle::Ready, &HashMap::new(), &None, &[], &None, &path, &mut written);
    let first = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    flush(&AppLifecycle::Ready, &HashMap::new(), &None, &[], &None, &path, &mut written);

    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), first,
               "the file was rewritten although nothing changed");
    let _ = std::fs::remove_dir_all(&dir);
}
