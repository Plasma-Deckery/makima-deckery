//! Which configs a device and a window get: device matching, the base
//! configs, window-class modules and compositor gating.

use super::*;

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
    let r = make_registry(vec![wrap(module("konsole", Some("org.kde.konsole")), true)]);
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
        wrap(module("konsole", Some("org.kde.konsole")), true),
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
        wrap(module("konsole", Some("org.kde.konsole")), true),
        wrap(module("layer2", None), true),
    ]);
    let mods = r.window_class_modules();
    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].module.match_window_class.as_deref(), Some(["org.kde.konsole".to_string()].as_slice()));
}

#[test]
fn window_class_modules_excludes_disabled() {
    let r = make_registry(vec![
        wrap(module("konsole", Some("org.kde.konsole")), false),
    ]);
    assert!(r.window_class_modules().is_empty());
}

// ── requires_compositor gating ────────────────────────────────────────────────

#[test]
fn compositor_specific_module_hidden_until_compositor_is_set() {
    let mut m = module("kde-gestures", Some("org.kde.konsole"));
    m.module.requires_compositor = Some("KDE".into());
    let r = make_registry(vec![wrap(m, true)]);
    assert!(r.window_class_modules().is_empty());
}

#[test]
fn compositor_specific_module_visible_on_matching_compositor() {
    let mut m = module("kde-gestures", Some("org.kde.konsole"));
    m.module.requires_compositor = Some("KDE".into());
    let r = make_registry(vec![wrap(m, true)]);
    r.set_compositor(Some("KDE".into()));
    assert_eq!(r.window_class_modules().len(), 1);
}

#[test]
fn compositor_specific_module_hidden_on_other_compositor() {
    let mut m = module("kde-gestures", Some("org.kde.konsole"));
    m.module.requires_compositor = Some("KDE".into());
    let r = make_registry(vec![wrap(m, true)]);
    r.set_compositor(Some("Hyprland".into()));
    assert!(r.window_class_modules().is_empty());
}

#[test]
fn unconditional_module_visible_regardless_of_compositor() {
    let r = make_registry(vec![wrap(module("konsole", Some("org.kde.konsole")), true)]);
    r.set_compositor(Some("Hyprland".into()));
    assert_eq!(r.window_class_modules().len(), 1);
}
