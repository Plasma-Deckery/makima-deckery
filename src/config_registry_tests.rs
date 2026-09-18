use super::*;
use crate::config::{Config, DeviceClass, DeviceDeclaration, Event};
use crate::udev_monitor::Client;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a registry directly from a list of entries (bypasses the filesystem).
///
/// The user root is a throwaway directory rather than an empty path: a test that
/// calls `set_enabled` persists preferences, and an empty root would resolve
/// relative to the process's working directory — i.e. into the source tree.
fn make_registry(entries: Vec<ConfigEntry>) -> Arc<ConfigRegistry> {
    let map = entries.into_iter().map(|e| (e.name.clone(), e)).collect();
    Arc::new(ConfigRegistry {
        roots: ConfigRoots {
            system: PathBuf::new(),
            user:   std::env::temp_dir().join("deckery-test-registry"),
        },
        entries: Mutex::new(map),
        compositor: Mutex::new(None),
        preferences: Mutex::new(crate::preferences::Preferences::default()),
        resolved: Mutex::new(HashMap::new()),
    })
}

fn wrap(config: Config, enabled: bool) -> ConfigEntry {
    ConfigEntry {
        name: config.name.clone(),
        config: Some(config),
        enabled,
        errors: vec![],
        from_user: false,
    }
}

/// Same, but read from the user's config directory rather than the shipped one.
fn wrap_user(config: Config, enabled: bool) -> ConfigEntry {
    ConfigEntry { from_user: true, ..wrap(config, enabled) }
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
        from_user: false,
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
fn a_user_module_wins_over_an_alphabetically_later_shipped_one() {
    // The point of the rule: a small hand-written file patches the shipped set
    // without its author having to find a name that sorts last.
    let shipped = with_binding(module("zzz Shipped", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let mine    = with_binding(module("aaa Mine",    None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);

    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(shipped, true),
        wrap_user(mine, true),
    ]);
    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_B));
}

/// Declare *trigger* a layer modifier of this config, the way the parser does
/// for every combo it reads. The other helpers here build through
/// `Config::new_empty` and leave the set empty, which is precisely why a merge
/// that dropped it went unnoticed for so long.
fn with_modifier(mut c: Config, trigger: evdev::Key) -> Config {
    c.mapped_modifiers.default.push(Event::Key(trigger));
    c.mapped_modifiers.all.push(Event::Key(trigger));
    c
}

#[test]
fn a_user_module_does_not_strip_the_bases_modifiers() {
    // The user's modules merge on top of the base, which means the merge starts
    // from an empty shell — and everything the base config knows has to survive
    // the trip. L1 not surviving it turns every L1-combo into a dead binding,
    // triggered by nothing more than a one-line file in ~/.config/deckery.
    let b = with_modifier(base("Steam Deck Base", &["Steam Deck"]), evdev::Key::BTN_TL);
    let r = make_registry(vec![
        wrap(b, true),
        wrap_user(module("My Tweaks", None, 0), true),
    ]);
    let cfg = r.resolve("Steam Deck Base", &Client::Default, 0).unwrap();
    assert!(cfg.mapped_modifiers.all.contains(&Event::Key(evdev::Key::BTN_TL)),
            "base modifier lost: {:?}", cfg.mapped_modifiers);
}

#[test]
fn a_module_brings_its_own_modifiers_along() {
    // A module is free to introduce a modifier the base config never uses. Its
    // bindings are merged either way, so without the modifier they would load
    // and then never fire.
    let m = with_modifier(module("KDE Desktop", None, 0), evdev::Key::BTN_TR);
    let r = make_registry(vec![
        wrap(base("Steam Deck Base", &["Steam Deck"]), true),
        wrap(m, true),
    ]);
    let cfg = r.resolve("Steam Deck Base", &Client::Default, 0).unwrap();
    assert!(cfg.mapped_modifiers.all.contains(&Event::Key(evdev::Key::BTN_TR)),
            "module modifier lost: {:?}", cfg.mapped_modifiers);
}

#[test]
fn an_app_override_keeps_the_modifiers_of_the_config_below_it() {
    let b = with_modifier(base("Steam Deck Base", &["Steam Deck"]), evdev::Key::BTN_TL);
    let r = make_registry(vec![
        wrap(b, true),
        wrap(module("Firefox", Some("firefox"), 0), true),
    ]);
    let cfg = r.resolve("Steam Deck Base", &class("firefox"), 0).unwrap();
    assert!(cfg.mapped_modifiers.all.contains(&Event::Key(evdev::Key::BTN_TL)),
            "base modifier lost under an app override: {:?}", cfg.mapped_modifiers);
}

#[test]
fn among_user_modules_the_alphabet_still_decides() {
    let first  = with_binding(module("aaa", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let second = with_binding(module("bbb", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);

    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap_user(first, true),
        wrap_user(second, true),
    ]);
    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_B));
}

#[test]
fn a_user_module_outranks_the_shipped_base_config() {
    // The bindings somebody wants to change are declared in the base config, so
    // the root rule would buy nothing if it stopped below it.
    let b    = with_binding(base("Steam Deck", &["Steam Deck"]), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let mine = with_binding(module("Mine", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);

    let r = make_registry(vec![wrap(b, true), wrap_user(mine, true)]);
    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_B));
}

#[test]
fn the_users_own_base_config_outranks_their_modules() {
    // Both files are the user's, so nothing is being worked around any more and
    // the more specific statement — the one naming the device — wins.
    let b    = with_binding(base("Steam Deck", &["Steam Deck"]), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let mine = with_binding(module("Mine", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);

    let r = make_registry(vec![wrap_user(b, true), wrap_user(mine, true)]);
    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));
}

#[test]
fn a_user_module_keeps_what_the_base_config_is() {
    // The user's modules are merged on top of the base, so the result is built
    // from a file that declares no hardware. What it *is* must survive that.
    let mut b = with_binding(base("Steam Deck", &["Steam Deck"]), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    b.aliases.insert("A".into(), "BTN_SOUTH".into());
    let mine = with_binding(module("Mine", None, 0), evdev::Key::BTN_NORTH, evdev::Key::KEY_B);

    let r = make_registry(vec![wrap(b, true), wrap_user(mine, true)]);
    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert_eq!(cfg.name, "Steam Deck");
    assert!(cfg.device.is_some(), "the resolved config must still name its device");
    assert_eq!(cfg.aliases.get("A").map(String::as_str), Some("BTN_SOUTH"));
    // And the base's own bindings are still there where nothing displaced them.
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));
    assert!(has_binding(&cfg, evdev::Key::BTN_NORTH, evdev::Key::KEY_B));
}

#[test]
fn a_user_module_does_not_take_over_gaming_mode() {
    // Same reasoning as for a shipped module: Gaming Mode is device-level, and
    // the base config is its sole authority whoever wrote the module.
    let mut b = base("Steam Deck", &["Steam Deck"]);
    b.gaming_mode_config.auto_detect_steam_games = false;
    let mine = module("Mine", None, 0);

    let r = make_registry(vec![wrap(b, true), wrap_user(mine, true)]);
    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert!(!cfg.gaming_mode_config.auto_detect_steam_games);
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
            from_user: false,
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
fn a_user_module_overriding_a_shipped_one_does_not_warn() {
    // Deliberate, and the only way to change one binding without adopting the
    // whole file it came in — warning about it would train the user to ignore
    // the warnings that do mean something.
    let shipped = with_binding(module("Steam Deck Buttons", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let mine    = with_binding(module("My Tweaks",          None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);
    let mut m: HashMap<String, ConfigEntry> = [
        (shipped.name.clone(), wrap(shipped, true)),
        (mine.name.clone(),    wrap_user(mine, true)),
    ].into_iter().collect();
    report_binding_conflicts(&mut m);

    assert!(warnings_of(&m, "My Tweaks").is_empty());
    assert!(warnings_of(&m, "Steam Deck Buttons").is_empty());
}

#[test]
fn two_user_modules_colliding_still_warn() {
    // Same root, so nothing says which of them the user meant to win.
    let mut m: HashMap<String, ConfigEntry> = [
        with_binding(module("aaa", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A),
        with_binding(module("bbb", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B),
    ].into_iter().map(|c| (c.name.clone(), wrap_user(c, true))).collect();
    report_binding_conflicts(&mut m);

    assert_eq!(warnings_of(&m, "bbb").len(), 1);
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
fn the_root_a_file_came_from_is_recorded() {
    // The one thing a file's location decides — everything else follows from
    // its contents.
    let root = scratch_dir("deckery_from_user_test");
    let system = root.join("system");
    let user   = root.join("user");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::create_dir_all(user.join("apps")).unwrap();
    std::fs::write(system.join("KDE Desktop.toml"), "[module]\n").unwrap();
    std::fs::write(user.join("My Module.toml"), "[module]\n").unwrap();
    std::fs::write(user.join("apps/Konsole.toml"),
                   "[module]\nmatch_window_class = [\"org.kde.konsole\"]\n").unwrap();

    let entries = ConfigRegistry::load_entries(&ConfigRoots { system, user });
    assert!(!entries["KDE Desktop"].from_user);
    assert!(entries["My Module"].from_user);
    // The `apps/` subdirectory belongs to the root above it.
    assert!(entries["Konsole"].from_user);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_user_file_replacing_a_system_one_counts_as_the_users() {
    let root = scratch_dir("deckery_override_root_test");
    let system = root.join("system");
    let user   = root.join("user");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(system.join("KDE Desktop.toml"), "[module]\n").unwrap();
    std::fs::write(user.join("KDE Desktop.toml"), "[module]\n").unwrap();

    let entries = ConfigRegistry::load_entries(&ConfigRoots { system, user });
    assert!(entries["KDE Desktop"].from_user);

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

// ── Exclusive groups ──────────────────────────────────────────────────────────

/// A module belonging to a set of mutually exclusive modules.
fn grouped(name: &str, group: &str) -> Config {
    let mut c = Config::new_empty(name.to_string());
    c.module.exclusive_group = Some(group.to_string());
    c
}

/// A registry whose preferences are written to a real (throwaway) user root, so
/// `set_enabled` can persist without touching the running user's config.
fn registry_with_user_root(entries: Vec<ConfigEntry>, user: &Path) -> Arc<ConfigRegistry> {
    let map = entries.into_iter().map(|e| (e.name.clone(), e)).collect();
    Arc::new(ConfigRegistry {
        roots: ConfigRoots { system: PathBuf::new(), user: user.to_path_buf() },
        entries: Mutex::new(map),
        compositor: Mutex::new(None),
        preferences: Mutex::new(Preferences::default()),
        resolved: Mutex::new(HashMap::new()),
    })
}

fn enabled_names(r: &ConfigRegistry) -> Vec<String> {
    let mut names: Vec<String> = r.entries.lock().unwrap().values()
        .filter(|e| e.enabled)
        .map(|e| e.name.clone())
        .collect();
    names.sort();
    names
}

#[test]
fn enabling_a_group_member_switches_its_siblings_off() {
    let dir = scratch_dir("deckery-prefs-exclusive");
    let r = registry_with_user_root(vec![
        wrap(grouped("Layout Horizontal", "layout"), true),
        wrap(grouped("Layout Vertical",   "layout"), false),
        wrap(grouped("Layout Grid",       "layout"), false),
        wrap(module("Voice Control", None, 0), true),
    ], &dir);

    assert!(r.set_enabled("Layout Vertical", true));

    assert_eq!(enabled_names(&r), vec!["Layout Vertical", "Voice Control"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn switching_the_active_member_off_switches_the_group_off() {
    let dir = scratch_dir("deckery-prefs-no-disable");
    let r = registry_with_user_root(vec![
        wrap(grouped("Layout Horizontal", "layout"), true),
        wrap(grouped("Layout Vertical",   "layout"), false),
    ], &dir);

    assert!(r.set_enabled("Layout Horizontal", false));
    assert!(enabled_names(&r).is_empty());

    // Recorded as a group state, not as two disabled modules: the members are
    // off because the group is, and a later apply_preferences must read it so.
    let written = Preferences::load(&dir);
    assert_eq!(written.groups.disabled, vec!["layout"]);
    assert!(written.modules.disabled.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn switching_an_inactive_member_off_does_nothing() {
    let dir = scratch_dir("deckery-prefs-inactive-off");
    let r = registry_with_user_root(vec![
        wrap(grouped("Layout Horizontal", "layout"), true),
        wrap(grouped("Layout Vertical",   "layout"), false),
    ], &dir);

    // It is already off. Taking the whole group down over a click that said
    // nothing new would be a surprise.
    assert!(r.set_enabled("Layout Vertical", false));
    assert_eq!(enabled_names(&r), vec!["Layout Horizontal"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_disabled_group_stays_off_across_a_reload() {
    let dir = scratch_dir("deckery-prefs-group-off");
    let r = registry_with_user_root(vec![
        wrap(grouped("Layout Horizontal", "layout"), true),
        wrap(grouped("Layout Vertical",   "layout"), false),
    ], &dir);

    assert!(r.set_group_enabled("layout", false));
    assert!(enabled_names(&r).is_empty());

    // What a restart does: fresh entries, all defaulting to enabled, stamped
    // with the file that was just written.
    let mut fresh: HashMap<String, ConfigEntry> = [
        wrap(grouped("Layout Horizontal", "layout"), true),
        wrap(grouped("Layout Vertical",   "layout"), true),
    ].into_iter().map(|e| (e.name.clone(), e)).collect();
    apply_preferences(&mut fresh, &Preferences::load(&dir));

    assert!(fresh.values().all(|e| !e.enabled));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn switching_a_group_back_on_restores_the_remembered_member() {
    let dir = scratch_dir("deckery-prefs-group-back-on");
    let r = registry_with_user_root(vec![
        wrap(grouped("Layout Horizontal", "layout"), true),
        wrap(grouped("Layout Vertical",   "layout"), false),
    ], &dir);

    r.set_enabled("Layout Vertical", true);
    r.set_group_enabled("layout", false);
    assert!(enabled_names(&r).is_empty());

    // Not "Layout Grid"-style alphabetical reset: the choice outlives the off state.
    assert!(r.set_group_enabled("layout", true));
    assert_eq!(enabled_names(&r), vec!["Layout Vertical"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn picking_a_member_switches_a_disabled_group_back_on() {
    let dir = scratch_dir("deckery-prefs-group-revive");
    let r = registry_with_user_root(vec![
        wrap(grouped("Layout Horizontal", "layout"), true),
        wrap(grouped("Layout Vertical",   "layout"), false),
    ], &dir);

    r.set_group_enabled("layout", false);
    assert!(r.set_enabled("Layout Vertical", true));

    assert_eq!(enabled_names(&r), vec!["Layout Vertical"]);
    assert!(Preferences::load(&dir).groups.disabled.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unknown_group_cannot_be_switched() {
    let dir = scratch_dir("deckery-prefs-group-unknown");
    let r = registry_with_user_root(vec![
        wrap(grouped("Layout Horizontal", "layout"), true),
    ], &dir);

    assert!(!r.set_group_enabled("nonexistent", false));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_group_choice_survives_a_reload() {
    let dir = scratch_dir("deckery-prefs-persist");
    let r = registry_with_user_root(vec![
        wrap(grouped("Layout Horizontal", "layout"), true),
        wrap(grouped("Layout Vertical",   "layout"), false),
    ], &dir);

    r.set_enabled("Layout Vertical", true);

    let written = Preferences::load(&dir);
    assert_eq!(written.group_choice("layout"), Some("Layout Vertical"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn disabling_a_module_is_recorded_as_a_preference() {
    let dir = scratch_dir("deckery-prefs-disable");
    let r = registry_with_user_root(vec![
        wrap(module("Voice Control", None, 0), true),
    ], &dir);

    r.set_enabled("Voice Control", false);

    assert_eq!(Preferences::load(&dir).modules.disabled, vec!["Voice Control".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_recorded_choice_is_restored_over_the_loaded_defaults() {
    let mut entries: HashMap<String, ConfigEntry> = [
        wrap(grouped("Layout Horizontal", "layout"), true),
        wrap(grouped("Layout Vertical",   "layout"), true),
    ].into_iter().map(|e| (e.name.clone(), e)).collect();

    let mut prefs = Preferences::default();
    prefs.set_group_choice("layout", "Layout Vertical", &HashSet::new());
    apply_preferences(&mut entries, &prefs);

    assert!(!entries["Layout Horizontal"].enabled);
    assert!(entries["Layout Vertical"].enabled);
}

#[test]
fn a_group_without_a_recorded_choice_activates_its_first_member() {
    let mut entries: HashMap<String, ConfigEntry> = [
        wrap(grouped("Layout Vertical", "layout"), true),
        wrap(grouped("Layout Grid",     "layout"), true),
    ].into_iter().map(|e| (e.name.clone(), e)).collect();

    apply_preferences(&mut entries, &Preferences::default());

    assert!(entries["Layout Grid"].enabled);
    assert!(!entries["Layout Vertical"].enabled);
}

#[test]
fn a_choice_naming_a_removed_module_falls_back_to_the_first_member() {
    let mut entries: HashMap<String, ConfigEntry> = [
        wrap(grouped("Layout Horizontal", "layout"), true),
        wrap(grouped("Layout Vertical",   "layout"), true),
    ].into_iter().map(|e| (e.name.clone(), e)).collect();

    let mut prefs = Preferences::default();
    prefs.set_group_choice("layout", "Layout Diagonal", &HashSet::new());
    apply_preferences(&mut entries, &prefs);

    assert!(entries["Layout Horizontal"].enabled);
    assert!(!entries["Layout Vertical"].enabled);
}

#[test]
fn a_disabled_module_stays_disabled_after_reapplying_preferences() {
    let mut entries: HashMap<String, ConfigEntry> = [
        wrap(module("Voice Control", None, 0), true),
    ].into_iter().map(|e| (e.name.clone(), e)).collect();

    let mut prefs = Preferences::default();
    prefs.set_disabled("Voice Control", true);
    apply_preferences(&mut entries, &prefs);

    assert!(!entries["Voice Control"].enabled);
}

#[test]
fn preferences_toml_is_not_discovered_as_a_config() {
    let root = scratch_dir("deckery-prefs-not-a-config");
    std::fs::write(root.join("Steam Deck.toml"), DECK).unwrap();
    std::fs::write(root.join("preferences.toml"), "[modules]\ndisabled = []\n").unwrap();

    let entries = ConfigRegistry::load_entries(&roots_at(&root));

    assert!(entries.contains_key("Steam Deck"));
    assert!(!entries.contains_key("preferences"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_shipped_preferences_template_is_copied_on_first_start() {
    let system = scratch_dir("deckery-seed-system");
    let user = scratch_dir("deckery-seed-user");
    std::fs::write(
        system.join("preferences.toml"),
        "[exclusive_groups]\nlayout = \"Layout Horizontal\"\n",
    ).unwrap();

    crate::preferences::seed_from(&system, &user);

    assert_eq!(Preferences::load(&user).group_choice("layout"), Some("Layout Horizontal"));
    let _ = std::fs::remove_dir_all(&system);
    let _ = std::fs::remove_dir_all(&user);
}

#[test]
fn seeding_never_overwrites_an_existing_choice() {
    let system = scratch_dir("deckery-seed-keep-system");
    let user = scratch_dir("deckery-seed-keep-user");
    std::fs::write(
        system.join("preferences.toml"),
        "[exclusive_groups]\nlayout = \"Layout Horizontal\"\n",
    ).unwrap();
    // The user has since picked something else. An update ships a new template;
    // their choice has to win, or every update would reset the tray.
    std::fs::write(
        user.join("preferences.toml"),
        "[exclusive_groups]\nlayout = \"Layout Grid\"\n",
    ).unwrap();

    crate::preferences::seed_from(&system, &user);

    assert_eq!(Preferences::load(&user).group_choice("layout"), Some("Layout Grid"));
    let _ = std::fs::remove_dir_all(&system);
    let _ = std::fs::remove_dir_all(&user);
}

#[test]
fn modules_in_the_same_exclusive_group_do_not_warn() {
    let mut a = grouped("Layout Alpha", "layout");
    let mut b = grouped("Layout Bravo", "layout");
    a = with_binding(a, evdev::Key::BTN_DPAD_UP, evdev::Key::KEY_A);
    b = with_binding(b, evdev::Key::BTN_DPAD_UP, evdev::Key::KEY_B);

    let mut entries: HashMap<String, ConfigEntry> =
        [wrap(a, true), wrap(b, true)].into_iter().map(|e| (e.name.clone(), e)).collect();
    report_binding_conflicts(&mut entries);

    assert!(warnings_of(&entries, "Layout Bravo").is_empty());
}

#[test]
fn a_group_member_still_warns_about_an_ungrouped_module() {
    let a = with_binding(module("Alpha", None, 0), evdev::Key::BTN_DPAD_UP, evdev::Key::KEY_A);
    let b = with_binding(grouped("Bravo", "layout"), evdev::Key::BTN_DPAD_UP, evdev::Key::KEY_B);

    let mut entries: HashMap<String, ConfigEntry> =
        [wrap(a, true), wrap(b, true)].into_iter().map(|e| (e.name.clone(), e)).collect();
    report_binding_conflicts(&mut entries);

    assert_eq!(warnings_of(&entries, "Bravo").len(), 1);
}

#[test]
fn a_group_choice_survives_a_restart() {
    let root = scratch_dir("deckery-prefs-roundtrip");
    std::fs::write(root.join("Steam Deck.toml"), DECK).unwrap();
    for name in ["Layout Alpha", "Layout Bravo"] {
        std::fs::write(
            root.join(format!("{name}.toml")),
            "[module]\nexclusive_group = \"layout\"\n",
        ).unwrap();
    }
    // Both roots point at the same directory: preferences are written next to
    // the configs, which is exactly the shape of a real user config dir.
    let roots = ConfigRoots { system: root.clone(), user: root.clone() };

    let first = ConfigRegistry::load(roots.clone());
    assert!(first.set_enabled("Layout Bravo", true));

    let second = ConfigRegistry::load(roots);
    let entries = second.entries.lock().unwrap();
    assert!(entries["Layout Bravo"].enabled);
    assert!(!entries["Layout Alpha"].enabled);
    drop(entries);

    let _ = std::fs::remove_dir_all(&root);
}

// ── Resolve cache ─────────────────────────────────────────────────────────────
//
// resolve() memoises its answer because it runs on every key press. That is only
// safe while every input change drops the memo, so each test here changes one
// input and asserts the *next* resolve sees it. A stale cache would be silent:
// bindings would keep working, just the old ones.

#[test]
fn resolving_twice_returns_the_same_instance() {
    let b = base("Steam Deck", &["Steam Deck"]);
    let m = with_binding(module("gestures", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let r = make_registry(vec![wrap(b, true), wrap(m, true)]);

    let first  = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    let second = r.resolve("Steam Deck", &Client::Default, 0).unwrap();

    // Same allocation, not merely equal — the second call did no work.
    assert!(Arc::ptr_eq(&first, &second));
}

#[test]
fn a_toggle_is_visible_to_the_next_resolve() {
    let dir = scratch_dir("deckery-cache-toggle");
    let b = base("Steam Deck", &["Steam Deck"]);
    let m = with_binding(module("gestures", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let r = registry_with_user_root(vec![wrap(b, true), wrap(m, true)], &dir);

    assert!(has_binding(&r.resolve("Steam Deck", &Client::Default, 0).unwrap(),
                        evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));

    assert!(r.set_enabled("gestures", false));

    assert!(!has_binding(&r.resolve("Steam Deck", &Client::Default, 0).unwrap(),
                         evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_compositor_change_is_visible_to_the_next_resolve() {
    let b = base("Steam Deck", &["Steam Deck"]);
    let mut m = with_binding(module("kde only", None, 0), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    m.module.requires_compositor = Some("KDE".to_string());
    let r = make_registry(vec![wrap(b, true), wrap(m, true)]);

    r.set_compositor(Some("Hyprland".to_string()));
    assert!(!has_binding(&r.resolve("Steam Deck", &Client::Default, 0).unwrap(),
                         evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));

    r.set_compositor(Some("KDE".to_string()));
    assert!(has_binding(&r.resolve("Steam Deck", &Client::Default, 0).unwrap(),
                        evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));
}

#[test]
fn the_cache_distinguishes_window_class_and_layout() {
    let b = with_binding(base("Steam Deck", &["Steam Deck"]), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);
    let app = with_binding(module("konsole", Some("org.kde.konsole"), 0),
                           evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let r = make_registry(vec![wrap(b, true), wrap(app, true)]);

    // Same base, same layout — only the focused window differs. Keying the cache
    // on the base alone would serve the app override to every other window.
    let focused = r.resolve("Steam Deck", &class("org.kde.konsole"), 0).unwrap();
    let plain   = r.resolve("Steam Deck", &Client::Default, 0).unwrap();

    assert!(has_binding(&focused, evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));
    assert!(has_binding(&plain,   evdev::Key::BTN_SOUTH, evdev::Key::KEY_B));
}

#[test]
fn a_reload_is_visible_to_the_next_resolve() {
    // The other invalidation tests drive the registry through its API. This one
    // goes through the disk, because reload() is the path SIGHUP and the file
    // watcher take — the two triggers that come from outside the process.
    let root = scratch_dir("deckery-cache-reload");
    let system = root.join("system");
    let user   = root.join("user");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(system.join("Steam Deck.toml"), DECK).unwrap();
    std::fs::write(system.join("gestures.toml"),
        "[module]\n\n[remap]\nBTN_SOUTH = [\"KEY_A\"]\n").unwrap();

    let r = ConfigRegistry::load(ConfigRoots { system: system.clone(), user });
    assert!(has_binding(&r.resolve("Steam Deck", &Client::Default, 0).unwrap(),
                        evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));

    std::fs::write(system.join("gestures.toml"),
        "[module]\n\n[remap]\nBTN_SOUTH = [\"KEY_B\"]\n").unwrap();
    r.reload();

    let cfg = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_B));
    assert!(!has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unclaimed_window_classes_share_one_cache_entry() {
    let b = base("Steam Deck", &["Steam Deck"]);
    let app = module("konsole", Some("org.kde.konsole"), 0);
    let r = make_registry(vec![wrap(b, true), wrap(app, true)]);

    // Neither class is claimed by a module, so both resolve to the same config
    // as no focused window at all — and must not each occupy the cache.
    let plain   = r.resolve("Steam Deck", &Client::Default, 0).unwrap();
    let firefox = r.resolve("Steam Deck", &class("firefox"), 0).unwrap();
    let mail    = r.resolve("Steam Deck", &class("thunderbird"), 0).unwrap();
    assert!(Arc::ptr_eq(&plain, &firefox));
    assert!(Arc::ptr_eq(&plain, &mail));

    // A claimed class keeps its own entry — folding it in would hand Konsole's
    // override to every other window.
    let konsole = r.resolve("Steam Deck", &class("org.kde.konsole"), 0).unwrap();
    assert!(!Arc::ptr_eq(&plain, &konsole));

    assert_eq!(r.resolved.lock().unwrap().len(), 2);
}

// ── User-directory README ─────────────────────────────────────────────────────

fn readme_roots(name: &str) -> (PathBuf, ConfigRoots) {
    let root   = scratch_dir(name);
    let system = root.join("system");
    let user   = root.join("user");
    std::fs::create_dir_all(&system).unwrap();
    (root, ConfigRoots { system, user })
}

#[test]
fn readme_is_copied_into_a_user_root_that_does_not_exist_yet() {
    let (root, roots) = readme_roots("deckery_readme_fresh");
    std::fs::write(roots.system.join(README_NAME), "the rules").unwrap();

    refresh_readme(&roots);

    assert_eq!(std::fs::read_to_string(roots.user.join(README_NAME)).unwrap(), "the rules");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn readme_is_overwritten_when_it_went_stale() {
    // The whole point of copying it on every start rather than seeding it once:
    // a description of the override rules has to describe the current ones.
    let (root, roots) = readme_roots("deckery_readme_stale");
    std::fs::create_dir_all(&roots.user).unwrap();
    std::fs::write(roots.system.join(README_NAME), "the new rules").unwrap();
    std::fs::write(roots.user.join(README_NAME), "last year's rules").unwrap();

    refresh_readme(&roots);

    assert_eq!(std::fs::read_to_string(roots.user.join(README_NAME)).unwrap(), "the new rules");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_unchanged_readme_is_not_rewritten() {
    // Rewriting identical bytes would bump the mtime on every single start, for
    // every watcher looking at that directory, to change nothing.
    let (root, roots) = readme_roots("deckery_readme_unchanged");
    std::fs::create_dir_all(&roots.user).unwrap();
    std::fs::write(roots.system.join(README_NAME), "the rules").unwrap();
    let target = roots.user.join(README_NAME);
    std::fs::write(&target, "the rules").unwrap();
    let before = std::fs::metadata(&target).unwrap().modified().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(20));
    refresh_readme(&roots);

    assert_eq!(std::fs::metadata(&target).unwrap().modified().unwrap(), before);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_missing_shipped_readme_is_not_an_error() {
    // Running against a config tree that predates the README, or a test root.
    let (root, roots) = readme_roots("deckery_readme_absent");

    refresh_readme(&roots);

    assert!(!roots.user.join(README_NAME).exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_readme_is_not_mistaken_for_a_config() {
    // It lives in the same directory as the configs and must stay invisible to
    // the scan — the tray would otherwise show it as a broken entry.
    let root = scratch_dir("deckery_readme_not_a_config");
    let system = root.join("system");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::write(system.join("Steam Deck.toml"), DECK).unwrap();
    std::fs::write(system.join(README_NAME), "# not a config").unwrap();

    let entries = ConfigRegistry::load_entries(&roots_at(&system));
    let names: Vec<&String> = entries.keys().collect();
    assert_eq!(names, vec!["Steam Deck"]);

    let _ = std::fs::remove_dir_all(&root);
}
