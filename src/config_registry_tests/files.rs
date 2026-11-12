//! Loading from disk: the two roots, errors in a file, the user-directory
//! README, and the watcher.

use super::*;

// ── base_config_error ─────────────────────────────────────────────────────────

#[test]
fn base_config_error_none_when_empty_registry() {
    assert!(ConfigRegistry::empty().base_config_error().is_none());
}

#[test]
fn base_config_error_none_when_all_valid() {
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(module("konsole", Some("org.kde.konsole")), true),
    ]);
    assert!(r.base_config_error().is_none());
}

#[test]
fn base_config_error_some_when_a_file_fails_to_parse() {
    let r = make_registry(vec![
        broken_entry("Steam Deck"),
        wrap(module("konsole", Some("org.kde.konsole")), true),
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

// ── Two-root discovery ────────────────────────────────────────────────────────




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

// ── A broken override falls back to the shipped config ────────────────────────

/// Two roots with the same module in each, the user's copy deliberately broken.
fn roots_with_a_broken_override(name: &str, user_body: &str) -> (PathBuf, ConfigRoots) {
    let root   = scratch_dir(name);
    let system = root.join("system");
    let user   = root.join("user");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(system.join("Steam Deck Base.toml"), DECK).unwrap();
    std::fs::write(system.join("KDE Desktop.toml"),
                   "[remap]\nBTN_TL = [\"KEY_LEFTCTRL\"]\n").unwrap();
    std::fs::write(user.join("KDE Desktop.toml"), user_body).unwrap();
    (root, ConfigRoots { system, user })
}

#[test]
fn a_broken_user_override_falls_back_to_the_shipped_config() {
    // Replacing a shipped config is what a same-named user file is for, so a
    // typo in one used to delete the shipped bindings outright.
    let (root, roots) = roots_with_a_broken_override(
        "deckery-fallback-parse", "[remap\nthis is not toml\n");

    let entries = ConfigRegistry::load_entries(&roots);
    let entry = entries.get("KDE Desktop").expect("the config must still exist");

    assert!(entry.config.is_some(), "the shipped config must stand in");
    assert!(has_binding(entry.config.as_ref().unwrap(),
                        evdev::Key::BTN_TL, evdev::Key::KEY_LEFTCTRL),
            "the shipped bindings must be the ones in effect");
    assert!(entry.enabled, "a config that loaded is not switched off by the fallback");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_fallback_warns_instead_of_erroring() {
    // An error would light the tray red and read as "this module is broken".
    // It is not: it runs, just not in the user's version of it.
    let (root, roots) = roots_with_a_broken_override(
        "deckery-fallback-warns", "[remap\nthis is not toml\n");

    let entries = ConfigRegistry::load_entries(&roots);
    let errors = &entries["KDE Desktop"].errors;

    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].severity, "warning");
    assert!(errors[0].message.contains("the version Deckery ships is in use"),
            "the message must say which file is running: {}", errors[0].message);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_fallback_config_is_layered_as_a_shipped_one() {
    // `from_user` is the first sort key of the merge. What is in effect here is
    // the shipped config, so it must not keep the user's rank.
    let (root, roots) = roots_with_a_broken_override(
        "deckery-fallback-rank", "[remap\nthis is not toml\n");

    let entries = ConfigRegistry::load_entries(&roots);
    assert!(!entries["KDE Desktop"].from_user);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_broken_user_file_with_no_shipped_twin_still_errors() {
    // Nothing to fall back to. Reporting it as a warning would claim a
    // replacement that does not exist.
    let root   = scratch_dir("deckery-fallback-none");
    let system = root.join("system");
    let user   = root.join("user");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(system.join("Steam Deck Base.toml"), DECK).unwrap();
    std::fs::write(user.join("My Tweaks.toml"), "[remap\nbroken\n").unwrap();

    let entries = ConfigRegistry::load_entries(&ConfigRoots { system, user });
    let entry = &entries["My Tweaks"];

    assert!(entry.config.is_none());
    assert_eq!(entry.errors[0].severity, "error");

    let _ = std::fs::remove_dir_all(&root);
}

// ── Unknown keys in the fixed sections ────────────────────────────────────────

#[test]
fn a_misspelled_module_key_is_rejected_rather_than_merged_everywhere() {
    // `match_window_classes` used to be dropped silently, which turned an app
    // override into a plain module applied to every window.
    let root   = scratch_dir("deckery-deny-module");
    let system = root.join("system");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::write(system.join("Steam Deck Base.toml"), DECK).unwrap();
    std::fs::write(system.join("Firefox.toml"),
                   "[module]\nmatch_window_classes = [\"firefox\"]\n").unwrap();

    let entries = ConfigRegistry::load_entries(&roots_at(&system));
    let entry = &entries["Firefox"];

    assert!(entry.config.is_none(), "the file must not load at all");
    assert_eq!(entry.errors[0].severity, "error");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_misspelled_section_name_is_rejected() {
    // `[remaps]` parses as valid TOML and used to be ignored wholesale.
    let root   = scratch_dir("deckery-deny-section");
    let system = root.join("system");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::write(system.join("Steam Deck Base.toml"), DECK).unwrap();
    std::fs::write(system.join("Typo.toml"), "[remaps]\nBTN_TL = [\"KEY_A\"]\n").unwrap();

    let entries = ConfigRegistry::load_entries(&roots_at(&system));

    assert!(entries["Typo"].config.is_none());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn button_names_are_still_free_form() {
    // The guard must not reach into the binding maps: their keys are button
    // names, and there is no list of legal ones to check against.
    let root   = scratch_dir("deckery-deny-bindings");
    let system = root.join("system");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::write(system.join("Steam Deck Base.toml"), DECK).unwrap();
    std::fs::write(system.join("Odd.toml"),
                   "[remap]\nBTN_TL-BTN_SOUTH = [\"KEY_A\"]\n").unwrap();

    let entries = ConfigRegistry::load_entries(&roots_at(&system));

    assert!(entries["Odd"].config.is_some(), "a combo key must still parse");

    let _ = std::fs::remove_dir_all(&root);
}

// ── Watcher filter ────────────────────────────────────────────────────────────
//
// The file watcher is the trigger of issue #42's item 5 — an edit that it
// does not report is an edit that never takes effect, however correct the
// reload behind it is.

fn fs_event(kind: notify::EventKind, path: &str) -> notify::Event {
    notify::Event::new(kind).add_path(PathBuf::from(path))
}

fn modify() -> notify::EventKind {
    notify::EventKind::Modify(notify::event::ModifyKind::Any)
}

#[test]
fn editing_a_toml_is_a_config_change() {
    assert!(ConfigRegistry::is_config_change(&fs_event(modify(), "/etc/deckery/Steam Deck.toml")));
}

#[test]
fn creating_or_removing_a_toml_is_a_config_change() {
    // Dropping a module into apps/ or deleting one changes what is loaded
    // just as much as editing it does.
    use notify::event::{CreateKind, RemoveKind};
    assert!(ConfigRegistry::is_config_change(&fs_event(
        notify::EventKind::Create(CreateKind::File), "/home/u/.config/deckery/apps/game.toml")));
    assert!(ConfigRegistry::is_config_change(&fs_event(
        notify::EventKind::Remove(RemoveKind::File), "/home/u/.config/deckery/apps/game.toml")));
}

#[test]
fn preferences_toml_is_not_a_config_change() {
    // Written by makima itself on every tray toggle; reacting to it would
    // reload everything to re-read what was just written.
    assert!(!ConfigRegistry::is_config_change(&fs_event(modify(), "/home/u/.config/deckery/preferences.toml")));
}

#[test]
fn other_files_are_not_a_config_change() {
    // Editor swap files and backups land next to the configs all the time.
    for path in ["/etc/deckery/README.md", "/etc/deckery/.Steam Deck.toml.swp",
                 "/etc/deckery/Steam Deck.toml~", "/etc/deckery/apps"] {
        assert!(!ConfigRegistry::is_config_change(&fs_event(modify(), path)), "{path}");
    }
}

#[test]
fn reading_a_toml_is_not_a_config_change() {
    // Access events fire when makima itself loads the file on reload; treating
    // them as changes would make every reload trigger the next one.
    let access = notify::EventKind::Access(notify::event::AccessKind::Read);
    assert!(!ConfigRegistry::is_config_change(&fs_event(access, "/etc/deckery/Steam Deck.toml")));
}

#[test]
fn one_toml_among_several_paths_is_enough() {
    // A rename event carries both the old and the new name.
    let event = notify::Event::new(modify())
        .add_path(PathBuf::from("/etc/deckery/.Steam Deck.toml.tmp"))
        .add_path(PathBuf::from("/etc/deckery/Steam Deck.toml"));
    assert!(ConfigRegistry::is_config_change(&event));
}

#[tokio::test]
async fn writing_a_config_file_signals_the_watcher() {
    // One test through real inotify, so the filter above is known to be wired
    // to the watcher over the directories the registry actually reads.
    let root = scratch_dir("deckery-watcher-signal");
    let system = root.join("system");
    std::fs::create_dir_all(system.join("apps")).unwrap();
    let registry = ConfigRegistry::load(roots_at(&system));
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let _watcher = registry.start_watcher(tx);

    std::fs::write(system.join("apps").join("game.toml"), "[module]\n").unwrap();

    let signalled = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await;
    assert!(matches!(signalled, Ok(Some(()))), "expected a change signal, got: {:?}", signalled);

    let _ = std::fs::remove_dir_all(&root);
}
