//! Building the active config: module merging, binding conflicts and the
//! aliases a merge collects.

use super::*;

// ── Automatic module merging ──────────────────────────────────────────────────

#[test]
fn plain_module_bindings_are_merged_into_base() {
    let b = base("Steam Deck", &["Steam Deck"]);
    let m = with_binding(module("gestures", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);

    let r = make_registry(vec![wrap(b, true), wrap(m, true)]);
    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));
}

#[test]
fn base_binding_wins_over_module() {
    let b = with_binding(base("Steam Deck", &["Steam Deck"]), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);
    let m = with_binding(module("gestures", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);

    let r = make_registry(vec![wrap(b, true), wrap(m, true)]);
    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_B));
}

#[test]
fn alphabetically_last_module_wins_over_earlier_one() {
    // Colliding bindings are a config bug, reported by report_binding_conflicts.
    // Order is fixed alphabetically so the outcome is at least deterministic,
    // and the warning lands on the config that actually took effect.
    let first  = with_binding(module("aaa", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let second = with_binding(module("bbb", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);

    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(first, true),
        wrap(second, true),
    ]);
    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_B));
}

#[test]
fn a_user_module_wins_over_an_alphabetically_later_shipped_one() {
    // The point of the rule: a small hand-written file patches the shipped set
    // without its author having to find a name that sorts last.
    let shipped = with_binding(module("zzz Shipped", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let mine    = with_binding(module("aaa Mine",    None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);

    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(shipped, true),
        wrap_user(mine, true),
    ]);
    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_B));
}

#[test]
fn two_base_configs_claiming_one_controller_are_reported() {
    // The leftover case: an old copy of the base config under a retired name.
    // Still valid, still names the same hardware, and whichever of the two ends
    // up driving the controller is down to HashMap order.
    let mut map: HashMap<String, ConfigEntry> = vec![
        wrap(base("Steam Deck Base", &["Steam Deck"]), true),
        wrap_user(base("Steam Deck", &["Steam Deck"]), true),
    ].into_iter().map(|e| (e.name.clone(), e)).collect();

    report_device_conflicts(&mut map);

    let warned: Vec<&String> = map.values()
        .filter(|e| e.errors.iter().any(|err| err.severity == "warning"))
        .map(|e| &e.name)
        .collect();
    assert_eq!(warned, vec!["Steam Deck Base"],
               "exactly one of the pair carries the warning");
}

#[test]
fn unrelated_controllers_are_not_reported() {
    // Neither name contains the other, so no device can match both — sharing a
    // word is not a conflict.
    let mut map: HashMap<String, ConfigEntry> = vec![
        wrap(base("Steam Deck Base", &["Steam Deck"]), true),
        wrap(base("Steam Controller", &["Steam Controller"]), true),
    ].into_iter().map(|e| (e.name.clone(), e)).collect();

    report_device_conflicts(&mut map);

    assert!(map.values().all(|e| e.errors.is_empty()),
            "unrelated declarations must stay quiet");
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
        wrap_user(module("My Tweaks", None), true),
    ]);
    let cfg = r.resolve("Steam Deck Base", &Client::Default).unwrap();
    assert!(cfg.mapped_modifiers.all.contains(&Event::Key(evdev::Key::BTN_TL)),
            "base modifier lost: {:?}", cfg.mapped_modifiers);
}

#[test]
fn a_module_brings_its_own_modifiers_along() {
    // A module is free to introduce a modifier the base config never uses. Its
    // bindings are merged either way, so without the modifier they would load
    // and then never fire.
    let m = with_modifier(module("KDE Desktop", None), evdev::Key::BTN_TR);
    let r = make_registry(vec![
        wrap(base("Steam Deck Base", &["Steam Deck"]), true),
        wrap(m, true),
    ]);
    let cfg = r.resolve("Steam Deck Base", &Client::Default).unwrap();
    assert!(cfg.mapped_modifiers.all.contains(&Event::Key(evdev::Key::BTN_TR)),
            "module modifier lost: {:?}", cfg.mapped_modifiers);
}

#[test]
fn an_app_override_keeps_the_modifiers_of_the_config_below_it() {
    let b = with_modifier(base("Steam Deck Base", &["Steam Deck"]), evdev::Key::BTN_TL);
    let r = make_registry(vec![
        wrap(b, true),
        wrap(module("Firefox", Some("firefox")), true),
    ]);
    let cfg = r.resolve("Steam Deck Base", &class("firefox")).unwrap();
    assert!(cfg.mapped_modifiers.all.contains(&Event::Key(evdev::Key::BTN_TL)),
            "base modifier lost under an app override: {:?}", cfg.mapped_modifiers);
}

#[test]
fn among_user_modules_the_alphabet_still_decides() {
    let first  = with_binding(module("aaa", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let second = with_binding(module("bbb", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);

    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap_user(first, true),
        wrap_user(second, true),
    ]);
    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_B));
}

#[test]
fn a_user_module_outranks_the_shipped_base_config() {
    // The bindings somebody wants to change are declared in the base config, so
    // the root rule would buy nothing if it stopped below it.
    let b    = with_binding(base("Steam Deck", &["Steam Deck"]), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let mine = with_binding(module("Mine", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);

    let r = make_registry(vec![wrap(b, true), wrap_user(mine, true)]);
    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_B));
}

#[test]
fn the_users_own_base_config_outranks_their_modules() {
    // Both files are the user's, so nothing is being worked around any more and
    // the more specific statement — the one naming the device — wins.
    let b    = with_binding(base("Steam Deck", &["Steam Deck"]), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let mine = with_binding(module("Mine", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);

    let r = make_registry(vec![wrap_user(b, true), wrap_user(mine, true)]);
    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
    assert!(has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));
}

#[test]
fn a_user_module_keeps_what_the_base_config_is() {
    // The user's modules are merged on top of the base, so the result is built
    // from a file that declares no hardware. What it *is* must survive that.
    let mut b = with_binding(base("Steam Deck", &["Steam Deck"]), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    b.aliases.insert("A".into(), "BTN_SOUTH".into());
    let mine = with_binding(module("Mine", None), evdev::Key::BTN_NORTH, evdev::Key::KEY_B);

    let r = make_registry(vec![wrap(b, true), wrap_user(mine, true)]);
    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
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
    let mine = module("Mine", None);

    let r = make_registry(vec![wrap(b, true), wrap_user(mine, true)]);
    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
    assert!(!cfg.gaming_mode_config.auto_detect_steam_games);
}

#[test]
fn module_gated_to_another_compositor_is_not_merged() {
    let mut m = with_binding(module("kde-only", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    m.module.requires_compositor = Some("KDE".into());

    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(m, true),
    ]);
    r.set_compositor(Some("Hyprland".into()));
    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
    assert!(!has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));
}

#[test]
fn disabled_module_is_not_merged() {
    let m = with_binding(module("gestures", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(m, false),
    ]);
    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
    assert!(!has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_A));
}

#[test]
fn an_app_override_is_not_merged_into_the_base() {
    // A plain module applies wherever it is merged; an app override waits for
    // its window. With no window focused, only the first of the two is in.
    let app   = with_binding(module("konsole", Some("org.kde.konsole")), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let plain = with_binding(module("Voice Control", None), evdev::Key::BTN_NORTH, evdev::Key::KEY_B);

    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(app, true),
        wrap(plain, true),
    ]);
    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
    assert!(!has_binding(&cfg, evdev::Key::BTN_SOUTH, evdev::Key::KEY_A),
            "the app override must wait for its window");
    assert!(has_binding(&cfg, evdev::Key::BTN_NORTH, evdev::Key::KEY_B),
            "the plain module applies unconditionally");
}

#[test]
fn trackpad_config_reaches_the_base_from_a_module() {
    // The shipped trackpad config lives in its own module, and the base config
    // carries no [trackpad] at all. merge_base() only inherits trackpad
    // settings when both of its own sides are "disabled" — which an empty base
    // happens to satisfy. Tightening that condition would silently leave both
    // pads dead, hence this test.
    let mut m = module("Steam Deck Trackpad", None);
    m.trackpad.right.mode = "mt-trackpad".to_string();
    m.trackpad.combined_gesture_device = true;

    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(m, true),
    ]);
    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
    assert_eq!(cfg.trackpad.right.mode, "mt-trackpad");
    assert!(cfg.trackpad.combined_gesture_device);
}

#[test]
fn base_trackpad_config_wins_over_a_module() {
    // The other direction: once the base declares a pad, the module must not
    // take it back — an override of Steam Deck.toml has to stay in charge.
    let mut b = base("Steam Deck", &["Steam Deck"]);
    b.trackpad.right.mode = "trackball".to_string();
    let mut m = module("Steam Deck Trackpad", None);
    m.trackpad.right.mode = "mt-trackpad".to_string();

    let r = make_registry(vec![wrap(b, true), wrap(m, true)]);
    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
    assert_eq!(cfg.trackpad.right.mode, "trackball");
}

#[test]
fn settings_reach_the_base_from_a_module() {
    // [settings] moved into Steam Deck Settings.toml, so stick mode and
    // deadzones now arrive through the module merge rather than from the file
    // that declares the device.
    let mut m = module("Steam Deck Settings", None);
    m.settings.insert("RSTICK".to_string(), "cursor".to_string());

    let r = make_registry(vec![
        wrap(base("Steam Deck", &["Steam Deck"]), true),
        wrap(m, true),
    ]);
    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
    assert_eq!(cfg.settings.get("RSTICK").map(String::as_str), Some("cursor"));
}

#[test]
fn module_does_not_override_base_gaming_mode() {
    let mut b = base("Steam Deck", &["Steam Deck"]);
    b.gaming_mode_config.auto_detect_steam_games = false;

    let r = make_registry(vec![wrap(b, true), wrap(module("gestures", None), true)]);
    let cfg = r.resolve("Steam Deck", &Client::Default).unwrap();
    assert!(!cfg.gaming_mode_config.auto_detect_steam_games);
}

// ── Binding conflicts ─────────────────────────────────────────────────────────



#[test]
fn colliding_modules_warn_on_the_losing_config() {
    let mut m = map_of(vec![
        with_binding(module("aaa", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A),
        with_binding(module("bbb", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B),
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
    let shipped = with_binding(module("Steam Deck Buttons", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    let mine    = with_binding(module("My Tweaks",          None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);
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
        with_binding(module("aaa", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A),
        with_binding(module("bbb", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B),
    ].into_iter().map(|c| (c.name.clone(), wrap_user(c, true))).collect();
    report_binding_conflicts(&mut m);

    assert_eq!(warnings_of(&m, "bbb").len(), 1);
}

#[test]
fn distinct_bindings_do_not_warn() {
    let mut m = map_of(vec![
        with_binding(module("aaa", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A),
        with_binding(module("bbb", None), evdev::Key::BTN_NORTH, evdev::Key::KEY_B),
    ]);
    report_binding_conflicts(&mut m);
    assert!(warnings_of(&m, "bbb").is_empty());
}

#[test]
fn modules_gated_to_different_compositors_do_not_warn() {
    // KDE and Hyprland modules never load together, so identical bindings in
    // both are the intended translation of one gesture, not a conflict.
    let mut kde = with_binding(module("KDE Desktop", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_A);
    kde.module.requires_compositor = Some("KDE".into());
    let mut hypr = with_binding(module("Hyprland Desktop", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B);
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
        with_binding(module("konsole", Some("org.kde.konsole")), evdev::Key::BTN_SOUTH, evdev::Key::KEY_B),
        with_binding(module("zzz", None), evdev::Key::BTN_SOUTH, evdev::Key::KEY_C),
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
