//! `resolve` and its cache.

use super::*;

// ── resolve ───────────────────────────────────────────────────────────────────

#[test]
fn resolve_returns_none_without_base() {
    let r = make_registry(vec![wrap(module("konsole", Some("org.kde.konsole")), true)]);
    assert!(r.resolve("Steam Deck", &class("org.kde.konsole")).is_none());
}

#[test]
fn resolve_returns_base_when_no_module_matches() {
    let r = make_registry(vec![wrap(base("Steam Deck", &["Steam Deck"]), true)]);
    let cfg = r.resolve("Steam Deck", &class("org.kde.konsole")).unwrap();
    assert_eq!(cfg.name, "Steam Deck");
}

#[test]
fn resolve_applies_window_class_module() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("konsole", Some("org.kde.konsole")), true),
    ]);
    let cfg = r.resolve("Steam Deck", &class("org.kde.konsole")).unwrap();
    assert_eq!(cfg.name, "konsole");
}

#[test]
fn resolve_ignores_module_for_other_window_class() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("konsole", Some("org.kde.konsole")), true),
    ]);
    let cfg = r.resolve("Steam Deck", &class("firefox")).unwrap();
    assert_eq!(cfg.name, "Steam Deck");
}

#[test]
fn resolve_window_class_module_wins_over_layout_only_module() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("layer1", None), true),
        wrap(module("konsole-layer1", Some("org.kde.konsole")), true),
    ]);
    let cfg = r.resolve("Steam Deck", &class("org.kde.konsole")).unwrap();
    assert_eq!(cfg.name, "konsole-layer1");
}

#[test]
fn resolve_disabled_base_returns_none() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), false),
        wrap(module("konsole", Some("org.kde.konsole")), true),
    ]);
    assert!(r.resolve("Steam Deck", &class("org.kde.konsole")).is_none());
}

#[test]
fn resolve_disabled_module_falls_back_to_base() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("konsole", Some("org.kde.konsole")), false),
    ]);
    let cfg = r.resolve("Steam Deck", &class("org.kde.konsole")).unwrap();
    assert_eq!(cfg.name, "Steam Deck");
}

#[test]
fn resolve_broken_module_falls_back_to_base() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        broken_entry("konsole"),
    ]);
    let cfg = r.resolve("Steam Deck", &class("org.kde.konsole")).unwrap();
    assert_eq!(cfg.name, "Steam Deck");
}

#[test]
fn resolve_unknown_base_name_returns_none() {
    let r = make_registry(vec![wrap(base("Steam Deck", &["Steam Deck"]), true)]);
    assert!(r.resolve("Xbox Controller", &Client::Default).is_none());
}

#[test]
fn resolve_keys_on_config_name_not_device_declaration() {
    // The config is named "deck"; its [device] names share nothing with it.
    // launch_tasks() already did the device matching, so resolve() must find
    // the config by name alone.
    let r = make_registry(vec![wrap(base("deck", &["Valve Software Steam Controller"]), true)]);
    assert_eq!(r.resolve("deck", &Client::Default).unwrap().name, "deck");
    // The kernel name is not a registry key and must not resolve.
    assert!(r.resolve("Valve Software Steam Controller", &Client::Default).is_none());
}

#[test]
fn resolve_module_name_is_not_a_base_config() {
    // Only entries with a [device] section can anchor a resolve.
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("konsole", Some("org.kde.konsole")), true),
    ]);
    assert!(r.resolve("konsole", &Client::Default).is_none());
}

#[test]
fn resolve_module_gated_by_compositor_does_not_apply() {
    let mut m = module("konsole", Some("org.kde.konsole"));
    m.module.requires_compositor = Some("KDE".into());
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(m, true),
    ]);
    r.set_compositor(Some("Hyprland".into()));
    let cfg = r.resolve("Steam Deck", &class("org.kde.konsole")).unwrap();
    assert_eq!(cfg.name, "Steam Deck");
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
    let m = with_binding(module("gestures", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let r = make_registry(vec![wrap(b, true), wrap(m, true)]);

    let first  = r.resolve("Steam Deck", &Client::Default).unwrap();
    let second = r.resolve("Steam Deck", &Client::Default).unwrap();

    // Same allocation, not merely equal — the second call did no work.
    assert!(Arc::ptr_eq(&first, &second));
}

#[test]
fn a_toggle_is_visible_to_the_next_resolve() {
    let dir = scratch_dir("deckery-cache-toggle");
    let b = base("Steam Deck", &["Steam Deck"]);
    let m = with_binding(module("gestures", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let r = registry_with_user_root(vec![wrap(b, true), wrap(m, true)], &dir);

    assert!(has_binding(&r.resolve("Steam Deck", &Client::Default).unwrap(),
                        evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));

    assert!(r.set_enabled("gestures", false));

    assert!(!has_binding(&r.resolve("Steam Deck", &Client::Default).unwrap(),
                         evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_compositor_change_is_visible_to_the_next_resolve() {
    let b = base("Steam Deck", &["Steam Deck"]);
    let mut m = with_binding(module("kde only", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    m.module.requires_compositor = Some("KDE".to_string());
    let r = make_registry(vec![wrap(b, true), wrap(m, true)]);

    r.set_compositor(Some("Hyprland".to_string()));
    assert!(!has_binding(&r.resolve("Steam Deck", &Client::Default).unwrap(),
                         evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));

    r.set_compositor(Some("KDE".to_string()));
    assert!(has_binding(&r.resolve("Steam Deck", &Client::Default).unwrap(),
                        evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));
}

#[test]
fn the_cache_distinguishes_window_class_and_layout() {
    let b = with_binding(base("Steam Deck", &["Steam Deck"]), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);
    let app = with_binding(module("konsole", Some("org.kde.konsole")),
                           evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let r = make_registry(vec![wrap(b, true), wrap(app, true)]);

    // Same base, same layout — only the focused window differs. Keying the cache
    // on the base alone would serve the app override to every other window.
    let focused = r.resolve("Steam Deck", &class("org.kde.konsole")).unwrap();
    let plain   = r.resolve("Steam Deck", &Client::Default).unwrap();

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
    assert!(has_binding(&r.resolve("Steam Deck", &Client::Default).unwrap(),
                        evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));

    std::fs::write(system.join("gestures.toml"),
        "[module]\n\n[remap]\nBTN_SOUTH = [\"KEY_B\"]\n").unwrap();
    r.reload();

    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_B));
    assert!(!has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unclaimed_window_classes_share_one_cache_entry() {
    let b = base("Steam Deck", &["Steam Deck"]);
    let app = module("konsole", Some("org.kde.konsole"));
    let r = make_registry(vec![wrap(b, true), wrap(app, true)]);

    // Neither class is claimed by a module, so both resolve to the same config
    // as no focused window at all — and must not each occupy the cache.
    let plain   = r.resolve("Steam Deck", &Client::Default).unwrap();
    let firefox = r.resolve("Steam Deck", &class("firefox")).unwrap();
    let mail    = r.resolve("Steam Deck", &class("thunderbird")).unwrap();
    assert!(Arc::ptr_eq(&plain, &firefox));
    assert!(Arc::ptr_eq(&plain, &mail));

    // A claimed class keeps its own entry — folding it in would hand Konsole's
    // override to every other window.
    let konsole = r.resolve("Steam Deck", &class("org.kde.konsole")).unwrap();
    assert!(!Arc::ptr_eq(&plain, &konsole));

    assert_eq!(r.resolved.lock().unwrap().len(), 2);
}
