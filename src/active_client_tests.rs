use super::*;

// `match_class` only answers "does any module claim this class?". Which of
// several claiming modules then wins is decided later by
// `ConfigRegistry::resolve_uncached`, and is covered by the registry tests. The
// tests here pin the first half: that the right windows get a `Client::Class`
// at all, and that every other window falls through to the base config.

/// A module bound to the given window classes, as `[module] match_window_class`
/// would produce it.
fn app(name: &str, classes: &[&str]) -> Config {
    let mut c = Config::new_empty(name.to_string());
    c.module.match_window_class = Some(classes.iter().map(|s| s.to_string()).collect());
    c
}

/// A module with no window binding at all, like a desktop layout module.
fn plain(name: &str) -> Config {
    Config::new_empty(name.to_string())
}

fn class(name: &str) -> Client {
    Client::Class(name.to_string(), String::new(), None)
}

#[test]
fn exact_class_match_yields_that_class() {
    let modules = vec![app("Konsole", &["org.kde.konsole"])];
    assert_eq!(match_class("org.kde.konsole".to_string(), &modules), class("org.kde.konsole"));
}

#[test]
fn unclaimed_class_falls_back_to_default() {
    // Returning a Class for a window nobody claims would only cost a cache
    // entry, but Default is what tells the resolver to reuse the base config.
    let modules = vec![app("Konsole", &["org.kde.konsole"])];
    assert_eq!(match_class("firefox".to_string(), &modules), Client::Default);
}

#[test]
fn no_modules_means_default() {
    assert_eq!(match_class("org.kde.konsole".to_string(), &[]), Client::Default);
}

#[test]
fn modules_without_window_binding_never_match() {
    // A module without `match_window_class` applies everywhere through the base
    // config. It must not turn arbitrary windows into app-specific clients.
    let modules = vec![plain("KDE Desktop"), plain("Voice Control")];
    assert_eq!(match_class("org.kde.konsole".to_string(), &modules), Client::Default);
}

#[test]
fn matching_is_case_sensitive() {
    // Compositors report the class verbatim and the config has to name it the
    // same way. X11 reports "Konsole" where Wayland reports "org.kde.konsole",
    // which is exactly why `match_window_class` accepts a list instead of
    // matching loosely.
    let modules = vec![app("Konsole", &["org.kde.konsole"])];
    assert_eq!(match_class("ORG.KDE.KONSOLE".to_string(), &modules), Client::Default);
    assert_eq!(match_class("Org.Kde.Konsole".to_string(), &modules), Client::Default);
}

#[test]
fn matching_is_whole_string_not_substring() {
    // "firefox" must not claim "firefox-developer-edition" or the other way
    // round, otherwise one app's bindings would leak into another.
    let modules = vec![app("Firefox", &["firefox"])];
    assert_eq!(match_class("firefox-developer-edition".to_string(), &modules), Client::Default);
    assert_eq!(match_class("fire".to_string(), &modules), Client::Default);
    assert_eq!(match_class(" firefox".to_string(), &modules), Client::Default);
}

#[test]
fn any_of_several_classes_in_one_module_matches() {
    let modules = vec![app("Firefox", &["firefox", "org.mozilla.firefox"])];
    assert_eq!(match_class("firefox".to_string(), &modules), class("firefox"));
    assert_eq!(match_class("org.mozilla.firefox".to_string(), &modules), class("org.mozilla.firefox"));
}

#[test]
fn a_later_module_can_claim_the_class() {
    // The scan must cover every module, not just the first one with a binding.
    let modules = vec![
        plain("KDE Desktop"),
        app("Konsole", &["org.kde.konsole"]),
        app("Firefox", &["firefox"]),
    ];
    assert_eq!(match_class("firefox".to_string(), &modules), class("firefox"));
}

#[test]
fn two_modules_claiming_the_same_class_still_yield_one_client() {
    // `match_class` deliberately carries no module identity: picking between
    // claimants is the resolver's job. All that matters here is that the
    // overlap neither panics nor degrades to Default.
    let modules = vec![app("Konsole A", &["org.kde.konsole"]), app("Konsole B", &["org.kde.konsole"])];
    assert_eq!(match_class("org.kde.konsole".to_string(), &modules), class("org.kde.konsole"));
}

#[test]
fn the_returned_client_carries_no_caption_or_pid() {
    // Caption and PID are only known on the KWin-script path. The polling path
    // has neither, so it must not invent them; there is no caption-based
    // matching in this module.
    let modules = vec![app("Konsole", &["org.kde.konsole"])];
    match match_class("org.kde.konsole".to_string(), &modules) {
        Client::Class(_, caption, pid) => {
            assert!(caption.is_empty());
            assert_eq!(pid, None);
        }
        Client::Default => panic!("expected a Class client"),
    }
}

#[test]
fn an_empty_class_is_default() {
    // A failed `kdotool` call or a window without class yields "", and that
    // must never select an app config.
    let modules = vec![app("Konsole", &["org.kde.konsole"])];
    assert_eq!(match_class(String::new(), &modules), Client::Default);
}

#[test]
fn an_empty_binding_list_claims_nothing() {
    // `match_window_class = []` is valid TOML. It must behave like no binding
    // rather than like a wildcard.
    let modules = vec![app("Nothing", &[])];
    assert_eq!(match_class("org.kde.konsole".to_string(), &modules), Client::Default);
}

// ── X11 WM_CLASS parsing ──────────────────────────────────────────────────────

#[test]
fn wm_class_yields_the_class_not_the_instance() {
    // WM_CLASS is "instance\0class\0". Matching against the instance would make
    // X11 configs disagree with the documented class names.
    assert_eq!(wm_class_name(b"konsole\0Konsole\0").as_deref(), Some("Konsole"));
}

#[test]
fn wm_class_without_trailing_nul_is_accepted() {
    // Some clients omit the final terminator; the class is still unambiguous.
    assert_eq!(wm_class_name(b"konsole\0Konsole").as_deref(), Some("Konsole"));
}

#[test]
fn wm_class_without_separator_is_rejected() {
    assert_eq!(wm_class_name(b"Konsole"), None);
    assert_eq!(wm_class_name(b""), None);
}

#[test]
fn wm_class_with_empty_class_part_is_empty() {
    // An empty class then falls through to Default in `match_class`.
    assert_eq!(wm_class_name(b"konsole\0\0").as_deref(), Some(""));
}

#[test]
fn wm_class_with_invalid_utf8_is_rejected() {
    assert_eq!(wm_class_name(b"x\0\xff\xfe\0"), None);
}
