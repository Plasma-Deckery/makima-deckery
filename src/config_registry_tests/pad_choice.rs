//! The pad modes chosen in `preferences.toml`, over the configs on disk.

use super::*;

const BASE: &str = r#"
[device]
class = "hid-steam"
names = ["Steam Deck"]
"#;

const TRACKPADS: &str = r#"
[trackpad.right]
mode = "mt-trackpad"
click_pressure = 30000
"#;

/// A system root with a base config and the shipped trackpad module, and a
/// user root with `preferences`.
fn roots_with(name: &str, preferences: &str) -> (PathBuf, ConfigRoots) {
    let dir = scratch_dir(name);
    let system = dir.join("system");
    let user = dir.join("user");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(system.join("Steam Deck.toml"), BASE).unwrap();
    std::fs::write(system.join("Steam Deck Trackpads.toml"), TRACKPADS).unwrap();
    std::fs::write(user.join("preferences.toml"), preferences).unwrap();
    (dir, ConfigRoots { system, user })
}

fn right_mode(registry: &ConfigRegistry) -> (String, Option<u16>) {
    let base = registry.base_configs().remove(0);
    (base.trackpad.right.mode, base.trackpad.right.click_pressure)
}

/// The choice replaces the module's mode and keeps the rest of its table, in
/// what a device session starts from and in what the readers resolve.
#[test]
fn a_chosen_mode_is_laid_over_the_module() {
    let (dir, roots) = roots_with("pad-choice", "[trackpad.right]\nmode = \"trackball\"\n");
    let registry = ConfigRegistry::load(roots);
    assert_eq!(right_mode(&registry), ("trackball".to_string(), Some(30000)));
    let resolved = registry.resolve("Steam Deck", &Client::Default).unwrap();
    assert_eq!(resolved.trackpad.right.mode, "trackball");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn without_a_choice_the_module_decides() {
    let (dir, roots) = roots_with("pad-choice-none", "");
    let registry = ConfigRegistry::load(roots);
    assert_eq!(right_mode(&registry).0, "mt-trackpad");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A mode makima does not know is ignored, and the file's other choices
/// still apply.
#[test]
fn an_unknown_mode_is_ignored_and_the_rest_of_the_file_holds() {
    let (dir, roots) = roots_with("pad-choice-bad",
        "[modules]\ndisabled = [\"Steam Deck Trackpads\"]\n[trackpad.right]\nmode = \"marble\"\n");
    let registry = ConfigRegistry::load(roots);
    assert_eq!(right_mode(&registry).0, "disabled", "the trackpad module is off, and marble ignored");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A change to the file is read on the next reload — which an edit from
/// outside triggers, see below.
#[test]
fn a_reload_reads_a_changed_choice() {
    let (dir, roots) = roots_with("pad-choice-reload", "[trackpad.right]\nmode = \"trackball\"\n");
    let user = roots.user.clone();
    let registry = ConfigRegistry::load(roots);
    std::fs::write(user.join("preferences.toml"), "[trackpad.right]\nmode = \"mt-trackpad\"\n").unwrap();
    registry.reload();
    assert_eq!(right_mode(&registry).0, "mt-trackpad");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A tray toggle rewrites the file; the pad choice survives it.
#[test]
fn a_toggle_keeps_the_pad_choice_in_the_file() {
    let (dir, roots) = roots_with("pad-choice-toggle", "[trackpad.right]\nmode = \"trackball\"\n");
    let user = roots.user.clone();
    let registry = ConfigRegistry::load(roots);
    assert!(registry.set_enabled("Steam Deck Trackpads", false));
    assert!(registry.set_enabled("Steam Deck Trackpads", true));
    let text = std::fs::read_to_string(user.join("preferences.toml")).unwrap();
    assert!(text.contains("[trackpad.right]") && text.contains("trackball"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The watcher reloads on an edit from outside and not on this process's
/// own writes.
#[test]
fn only_an_edit_from_outside_reloads() {
    let (dir, roots) = roots_with("pad-choice-watch", "");
    let file = roots.user.join("preferences.toml");
    let registry = ConfigRegistry::load(roots);
    let event = || notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
        .add_path(file.clone());
    let known = || registry.preferences_text.lock().unwrap().clone();

    assert!(!ConfigRegistry::is_preferences_edit(&event(), &known()), "nothing changed");

    registry.set_enabled("Steam Deck Trackpads", false);
    assert!(!ConfigRegistry::is_preferences_edit(&event(), &known()), "its own write");

    std::fs::write(&file, "[trackpad.right]\nmode = \"trackball\"\n").unwrap();
    assert!(ConfigRegistry::is_preferences_edit(&event(), &known()), "the tour's write");

    registry.reload();
    assert!(!ConfigRegistry::is_preferences_edit(&event(), &known()), "read back on reload");
    let _ = std::fs::remove_dir_all(&dir);
}
