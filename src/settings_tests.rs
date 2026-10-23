use super::*;
use crate::config::Config;

use std::collections::HashMap;

/// A config whose `[settings]` says exactly this and nothing else.
fn with(pairs: &[(&str, &str)]) -> Config {
    let mut config = Config::new_empty("test".to_string());
    config.settings = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<HashMap<_, _>>();
    config
}

/// An empty `[settings]` is the normal case and yields no complaints.
#[test]
fn nothing_configured_is_not_a_problem() {
    let (settings, problems) = Settings::from_config(&with(&[]));

    assert!(problems.is_empty());
    assert_eq!(settings.lstick.function, "disabled");
    assert_eq!(settings.lstick.deadzone, 5);
    assert_eq!(settings.cursor.speed, 0);
}

/// The two defaults that are `true`, which is the direction that is easy to get
/// backwards and would silently change behaviour for everyone.
#[test]
fn the_two_defaults_that_are_true() {
    let (settings, _) = Settings::from_config(&with(&[]));

    assert!(settings.chain_only, "chain_only defaults to true");
    assert!(settings.cursor_when_paused, "a paused makima still moves the pointer");
}

/// A value that parses is used.
#[test]
fn a_good_value_is_used() {
    let (settings, problems) = Settings::from_config(&with(&[
        ("RSTICK", "cursor"),
        ("RSTICK_DEADZONE", "12"),
        ("RSTICK_SENSITIVITY", "8"),
        ("16_BIT_AXIS", "true"),
    ]));

    assert!(problems.is_empty());
    assert_eq!(settings.rstick.function, "cursor");
    assert_eq!(settings.rstick.deadzone, 12);
    assert_eq!(settings.rstick.sensitivity, 8);
    assert!(settings.axis_16_bit);
}

/// The point of the whole module: a mistyped number is reported and the default
/// is used, where it used to panic the daemon at construction.
#[test]
fn a_bad_number_is_reported_and_the_default_stands() {
    let (settings, problems) = Settings::from_config(&with(&[
        ("LSTICK_DEADZONE", "ziemlich viel"),
    ]));

    assert_eq!(settings.lstick.deadzone, 5, "the default stands");
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].key, "LSTICK_DEADZONE");
    assert_eq!(problems[0].value, "ziemlich viel");
    assert!(problems[0].to_string().contains("the default is used instead"));
}

/// A flag spelled some other way is reported too, rather than quietly meaning
/// the opposite of what it says — which is what treating everything but "true"
/// as false would do to `CHAIN_ONLY = "yes"`.
#[test]
fn a_flag_that_is_neither_true_nor_false_is_reported() {
    let (settings, problems) = Settings::from_config(&with(&[("CHAIN_ONLY", "yes")]));

    assert!(settings.chain_only, "the default stands, which here is true");
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].key, "CHAIN_ONLY");
}

/// Several broken values all get reported; parsing never stops at the first.
/// A config with one typo and one real change should not hide the change.
#[test]
fn every_broken_value_is_reported() {
    let (_, problems) = Settings::from_config(&with(&[
        ("CURSOR_SPEED", "schnell"),
        ("SCROLL_ACCEL", "viel"),
        ("STADIA", "vielleicht"),
    ]));

    let mut keys: Vec<&str> = problems.iter().map(|p| p.key.as_str()).collect();
    keys.sort();
    assert_eq!(keys, vec!["CURSOR_SPEED", "SCROLL_ACCEL", "STADIA"]);
}

/// Absent and unparseable are different things, and only the second is worth
/// telling anyone about.
#[test]
fn absent_and_broken_are_not_the_same() {
    let (_, absent) = Settings::from_config(&with(&[]));
    let (_, broken) = Settings::from_config(&with(&[("CURSOR_ACCEL", "x")]));

    assert!(absent.is_empty());
    assert_eq!(broken.len(), 1);
}

/// Every reportable key really is one the parser looks at, and really does
/// produce a problem naming itself. That is what keeps the list from drifting
/// away from the parsing — a key missing here would be a warning that can be
/// set and never cleared.
#[test]
fn every_reportable_key_reports_itself() {
    for key in REPORTABLE_KEYS {
        let (_, problems) = Settings::from_config(&with(&[(key, "völliger Unsinn")]));
        assert_eq!(
            problems.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(),
            vec![*key],
            "{key} did not report itself",
        );
    }
}

/// The two free-string keys cannot fail, which is why they are not in the list.
#[test]
fn the_stick_function_keys_cannot_be_wrong() {
    let (settings, problems) = Settings::from_config(&with(&[("LSTICK", "Unsinn")]));

    assert!(problems.is_empty(), "a free string has no parse to fail");
    assert_eq!(settings.lstick.function, "Unsinn");
    assert!(!REPORTABLE_KEYS.contains(&"LSTICK"));
    assert!(!REPORTABLE_KEYS.contains(&"RSTICK"));
}
