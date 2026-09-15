use super::*;
use crate::config::{Config, DeviceClass, DeviceDeclaration, Event};
use crate::udev_monitor::Client;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a registry directly from a list of entries (bypasses the filesystem).
fn make_registry(entries: Vec<ConfigEntry>) -> Arc<ConfigRegistry> {
    let map = entries.into_iter().map(|e| (e.name.clone(), e)).collect();
    Arc::new(ConfigRegistry {
        roots: ConfigRoots { system: PathBuf::new(), user: PathBuf::new() },
        entries: Mutex::new(map),
        compositor: Mutex::new(None),
    })
}

fn wrap(config: Config, enabled: bool) -> ConfigEntry {
    ConfigEntry {
        name: config.name.clone(),
        config: Some(config),
        enabled,
        errors: vec![],
    }
}

/// A base config declaring a `[device]` section.
fn base(name: &str, device_names: &[&str]) -> Config {
    let mut c = Config::new_empty(name.to_string());
    c.device = Some(DeviceDeclaration {
        class: DeviceClass::HidSteam,
        names: device_names.iter().map(|s| s.to_string()).collect(),
    });
    c
}

/// A module config: no `[device]`, optionally bound to a window class / layout.
fn module(name: &str, window_class: Option<&str>, layout: u16) -> Config {
    let mut c = Config::new_empty(name.to_string());
    c.module.match_window_class = window_class.map(|s| vec![s.to_string()]);
    c.module.layout = layout;
    c
}

/// Give a config one distinguishable binding so merge results can be asserted.
fn with_binding(mut c: Config, trigger: evdev::Key, output: evdev::Key) -> Config {
    c.bindings.remap
        .entry(Event::Key(trigger))
        .or_default()
        .insert(vec![], vec![output]);
    c
}

fn has_binding(c: &Config, trigger: evdev::Key, output: evdev::Key) -> bool {
    c.bindings.remap
        .get(&Event::Key(trigger))
        .and_then(|m| m.get(&vec![]))
        .is_some_and(|keys| keys == &vec![output])
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

// ── any_device_matches ────────────────────────────────────────────────────────

#[test]
fn device_matches_declared_name() {
    let r = make_registry(vec![wrap(base("Steam Deck", &["Steam Deck"]), true)]);
    assert!(r.any_device_matches("Steam Deck"));
}

#[test]
fn device_matches_by_substring() {
    // The kernel-reported name varies; a declared name matching as a substring is enough.
    let r = make_registry(vec![wrap(base("Steam Deck", &["Steam Controller"]), true)]);
    assert!(r.any_device_matches("Valve Software Steam Controller"));
}

#[test]
fn device_matches_any_of_several_declared_names() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck", "Steam Controller"]), true),
    ]);
    assert!(r.any_device_matches("Valve Software Steam Controller"));
    assert!(r.any_device_matches("Steam Deck"));
}

#[test]
fn device_does_not_match_undeclared_name() {
    let r = make_registry(vec![wrap(base("Steam Deck", &["Steam Deck"]), true)]);
    assert!(!r.any_device_matches("Xbox Controller"));
}

#[test]
fn module_without_device_section_matches_nothing() {
    let r = make_registry(vec![wrap(module("konsole", Some("org.kde.konsole"), 0), true)]);
    assert!(!r.any_device_matches("Steam Deck"));
}

#[test]
fn disabled_base_matches_nothing() {
    let r = make_registry(vec![wrap(base("Steam Deck", &["Steam Deck"]), false)]);
    assert!(!r.any_device_matches("Steam Deck"));
}

#[test]
fn empty_registry_matches_nothing() {
    assert!(!ConfigRegistry::empty().any_device_matches("Steam Deck"));
}

// ── base_configs ──────────────────────────────────────────────────────────────

#[test]
fn base_configs_lists_only_configs_with_device_section() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("konsole", Some("org.kde.konsole"), 0), true),
    ]);
    let bases = r.base_configs();
    assert_eq!(bases.len(), 1);
    assert_eq!(bases[0].name, "Steam Deck");
}

#[test]
fn base_configs_excludes_disabled_and_broken() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), false),
        broken_entry("Xbox"),
    ]);
    assert!(r.base_configs().is_empty());
}

// ── window_class_modules ──────────────────────────────────────────────────────

#[test]
fn window_class_modules_lists_only_class_bound_modules() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("konsole", Some("org.kde.konsole"), 0), true),
        wrap(module("layer2", None, 2), true),
    ]);
    let mods = r.window_class_modules();
    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].module.match_window_class.as_deref(), Some(["org.kde.konsole".to_string()].as_slice()));
}

#[test]
fn window_class_modules_excludes_disabled() {
    let r = make_registry(vec![
        wrap(module("konsole", Some("org.kde.konsole"), 0), false),
    ]);
    assert!(r.window_class_modules().is_empty());
}

// ── requires_compositor gating ────────────────────────────────────────────────

#[test]
fn compositor_specific_module_hidden_until_compositor_is_set() {
    let mut m = module("kde-gestures", Some("org.kde.konsole"), 0);
    m.module.requires_compositor = Some("KDE".into());
    let r = make_registry(vec![wrap(m, true)]);
    assert!(r.window_class_modules().is_empty());
}

#[test]
fn compositor_specific_module_visible_on_matching_compositor() {
    let mut m = module("kde-gestures", Some("org.kde.konsole"), 0);
    m.module.requires_compositor = Some("KDE".into());
    let r = make_registry(vec![wrap(m, true)]);
    r.set_compositor(Some("KDE".into()));
    assert_eq!(r.window_class_modules().len(), 1);
}

#[test]
fn compositor_specific_module_hidden_on_other_compositor() {
    let mut m = module("kde-gestures", Some("org.kde.konsole"), 0);
    m.module.requires_compositor = Some("KDE".into());
    let r = make_registry(vec![wrap(m, true)]);
    r.set_compositor(Some("Hyprland".into()));
    assert!(r.window_class_modules().is_empty());
}

#[test]
fn unconditional_module_visible_regardless_of_compositor() {
    let r = make_registry(vec![wrap(module("konsole", Some("org.kde.konsole"), 0), true)]);
    r.set_compositor(Some("Hyprland".into()));
    assert_eq!(r.window_class_modules().len(), 1);
}

// ── Automatic module merging ──────────────────────────────────────────────────

#[test]
fn plain_module_bindings_are_merged_into_base() {
    let b = base("Steam Deck", &["Steam Deck"]);
    let m = with_binding(module("gestures", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);

    let r = make_registry(vec![wrap(b, true), wrap(m, true)]);
    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));
}

#[test]
fn base_binding_wins_over_module() {
    let b = with_binding(base("Steam Deck", &["Steam Deck"]), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);
    let m = with_binding(module("gestures", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);

    let r = make_registry(vec![wrap(b, true), wrap(m, true)]);
    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_B));
}

#[test]
fn alphabetically_last_module_wins_over_earlier_one() {
    // Colliding bindings are a config bug, reported by report_binding_conflicts.
    // Order is fixed alphabetically so the outcome is at least deterministic,
    // and the warning lands on the config that actually took effect.
    let first  = with_binding(module("aaa", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let second = with_binding(module("bbb", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);

    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(first, true),
        wrap(second, true),
    ]);
    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_B));
}

#[test]
fn module_gated_to_another_compositor_is_not_merged() {
    let mut m = with_binding(module("kde-only", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    m.module.requires_compositor = Some("KDE".into());

    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(m, true),
    ]);
    r.set_compositor(Some("Hyprland".into()));
    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert!(!has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));
}

#[test]
fn disabled_module_is_not_merged() {
    let m = with_binding(module("gestures", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(m, false),
    ]);
    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert!(!has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));
}

#[test]
fn app_and_layout_modules_are_not_merged_into_base() {
    let app    = with_binding(module("konsole", Some("org.kde.konsole"), 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let layout = with_binding(module("layer2", None, 2), evdev::Key::BTN_NORTH, evdev::Key::KEY_B);

    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(app, true),
        wrap(layout, true),
    ]);
    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert!(!has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));
    assert!(!has_binding(&cfg, evdev::Key::BTN_NORTH, evdev::Key::KEY_B));
}

#[test]
fn trackpad_config_reaches_the_base_from_a_module() {
    // The shipped trackpad config lives in its own module, and the base config
    // carries no [trackpad] at all. merge_base() only inherits trackpad
    // settings when both of its own sides are "disabled" — which an empty base
    // happens to satisfy. Tightening that condition would silently leave both
    // pads dead, hence this test.
    let mut m = module("Steam Deck Trackpad", None, 0);
    m.trackpad.right.mode = "mt-trackpad".to_string();
    m.trackpad.combined_gesture_device = true;

    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(m, true),
    ]);
    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert_eq!(cfg.trackpad.right.mode, "mt-trackpad");
    assert!(cfg.trackpad.combined_gesture_device);
}

#[test]
fn base_trackpad_config_wins_over_a_module() {
    // The other direction: once the base declares a pad, the module must not
    // take it back — an override of Steam Deck.toml has to stay in charge.
    let mut b = base("Steam Deck", &["Steam Deck"]);
    b.trackpad.right.mode = "trackball".to_string();
    let mut m = module("Steam Deck Trackpad", None, 0);
    m.trackpad.right.mode = "mt-trackpad".to_string();

    let r = make_registry(vec![wrap(b, true), wrap(m, true)]);
    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert_eq!(cfg.trackpad.right.mode, "trackball");
}

#[test]
fn settings_reach_the_base_from_a_module() {
    // [settings] moved into Steam Deck Settings.toml, so stick mode and
    // deadzones now arrive through the module merge rather than from the file
    // that declares the device.
    let mut m = module("Steam Deck Settings", None, 0);
    m.settings.insert("RSTICK".to_string(), "cursor".to_string());

    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(m, true),
    ]);
    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert_eq!(cfg.settings.get("RSTICK").map(String::as_str), Some("cursor"));
}

#[test]
fn module_does_not_override_base_gaming_mode() {
    let mut b = base("Steam Deck", &["Steam Deck"]);
    b.gaming_mode_config.auto_detect_steam_games = false;

    let r = make_registry(vec![wrap(b, true), wrap(module("gestures", None, 0), true)]);
    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert!(!cfg.gaming_mode_config.auto_detect_steam_games);
}

// ── resolve ───────────────────────────────────────────────────────────────────

#[test]
fn resolve_returns_none_without_base() {
    let r = make_registry(vec![wrap(module("konsole", Some("org.kde.konsole"), 0), true)]);
    assert!(r.resolve("Steam Deck", &class("org.kde.konsole"), 0).is_none());
}

#[test]
fn resolve_returns_base_when_no_module_matches() {
    let r = make_registry(vec![wrap(base("Steam Deck", &["Steam Deck"]), true)]);
    let cfg = r.resolve("Steam Deck", &class("org.kde.konsole"), 0).unwrap();
    assert_eq!(cfg.name, "Steam Deck");
}

#[test]
fn resolve_applies_window_class_module() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("konsole", Some("org.kde.konsole"), 0), true),
    ]);
    let cfg = r.resolve("Steam Deck", &class("org.kde.konsole"), 0).unwrap();
    assert_eq!(cfg.name, "konsole");
}

#[test]
fn resolve_ignores_module_for_other_window_class() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("konsole", Some("org.kde.konsole"), 0), true),
    ]);
    let cfg = r.resolve("Steam Deck", &class("firefox"), 0).unwrap();
    assert_eq!(cfg.name, "Steam Deck");
}

#[test]
fn resolve_window_class_module_wins_over_layout_only_module() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("layer1", None, 1), true),
        wrap(module("konsole-layer1", Some("org.kde.konsole"), 1), true),
    ]);
    let cfg = r.resolve("Steam Deck", &class("org.kde.konsole"), 1).unwrap();
    assert_eq!(cfg.name, "konsole-layer1");
}

#[test]
fn resolve_falls_back_to_layout_only_module() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("layer2", None, 2), true),
    ]);
    let cfg = r.resolve("Steam Deck", &class("firefox"), 2).unwrap();
    assert_eq!(cfg.name, "layer2");
}

#[test]
fn resolve_returns_none_for_unpopulated_layout() {
    // change_active_layout() relies on this to skip empty layout slots.
    let r = make_registry(vec![wrap(base("Steam Deck", &["Steam Deck"]), true)]);
    assert!(r.resolve("Steam Deck", &Client::Default, 3).is_none());
}

#[test]
fn resolve_module_bound_to_other_layout_does_not_apply() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("konsole", Some("org.kde.konsole"), 1), true),
    ]);
    let cfg = r.resolve("Steam Deck", &class("org.kde.konsole"), 0).unwrap();
    assert_eq!(cfg.name, "Steam Deck");
}

#[test]
fn resolve_disabled_base_returns_none() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), false),
        wrap(module("konsole", Some("org.kde.konsole"), 0), true),
    ]);
    assert!(r.resolve("Steam Deck", &class("org.kde.konsole"), 0).is_none());
}

#[test]
fn resolve_disabled_module_falls_back_to_base() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("konsole", Some("org.kde.konsole"), 0), false),
    ]);
    let cfg = r.resolve("Steam Deck", &class("org.kde.konsole"), 0).unwrap();
    assert_eq!(cfg.name, "Steam Deck");
}

#[test]
fn resolve_broken_module_falls_back_to_base() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        broken_entry("konsole"),
    ]);
    let cfg = r.resolve("Steam Deck", &class("org.kde.konsole"), 0).unwrap();
    assert_eq!(cfg.name, "Steam Deck");
}

#[test]
fn resolve_unknown_base_name_returns_none() {
    let r = make_registry(vec![wrap(base("Steam Deck", &["Steam Deck"]), true)]);
    assert!(r.resolve("Xbox Controller", &Client::Default, 0).is_none());
}

#[test]
fn resolve_keys_on_config_name_not_device_declaration() {
    // The config is named "deck"; its [device] names share nothing with it.
    // launch_tasks() already did the device matching, so resolve() must find
    // the config by name alone.
    let r = make_registry(vec![wrap(base("deck", &["Valve Software Steam Controller"]), true)]);
    assert_eq!(r.resolve("deck", &Client::Default, 0).unwrap().name, "deck");
    // The kernel name is not a registry key and must not resolve.
    assert!(r.resolve("Valve Software Steam Controller", &Client::Default, 0).is_none());
}

#[test]
fn resolve_module_name_is_not_a_base_config() {
    // Only entries with a [device] section can anchor a resolve.
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("konsole", Some("org.kde.konsole"), 0), true),
    ]);
    assert!(r.resolve("konsole", &Client::Default, 0).is_none());
}

#[test]
fn resolve_module_gated_by_compositor_does_not_apply() {
    let mut m = module("konsole", Some("org.kde.konsole"), 0);
    m.module.requires_compositor = Some("KDE".into());
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(m, true),
    ]);
    r.set_compositor(Some("Hyprland".into()));
    let cfg = r.resolve("Steam Deck", &class("org.kde.konsole"), 0).unwrap();
    assert_eq!(cfg.name, "Steam Deck");
}

// ── set_enabled / snapshot ────────────────────────────────────────────────────

#[test]
fn set_enabled_returns_true_when_found() {
    let r = make_registry(vec![wrap(base("Steam Deck", &["Steam Deck"]), true)]);
    assert!(r.set_enabled("Steam Deck", false));
}

#[test]
fn set_enabled_returns_false_when_not_found() {
    assert!(!ConfigRegistry::empty().set_enabled("Steam Deck", false));
}

#[test]
fn set_enabled_reflected_in_snapshot() {
    let r = make_registry(vec![wrap(base("Steam Deck", &["Steam Deck"]), true)]);
    assert!(r.snapshot().iter().any(|e| e.name == "Steam Deck" && e.enabled));
    r.set_enabled("Steam Deck", false);
    assert!(r.snapshot().iter().any(|e| e.name == "Steam Deck" && !e.enabled));
}

#[test]
fn set_enabled_refuses_to_activate_broken_config() {
    let r = make_registry(vec![broken_entry("Steam Deck")]);
    assert!(!r.set_enabled("Steam Deck", true));
}

#[test]
fn snapshot_contains_all_entries() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("konsole", Some("org.kde.konsole"), 0), true),
        broken_entry("bad"),
    ]);
    assert_eq!(r.snapshot().len(), 3);
}

#[test]
fn snapshot_reports_kind_per_entry() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("Konsole", Some("org.kde.konsole"), 0), true),
        wrap(module("Voice Control", None, 0), true),
        broken_entry("bad"),
    ]);
    let snap = r.snapshot();
    let kind = |n: &str| snap.iter().find(|e| e.name == n).unwrap().kind;
    assert_eq!(kind("Steam Deck"),    "base");
    assert_eq!(kind("Konsole"),       "app");
    assert_eq!(kind("Voice Control"), "module");
    assert_eq!(kind("bad"),           "unknown");
}

#[test]
fn snapshot_nests_plain_modules_under_the_base_config() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("Voice Control", None, 0), true),
        wrap(module("Konsole", Some("org.kde.konsole"), 0), true),
    ]);
    let snap = r.snapshot();
    let parent = |n: &str| snap.iter().find(|e| e.name == n).unwrap().parent.clone();
    assert_eq!(parent("Voice Control"), Some("Steam Deck".to_string()));
    assert_eq!(parent("Steam Deck"),    None);
    assert_eq!(parent("Konsole"),       None);
}

#[test]
fn snapshot_hides_modules_gated_to_another_compositor() {
    let mut hypr = module("Hyprland Desktop", None, 0);
    hypr.module.requires_compositor = Some("Hyprland".to_string());
    let mut kde = module("KDE Desktop", None, 0);
    kde.module.requires_compositor = Some("KDE".to_string());
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(hypr, true),
        wrap(kde, true),
    ]);
    r.set_compositor(Some("KDE".to_string()));
    let names: Vec<String> = r.snapshot().into_iter().map(|e| e.name).collect();
    assert!(names.contains(&"KDE Desktop".to_string()));
    assert!(!names.contains(&"Hyprland Desktop".to_string()));
}

#[test]
fn snapshot_keeps_unparsed_files_regardless_of_compositor() {
    let r = make_registry(vec![broken_entry("Hyprland Desktop")]);
    r.set_compositor(Some("KDE".to_string()));
    assert_eq!(r.snapshot().len(), 1);
}

// ── base_config_error ─────────────────────────────────────────────────────────

#[test]
fn base_config_error_none_when_empty_registry() {
    assert!(ConfigRegistry::empty().base_config_error().is_none());
}

#[test]
fn base_config_error_none_when_all_valid() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("konsole", Some("org.kde.konsole"), 0), true),
    ]);
    assert!(r.base_config_error().is_none());
}

#[test]
fn base_config_error_some_when_a_file_fails_to_parse() {
    let r = make_registry(vec![
        broken_entry("Steam Deck"),
        wrap(module("konsole", Some("org.kde.konsole"), 0), true),
    ]);
    let msg = r.base_config_error();
    assert!(msg.is_some_and(|m| m.contains("parse failed")));
}

#[test]
fn base_config_error_ignores_warning_severity() {
    let r = make_registry(vec![
        ConfigEntry {
            name:    "Steam Deck".to_string(),
            config:  None,
            enabled: false,
            errors:  vec![ConfigError { severity: "warning", message: "unknown key".into() }],
        },
    ]);
    assert!(r.base_config_error().is_none());
}

#[test]
fn base_config_error_cleared_after_entry_replaced() {
    // Simulate a reload: broken entry replaced by a valid one.
    let r = make_registry(vec![broken_entry("Steam Deck")]);
    assert!(r.base_config_error().is_some());
    {
        let mut map = r.entries.lock().unwrap();
        map.insert(
            "Steam Deck".to_string(),
            wrap(base("Steam Deck", &["Steam Deck"]), true),
        );
    }
    assert!(r.base_config_error().is_none());
}

// ── Binding conflicts ─────────────────────────────────────────────────────────

fn map_of(configs: Vec<Config>) -> HashMap<String, ConfigEntry> {
    configs.into_iter().map(|c| (c.name.clone(), wrap(c, true))).collect()
}

fn warnings_of(entries: &HashMap<String, ConfigEntry>, name: &str) -> Vec<String> {
    entries[name].errors.iter()
        .filter(|e| e.severity == "warning")
        .map(|e| e.message.clone())
        .collect()
}

#[test]
fn colliding_modules_warn_on_the_losing_config() {
    let mut m = map_of(vec![
        with_binding(module("aaa", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A),
        with_binding(module("bbb", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B),
    ]);
    report_binding_conflicts(&mut m);

    assert!(warnings_of(&m, "aaa").is_empty());
    let bbb = warnings_of(&m, "bbb");
    assert_eq!(bbb.len(), 1);
    assert!(bbb[0].contains("aaa"), "warning must name the config it collides with: {}", bbb[0]);
}

#[test]
fn distinct_bindings_do_not_warn() {
    let mut m = map_of(vec![
        with_binding(module("aaa", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A),
        with_binding(module("bbb", None, 0), evdev::Key::BTN_NORTH, evdev::Key::KEY_B),
    ]);
    report_binding_conflicts(&mut m);
    assert!(warnings_of(&m, "bbb").is_empty());
}

#[test]
fn modules_gated_to_different_compositors_do_not_warn() {
    // KDE and Hyprland modules never load together, so identical bindings in
    // both are the intended translation of one gesture, not a conflict.
    let mut kde = with_binding(module("KDE Desktop", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    kde.module.requires_compositor = Some("KDE".into());
    let mut hypr = with_binding(module("Hyprland Desktop", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);
    hypr.module.requires_compositor = Some("Hyprland".into());

    let mut m = map_of(vec![kde, hypr]);
    report_binding_conflicts(&mut m);
    assert!(warnings_of(&m, "KDE Desktop").is_empty());
    assert!(warnings_of(&m, "Hyprland Desktop").is_empty());
}

#[test]
fn base_and_app_configs_are_exempt_from_conflict_reporting() {
    // Only plain modules stack on top of each other; a base config is expected
    // to override its modules, and an app config only applies to its window.
    let mut m = map_of(vec![
        with_binding(base("Steam Deck", &["Steam Deck"]), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A),
        with_binding(module("konsole", Some("org.kde.konsole"), 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B),
        with_binding(module("zzz", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_C),
    ]);
    report_binding_conflicts(&mut m);
    assert!(warnings_of(&m, "zzz").is_empty());
}

// ── Alias collection ──────────────────────────────────────────────────────────

/// A broken alias must be dropped from the table rather than carried into the
/// binding parser, where it would surface once per use site under the
/// substituted name and never point at the entry that needs fixing.
#[test]
fn collect_aliases_drops_entries_pointing_at_unknown_events() {
    let dir = scratch_dir("deckery_alias_collect_test");
    std::fs::write(dir.join("Steam Deck.toml"), r#"
[device]
class = "hid-steam"
names = ["Steam Deck"]
aliases = { L1 = "BTN_TL", Broken = "BTN_TYPO" }
"#).unwrap();

    let aliases = collect_aliases(&roots_at(&dir));

    assert_eq!(aliases.get("L1").map(String::as_str), Some("BTN_TL"));
    assert!(!aliases.contains_key("Broken"), "alias with unknown target must be dropped");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Aliases come from base configs in the directory root; a module contributes none.
#[test]
fn collect_aliases_reads_every_root_file() {
    let dir = scratch_dir("deckery_alias_collect_root_test");
    std::fs::write(dir.join("Steam Deck.toml"), r#"
[device]
class = "hid-steam"
names = ["Steam Deck"]
aliases = { A = "BTN_SOUTH" }
"#).unwrap();
    std::fs::write(dir.join("kde-desktop.toml"), "[module]\nrequires_compositor = \"KDE\"\n").unwrap();

    let aliases = collect_aliases(&roots_at(&dir));
    assert_eq!(aliases.len(), 1);
    assert_eq!(aliases.get("A").map(String::as_str), Some("BTN_SOUTH"));

    let _ = std::fs::remove_dir_all(&dir);
}

// ── Two-root discovery ────────────────────────────────────────────────────────

fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Roots where only the system dir exists — the shape of a fresh install.
fn roots_at(system: &Path) -> ConfigRoots {
    ConfigRoots { system: system.to_path_buf(), user: system.join("nonexistent-user-root") }
}

const DECK: &str = r#"
[device]
class = "hid-steam"
names = ["Steam Deck"]
"#;

#[test]
fn every_toml_in_both_roots_is_discovered_without_being_listed() {
    let root = scratch_dir("deckery_discovery_test");
    let system = root.join("system");
    let user   = root.join("user");
    std::fs::create_dir_all(system.join("apps")).unwrap();
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(system.join("Steam Deck.toml"), DECK).unwrap();
    std::fs::write(system.join("KDE Desktop.toml"), "[module]\n").unwrap();
    std::fs::write(system.join("apps/Konsole.toml"), "[module]\nmatch_window_class = [\"org.kde.konsole\"]\n").unwrap();
    std::fs::write(user.join("My Module.toml"), "[module]\n").unwrap();

    let entries = ConfigRegistry::load_entries(&ConfigRoots { system, user });
    let mut names: Vec<&String> = entries.keys().collect();
    names.sort();
    assert_eq!(names, vec!["KDE Desktop", "Konsole", "My Module", "Steam Deck"]);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn user_file_replaces_the_system_file_of_the_same_name() {
    let root = scratch_dir("deckery_override_test");
    let system = root.join("system");
    let user   = root.join("user");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(system.join("KDE Desktop.toml"), "[module]\nrequires_compositor = \"KDE\"\n").unwrap();
    std::fs::write(user.join("KDE Desktop.toml"), "[module]\nrequires_compositor = \"Hyprland\"\n").unwrap();

    let entries = ConfigRegistry::load_entries(&ConfigRoots { system, user });
    assert_eq!(entries.len(), 1);
    let config = entries["KDE Desktop"].config.as_ref().unwrap();
    assert_eq!(config.module.requires_compositor.as_deref(), Some("Hyprland"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_user_root_is_not_an_error() {
    let root = scratch_dir("deckery_no_user_root_test");
    std::fs::write(root.join("Steam Deck.toml"), DECK).unwrap();

    let entries = ConfigRegistry::load_entries(&roots_at(&root));
    assert!(entries.contains_key("Steam Deck"));

    let _ = std::fs::remove_dir_all(&root);
}
