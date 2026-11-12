//! Switching configs and groups on and off.

use super::*;

// ── set_enabled / snapshot ────────────────────────────────────────────────────

#[test]
fn set_enabled_returns_true_when_found() {
    // A module, not the base config: switching the device itself off is
    // refused — see a_base_config_cannot_be_disabled.
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("Voice Control", None), true),
    ]);
    assert!(r.set_enabled("Voice Control", false));
}

#[test]
fn set_enabled_returns_false_when_not_found() {
    assert!(!ConfigRegistry::empty().set_enabled("Steam Deck", false));
}

#[test]
fn set_enabled_reflected_in_snapshot() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("Voice Control", None), true),
    ]);
    assert!(r.snapshot().iter().any(|e| e.name == "Voice Control" && e.enabled));
    r.set_enabled("Voice Control", false);
    assert!(r.snapshot().iter().any(|e| e.name == "Voice Control" && !e.enabled));
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
        wrap(module("konsole", Some("org.kde.konsole")), true),
        broken_entry("bad"),
    ]);
    assert_eq!(r.snapshot().len(), 3);
}

#[test]
fn snapshot_reports_kind_per_entry() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("Konsole", Some("org.kde.konsole")), true),
        wrap(module("Voice Control", None), true),
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
        wrap(module("Voice Control", None), true),
        wrap(module("Konsole", Some("org.kde.konsole")), true),
    ]);
    let snap = r.snapshot();
    let parent = |n: &str| snap.iter().find(|e| e.name == n).unwrap().parent.clone();
    assert_eq!(parent("Voice Control"), Some("Steam Deck".to_string()));
    assert_eq!(parent("Steam Deck"),    None);
    assert_eq!(parent("Konsole"),       None);
}

#[test]
fn snapshot_hides_modules_gated_to_another_compositor() {
    let mut hypr = module("Hyprland Desktop", None);
    hypr.module.requires_compositor = Some("Hyprland".to_string());
    let mut kde = module("KDE Desktop", None);
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

// ── Exclusive groups ──────────────────────────────────────────────────────────

/// A module belonging to a set of mutually exclusive modules.
fn grouped(name: &str, group: &str) -> Config {
    let mut c = Config::new_empty(name.to_string());
    c.module.exclusive_group = Some(group.to_string());
    c
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
        wrap(module("Voice Control", None), true),
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
    apply_preferences(&mut fresh, &Preferences::load(&dir), None);

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
        wrap(module("Voice Control", None), true),
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
    apply_preferences(&mut entries, &prefs, None);

    assert!(!entries["Layout Horizontal"].enabled);
    assert!(entries["Layout Vertical"].enabled);
}

/// A member of *group* that only runs under *compositor*.
fn gated(name: &str, group: &str, compositor: &str) -> Config {
    let mut c = grouped(name, group);
    c.module.requires_compositor = Some(compositor.to_string());
    c
}

#[test]
fn a_group_skips_the_member_that_cannot_run_here() {
    // Alphabetically first, but gated to a compositor that is not running.
    // Enabling it anyway gives the group a named winner and no effect —
    // usable() filters it straight back out when the config is resolved.
    let mut entries: HashMap<String, ConfigEntry> = [
        wrap(gated("Layout Grid", "layout", "Hyprland"), true),
        wrap(grouped("Layout Vertical", "layout"), true),
    ].into_iter().map(|e| (e.name.clone(), e)).collect();

    apply_preferences(&mut entries, &Preferences::default(), Some("KDE"));

    assert!(!entries["Layout Grid"].enabled);
    assert!(entries["Layout Vertical"].enabled);
}

#[test]
fn a_recorded_choice_that_cannot_run_here_gives_way() {
    let mut entries: HashMap<String, ConfigEntry> = [
        wrap(gated("Layout Grid", "layout", "Hyprland"), true),
        wrap(grouped("Layout Vertical", "layout"), true),
    ].into_iter().map(|e| (e.name.clone(), e)).collect();

    let mut prefs = Preferences::default();
    prefs.set_group_choice("layout", "Layout Grid", &HashSet::new());
    apply_preferences(&mut entries, &prefs, Some("KDE"));

    assert!(entries["Layout Vertical"].enabled,
            "a member that cannot run must not hold the group hostage");
}

#[test]
fn an_undetected_compositor_rules_nothing_out() {
    // Load time: the compositor is not known yet. Gating is only ever a reason
    // to reject, so not knowing must not reject — set_compositor() revisits it.
    let mut entries: HashMap<String, ConfigEntry> = [
        wrap(gated("Layout Grid", "layout", "Hyprland"), true),
        wrap(grouped("Layout Vertical", "layout"), true),
    ].into_iter().map(|e| (e.name.clone(), e)).collect();

    apply_preferences(&mut entries, &Preferences::default(), None);

    assert!(entries["Layout Grid"].enabled);
}

#[test]
fn a_group_without_a_recorded_choice_activates_its_first_member() {
    let mut entries: HashMap<String, ConfigEntry> = [
        wrap(grouped("Layout Vertical", "layout"), true),
        wrap(grouped("Layout Grid",     "layout"), true),
    ].into_iter().map(|e| (e.name.clone(), e)).collect();

    apply_preferences(&mut entries, &Preferences::default(), None);

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
    apply_preferences(&mut entries, &prefs, None);

    assert!(entries["Layout Horizontal"].enabled);
    assert!(!entries["Layout Vertical"].enabled);
}

#[test]
fn a_disabled_module_stays_disabled_after_reapplying_preferences() {
    let mut entries: HashMap<String, ConfigEntry> = [
        wrap(module("Voice Control", None), true),
    ].into_iter().map(|e| (e.name.clone(), e)).collect();

    let mut prefs = Preferences::default();
    prefs.set_disabled("Voice Control", true);
    apply_preferences(&mut entries, &prefs, None);

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
    let a = with_binding(module("Alpha", None), evdev::Key::BTN_DPAD_UP, evdev::Key::KEY_A);
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

// ── A base config cannot be switched off ──────────────────────────────────────

#[test]
fn a_base_config_cannot_be_disabled() {
    // The tray draws it as a plain row, but the control socket is the other way
    // in. Without this, `config disable "Steam Deck Base"` left resolve() with
    // no answer on any layout.
    let dir = scratch_dir("deckery-base-off");
    let reg = registry_with_user_root(
        vec![wrap(base("Steam Deck Base", &["Steam Deck"]), true)], &dir);

    assert!(!reg.set_enabled("Steam Deck Base", false),
            "disabling a base config must be refused");
    assert_eq!(enabled_names(&reg), vec!["Steam Deck Base"]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_module_can_still_be_disabled() {
    // The guard keys on [device], not on being first in the map.
    let dir = scratch_dir("deckery-module-off");
    let reg = registry_with_user_root(vec![
        wrap(base("Steam Deck Base", &["Steam Deck"]), true),
        wrap(module("Voice Control", None), true),
    ], &dir);

    assert!(reg.set_enabled("Voice Control", false));
    assert_eq!(enabled_names(&reg), vec!["Steam Deck Base"]);

    let _ = std::fs::remove_dir_all(&dir);
}
