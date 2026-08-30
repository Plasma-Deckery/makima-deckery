use super::*;
use crate::config::{Associations, Config};
use crate::udev_monitor::Client;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a registry directly from a list of entries (bypasses the filesystem).
fn make_registry(entries: Vec<ConfigEntry>) -> Arc<ConfigRegistry> {
    let map = entries.into_iter().map(|e| (e.name.clone(), e)).collect();
    Arc::new(ConfigRegistry { entries: Mutex::new(map) })
}

/// A minimal valid ConfigEntry with a parsed config.
fn entry(name: &str, layout: u16, client: Client, enabled: bool) -> ConfigEntry {
    let mut c = Config::new_empty(name.to_string());
    c.associations = Associations { client, layout };
    ConfigEntry { name: name.to_string(), config: Some(c), enabled, errors: vec![] }
}

/// A ConfigEntry that failed to parse (config: None).
fn broken_entry(name: &str) -> ConfigEntry {
    ConfigEntry {
        name: name.to_string(),
        config: None,
        enabled: true,
        errors: vec![ConfigError { severity: "error", message: "parse failed".into() }],
    }
}

fn class(name: &str) -> Client { Client::Class(name.to_string(), String::new(), None) }

// ── parse_associations ────────────────────────────────────────────────────────

#[test]
fn parse_base_name() {
    let (assoc, warnings) = parse_associations("Steam Deck");
    assert_eq!(assoc, Associations::default());
    assert!(warnings.is_empty());
}

#[test]
fn parse_layout_only() {
    let (assoc, warnings) = parse_associations("Steam Deck::2");
    assert_eq!(assoc.layout, 2);
    assert!(matches!(assoc.client, Client::Default));
    assert!(warnings.is_empty());
}

#[test]
fn parse_client_only() {
    let (assoc, warnings) = parse_associations("Steam Deck::Firefox");
    assert_eq!(assoc.layout, 0);
    assert!(matches!(&assoc.client, Client::Class(c, _, _) if c == "Firefox"));
    assert!(warnings.is_empty());
}

#[test]
fn parse_layout_then_client() {
    // "Device::N::Class" — layout first, then class
    let (assoc, warnings) = parse_associations("Steam Deck::3::Firefox");
    assert_eq!(assoc.layout, 3);
    assert!(matches!(&assoc.client, Client::Class(c, _, _) if c == "Firefox"));
    assert!(warnings.is_empty());
}

#[test]
fn parse_client_then_layout() {
    // "Device::Class::N" — class first, then layout (alternative order)
    let (assoc, warnings) = parse_associations("Steam Deck::Firefox::3");
    assert_eq!(assoc.layout, 3);
    assert!(matches!(&assoc.client, Client::Class(c, _, _) if c == "Firefox"));
    assert!(warnings.is_empty());
}

#[test]
fn parse_two_non_numeric_segments_yields_warning() {
    let (assoc, warnings) = parse_associations("Steam Deck::foo::bar");
    assert_eq!(assoc, Associations::default());
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].severity, "warning");
}

#[test]
fn parse_too_many_segments_yields_warning() {
    let (assoc, warnings) = parse_associations("Steam Deck::a::b::c");
    assert_eq!(assoc, Associations::default());
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].severity, "warning");
}

// ── device_has_configs ────────────────────────────────────────────────────────

#[test]
fn device_has_configs_exact_base() {
    let r = make_registry(vec![entry("Steam Deck", 0, Client::Default, true)]);
    assert!(r.device_has_configs("Steam Deck"));
}

#[test]
fn device_has_configs_via_app_prefix() {
    let r = make_registry(vec![entry("Steam Deck::Firefox", 0, class("Firefox"), true)]);
    assert!(r.device_has_configs("Steam Deck"));
}

#[test]
fn device_has_configs_no_partial_prefix_match() {
    let r = make_registry(vec![entry("Steam Deck::Firefox", 0, class("Firefox"), true)]);
    // "Steam" is not the full device-name prefix — must not match
    assert!(!r.device_has_configs("Steam"));
}

#[test]
fn device_has_configs_empty_registry() {
    let r = ConfigRegistry::empty();
    assert!(!r.device_has_configs("Steam Deck"));
}

// ── base_config ───────────────────────────────────────────────────────────────

#[test]
fn base_config_returns_when_enabled() {
    let r = make_registry(vec![entry("Steam Deck", 0, Client::Default, true)]);
    assert!(r.base_config("Steam Deck").is_some());
}

#[test]
fn base_config_none_when_disabled() {
    let r = make_registry(vec![entry("Steam Deck", 0, Client::Default, false)]);
    assert!(r.base_config("Steam Deck").is_none());
}

#[test]
fn base_config_none_when_parse_failed() {
    let r = make_registry(vec![broken_entry("Steam Deck")]);
    assert!(r.base_config("Steam Deck").is_none());
}

// ── set_enabled / snapshot ────────────────────────────────────────────────────

#[test]
fn set_enabled_returns_true_when_found() {
    let r = make_registry(vec![entry("Steam Deck", 0, Client::Default, true)]);
    assert!(r.set_enabled("Steam Deck", false));
}

#[test]
fn set_enabled_returns_false_when_not_found() {
    let r = ConfigRegistry::empty();
    assert!(!r.set_enabled("Steam Deck", false));
}

#[test]
fn set_enabled_reflected_in_snapshot() {
    let r = make_registry(vec![entry("Steam Deck", 0, Client::Default, true)]);
    assert!(r.snapshot().iter().any(|e| e.name == "Steam Deck" && e.enabled));
    r.set_enabled("Steam Deck", false);
    assert!(r.snapshot().iter().any(|e| e.name == "Steam Deck" && !e.enabled));
}

#[test]
fn snapshot_contains_all_entries() {
    let r = make_registry(vec![
        entry("Steam Deck",          0, Client::Default,  true),
        entry("Steam Deck::Firefox", 0, class("Firefox"), true),
        broken_entry("Steam Deck::bad"),
    ]);
    assert_eq!(r.snapshot().len(), 3);
}

// ── resolve ───────────────────────────────────────────────────────────────────

#[test]
fn resolve_returns_none_without_base() {
    // Only an app config, no base — resolve must return None
    let r = make_registry(vec![entry("Steam Deck::Firefox", 0, class("Firefox"), true)]);
    assert!(r.resolve("Steam Deck", &class("Firefox"), 0).is_none());
}

#[test]
fn resolve_returns_base_when_no_app_matches() {
    let r = make_registry(vec![entry("Steam Deck", 0, Client::Default, true)]);
    let cfg = r.resolve("Steam Deck", &class("Firefox"), 0).unwrap();
    assert_eq!(cfg.name, "Steam Deck");
}

#[test]
fn resolve_exact_match_wins() {
    // Both a layout-only and an exact (layout+client) config exist.
    // The exact match must be preferred.
    let r = make_registry(vec![
        entry("Steam Deck",             0, Client::Default,  true),
        entry("Steam Deck::1",          1, Client::Default,  true),   // layout-only
        entry("Steam Deck::1::Firefox", 1, class("Firefox"), true),   // exact
    ]);
    let cfg = r.resolve("Steam Deck", &class("Firefox"), 1).unwrap();
    // The merged config carries the app config's name
    assert_eq!(cfg.name, "Steam Deck::1::Firefox");
}

#[test]
fn resolve_layout_only_fallback() {
    // Exact client not present — fall back to layout-only config.
    let r = make_registry(vec![
        entry("Steam Deck",    0, Client::Default, true),
        entry("Steam Deck::2", 2, Client::Default, true),
    ]);
    let cfg = r.resolve("Steam Deck", &class("Firefox"), 2).unwrap();
    assert_eq!(cfg.name, "Steam Deck::2");
}

#[test]
fn resolve_disabled_base_returns_none() {
    let r = make_registry(vec![
        entry("Steam Deck",          0, Client::Default,  false), // disabled
        entry("Steam Deck::Firefox", 0, class("Firefox"), true),
    ]);
    assert!(r.resolve("Steam Deck", &class("Firefox"), 0).is_none());
}

#[test]
fn resolve_disabled_app_falls_back_to_base() {
    let r = make_registry(vec![
        entry("Steam Deck",          0, Client::Default,  true),
        entry("Steam Deck::Firefox", 0, class("Firefox"), false), // disabled
    ]);
    let cfg = r.resolve("Steam Deck", &class("Firefox"), 0).unwrap();
    // No app matched — should return base config
    assert_eq!(cfg.name, "Steam Deck");
}

#[test]
fn resolve_broken_app_config_falls_back_to_base() {
    let r = make_registry(vec![
        entry("Steam Deck", 0, Client::Default, true),
        broken_entry("Steam Deck::Firefox"),
    ]);
    let cfg = r.resolve("Steam Deck", &class("Firefox"), 0).unwrap();
    assert_eq!(cfg.name, "Steam Deck");
}

#[test]
fn resolve_wrong_device_returns_none() {
    let r = make_registry(vec![entry("Steam Deck", 0, Client::Default, true)]);
    assert!(r.resolve("Xbox Controller", &Client::Default, 0).is_none());
}

// ── base_config_error ─────────────────────────────────────────────────────────

#[test]
fn base_config_error_none_when_empty_registry() {
    // Empty registry → no base config → no error.
    let r = ConfigRegistry::empty();
    assert!(r.base_config_error().is_none());
}

#[test]
fn base_config_error_none_when_all_valid() {
    // All configs parse fine → no global error.
    let r = make_registry(vec![
        entry("Steam Deck",          0, Client::Default,  true),
        entry("Steam Deck::Firefox", 0, class("Firefox"), true),
    ]);
    assert!(r.base_config_error().is_none());
}

#[test]
fn base_config_error_some_when_base_broken() {
    // Base config fails to parse → error message returned.
    let r = make_registry(vec![
        broken_entry("Steam Deck"),
        entry("Steam Deck::Firefox", 0, class("Firefox"), true),
    ]);
    let msg = r.base_config_error();
    assert!(msg.is_some());
    assert!(msg.unwrap().contains("parse failed"));
}

#[test]
fn base_config_error_none_when_only_app_config_broken() {
    // A broken app config (with "::") is NOT a global base error.
    let r = make_registry(vec![
        entry("Steam Deck",          0, Client::Default,  true),
        broken_entry("Steam Deck::Firefox"),
    ]);
    assert!(r.base_config_error().is_none());
}

#[test]
fn base_config_error_none_when_app_config_has_warning() {
    // Warnings on app configs are not errors.
    let r = make_registry(vec![
        entry("Steam Deck", 0, Client::Default, true),
        ConfigEntry {
            name:    "Steam Deck::Firefox".to_string(),
            config:  None,
            enabled: false,
            errors:  vec![ConfigError { severity: "warning", message: "deprecated key".into() }],
        },
    ]);
    assert!(r.base_config_error().is_none());
}

#[test]
fn base_config_error_ignores_warning_severity_on_base() {
    // A warning on the base config is NOT a global error — only "error" severity counts.
    let r = make_registry(vec![
        ConfigEntry {
            name:    "Steam Deck".to_string(),
            config:  Some(Config::new_empty("Steam Deck".to_string())),
            enabled: true,
            errors:  vec![ConfigError { severity: "warning", message: "unknown key".into() }],
        },
    ]);
    assert!(r.base_config_error().is_none());
}

#[test]
fn base_config_error_multiple_devices_only_returns_broken_base() {
    // Two devices: one with a broken base, one fine.
    // Error should be reported (we only check Some vs None here — the
    // registry doesn't guarantee which device's message comes first).
    let r = make_registry(vec![
        broken_entry("Steam Deck"),
        entry("Xbox Controller",          0, Client::Default,  true),
        entry("Xbox Controller::Firefox", 0, class("Firefox"), true),
    ]);
    assert!(r.base_config_error().is_some());
}

#[test]
fn base_config_error_cleared_after_entry_removed_and_recreated() {
    // Simulate a reload: broken entry replaced by a valid one.
    // base_config_error() reflects current in-memory state.
    let mut entries = std::collections::HashMap::new();
    let broken = broken_entry("Steam Deck");
    entries.insert(broken.name.clone(), broken);
    let r = Arc::new(ConfigRegistry { entries: std::sync::Mutex::new(entries) });
    assert!(r.base_config_error().is_some());

    // Replace with a valid entry (simulates successful reload).
    {
        let mut map = r.entries.lock().unwrap();
        map.insert("Steam Deck".to_string(), entry("Steam Deck", 0, Client::Default, true));
    }
    assert!(r.base_config_error().is_none());
}
