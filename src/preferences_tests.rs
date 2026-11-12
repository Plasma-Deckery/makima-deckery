use super::*;

// preferences.toml is the one file the user cannot reconstruct from what is on
// disk: it records clicks, not configuration. These tests pin that every click
// survives a save/load cycle, and that a missing or broken file degrades to
// "everything on" instead of taking the input stack down.

fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn set(names: &[&str]) -> HashSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

// ── Loading ───────────────────────────────────────────────────────────────────

#[test]
fn missing_file_loads_defaults() {
    let dir = scratch_dir("deckery-prefs-missing");
    let prefs = Preferences::load(&dir);
    assert_eq!(prefs, Preferences::default());
    assert!(!prefs.is_disabled("Voice Control"));
    assert_eq!(prefs.group_choice("layout"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_root_directory_loads_defaults() {
    // A fresh install has no user root yet; that is not an error.
    let dir = std::env::temp_dir().join("deckery-prefs-no-root-does-not-exist");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(Preferences::load(&dir), Preferences::default());
}

#[test]
fn empty_file_loads_defaults() {
    let dir = scratch_dir("deckery-prefs-empty");
    std::fs::write(path_in(&dir), "").unwrap();
    assert_eq!(Preferences::load(&dir), Preferences::default());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn documented_example_file_loads() {
    // The layout shown in the module comment is what the shipped template and
    // users' hand edits look like.
    let dir = scratch_dir("deckery-prefs-example");
    std::fs::write(path_in(&dir), "\
[exclusive_groups]
kde-desktop-layout = \"KDE Desktop Layout Horizontal\"

[groups]
disabled = [\"voice-control-language\"]

[modules]
disabled = [\"Voice Control\"]
").unwrap();
    let prefs = Preferences::load(&dir);
    assert_eq!(prefs.group_choice("kde-desktop-layout"), Some("KDE Desktop Layout Horizontal"));
    assert!(prefs.is_group_disabled("voice-control-language"));
    assert!(prefs.is_disabled("Voice Control"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn partial_file_fills_in_missing_tables() {
    // Only deviations are recorded, so a file with just one table is normal.
    let dir = scratch_dir("deckery-prefs-partial");
    std::fs::write(path_in(&dir), "[modules]\ndisabled = [\"Voice Control\"]\n").unwrap();
    let prefs = Preferences::load(&dir);
    assert!(prefs.is_disabled("Voice Control"));
    assert!(prefs.exclusive_groups.is_empty());
    assert!(prefs.groups.disabled.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_keys_are_ignored() {
    // A file written by a newer version must still be readable by an older one
    // instead of being thrown away wholesale.
    let dir = scratch_dir("deckery-prefs-unknown");
    std::fs::write(path_in(&dir), "\
future_setting = 1

[modules]
disabled = [\"Voice Control\"]
also_new = true

[something_else]
x = 1
").unwrap();
    let prefs = Preferences::load(&dir);
    assert!(prefs.is_disabled("Voice Control"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn malformed_toml_loads_defaults() {
    let dir = scratch_dir("deckery-prefs-malformed");
    std::fs::write(path_in(&dir), "[modules\ndisabled = [\"Voice Control\"").unwrap();
    assert_eq!(Preferences::load(&dir), Preferences::default());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wrong_value_type_loads_defaults() {
    // Valid TOML but the wrong shape counts as malformed too: the whole file is
    // ignored, not just the bad key.
    let dir = scratch_dir("deckery-prefs-wrong-type");
    std::fs::write(path_in(&dir), "\
[exclusive_groups]
layout = \"Layout Vertical\"

[modules]
disabled = \"Voice Control\"
").unwrap();
    assert_eq!(Preferences::load(&dir), Preferences::default());
    let _ = std::fs::remove_dir_all(&dir);
}

// ── Saving ────────────────────────────────────────────────────────────────────

#[test]
fn a_disabled_module_stays_disabled_across_a_round_trip() {
    let dir = scratch_dir("deckery-prefs-roundtrip-module");
    let mut prefs = Preferences::default();
    prefs.set_disabled("Voice Control", true);
    prefs.save(&dir);

    let loaded = Preferences::load(&dir);
    assert!(loaded.is_disabled("Voice Control"));
    assert!(!loaded.is_disabled("KDE Desktop"));
    assert_eq!(loaded, prefs);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exclusive_group_choice_survives_a_round_trip() {
    let dir = scratch_dir("deckery-prefs-roundtrip-group");
    let mut prefs = Preferences::default();
    prefs.set_group_choice("layout", "Layout Vertical", &set(&["Layout Horizontal", "Layout Vertical"]));
    prefs.save(&dir);

    assert_eq!(Preferences::load(&dir).group_choice("layout"), Some("Layout Vertical"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_disabled_group_remembers_its_member_across_a_round_trip() {
    // Switching a group off must not forget the pick; otherwise switching it
    // back on would reset to the alphabetically first member.
    let dir = scratch_dir("deckery-prefs-roundtrip-group-off");
    let mut prefs = Preferences::default();
    prefs.set_group_choice("layout", "Layout Vertical", &set(&["Layout Horizontal", "Layout Vertical"]));
    prefs.set_group_disabled("layout", true);
    prefs.save(&dir);

    let loaded = Preferences::load(&dir);
    assert!(loaded.is_group_disabled("layout"));
    assert_eq!(loaded.group_choice("layout"), Some("Layout Vertical"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_creates_the_user_root() {
    let dir = std::env::temp_dir().join("deckery-prefs-create-root").join("nested");
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    let mut prefs = Preferences::default();
    prefs.set_disabled("Voice Control", true);
    prefs.save(&dir);
    assert!(Preferences::load(&dir).is_disabled("Voice Control"));
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

#[test]
fn save_leaves_no_temporary_file_behind() {
    // The sibling .tmp would otherwise be picked up next to the configs.
    let dir = scratch_dir("deckery-prefs-no-tmp");
    Preferences::default().save(&dir);
    let names: Vec<String> = std::fs::read_dir(&dir).unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec![FILE_NAME.to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_replaces_a_malformed_file() {
    // The documented recovery path: a broken file is ignored on load and the
    // next write puts something valid in its place.
    let dir = scratch_dir("deckery-prefs-repair");
    std::fs::write(path_in(&dir), "not = [valid").unwrap();
    let mut prefs = Preferences::load(&dir);
    prefs.set_disabled("Voice Control", true);
    prefs.save(&dir);
    assert!(Preferences::load(&dir).is_disabled("Voice Control"));
    let _ = std::fs::remove_dir_all(&dir);
}

// ── In-memory transitions ─────────────────────────────────────────────────────

#[test]
fn set_disabled_does_not_duplicate_and_stays_sorted() {
    // Sorted output keeps the file diff-friendly and stable between saves.
    let mut prefs = Preferences::default();
    prefs.set_disabled("Zeta", true);
    prefs.set_disabled("Alpha", true);
    prefs.set_disabled("Zeta", true);
    assert_eq!(prefs.modules.disabled, vec!["Alpha", "Zeta"]);
}

#[test]
fn re_enabling_a_module_removes_it() {
    // Only deviations are recorded; an enabled module has no entry at all.
    let mut prefs = Preferences::default();
    prefs.set_disabled("Voice Control", true);
    prefs.set_disabled("Voice Control", false);
    assert!(prefs.modules.disabled.is_empty());
    prefs.set_disabled("Never Disabled", false);
    assert!(prefs.modules.disabled.is_empty());
}

#[test]
fn picking_a_member_clears_stale_sibling_disables() {
    // Siblings are tracked through the group entry only. A leftover module
    // disable from before the module joined the group would otherwise keep the
    // chosen member off.
    let mut prefs = Preferences::default();
    prefs.set_disabled("Layout Vertical", true);
    prefs.set_disabled("Voice Control", true);
    prefs.set_group_choice("layout", "Layout Vertical", &set(&["Layout Horizontal", "Layout Vertical"]));
    assert_eq!(prefs.modules.disabled, vec!["Voice Control"]);
}

#[test]
fn picking_a_member_switches_the_group_back_on() {
    let mut prefs = Preferences::default();
    prefs.set_group_disabled("layout", true);
    prefs.set_group_choice("layout", "Layout Horizontal", &set(&["Layout Horizontal"]));
    assert!(!prefs.is_group_disabled("layout"));
}

#[test]
fn a_new_pick_replaces_the_old_one() {
    let mut prefs = Preferences::default();
    let members = set(&["Layout Horizontal", "Layout Vertical"]);
    prefs.set_group_choice("layout", "Layout Horizontal", &members);
    prefs.set_group_choice("layout", "Layout Vertical", &members);
    assert_eq!(prefs.group_choice("layout"), Some("Layout Vertical"));
    assert_eq!(prefs.exclusive_groups.len(), 1);
}

#[test]
fn group_disable_is_independent_per_group() {
    let mut prefs = Preferences::default();
    prefs.set_group_disabled("b", true);
    prefs.set_group_disabled("a", true);
    prefs.set_group_disabled("b", true);
    assert_eq!(prefs.groups.disabled, vec!["a", "b"]);
    prefs.set_group_disabled("a", false);
    assert!(!prefs.is_group_disabled("a"));
    assert!(prefs.is_group_disabled("b"));
}

// ── Seeding from the shipped template ─────────────────────────────────────────

#[test]
fn seed_copies_the_template_when_the_user_has_none() {
    let root = scratch_dir("deckery-prefs-seed");
    let system = root.join("system");
    let user = root.join("user");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::write(path_in(&system), "[exclusive_groups]\nlayout = \"Layout Vertical\"\n").unwrap();

    seed_from(&system, &user);

    assert_eq!(Preferences::load(&user).group_choice("layout"), Some("Layout Vertical"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn seed_never_overwrites_the_users_file() {
    // From the first copy on the file is the user's. A package update shipping
    // a new template must not reset their choices.
    let root = scratch_dir("deckery-prefs-seed-keep");
    let system = root.join("system");
    let user = root.join("user");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(path_in(&system), "[exclusive_groups]\nlayout = \"Layout Vertical\"\n").unwrap();
    std::fs::write(path_in(&user), "[exclusive_groups]\nlayout = \"Layout Horizontal\"\n").unwrap();

    seed_from(&system, &user);

    assert_eq!(Preferences::load(&user).group_choice("layout"), Some("Layout Horizontal"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn seed_without_template_does_nothing() {
    let root = scratch_dir("deckery-prefs-seed-none");
    let user = root.join("user");
    seed_from(&root.join("system"), &user);
    assert!(!path_in(&user).exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn only_the_exact_file_name_is_the_preferences_file() {
    // load_entries uses this to skip the file; a module that merely has a
    // similar name must still be discovered.
    assert!(is_preferences_file("preferences.toml"));
    assert!(!is_preferences_file("preferences.toml.tmp"));
    assert!(!is_preferences_file("My preferences.toml"));
    assert!(!is_preferences_file("Preferences.toml"));
}
