//! Hints: what the overlay suggests for a held button, and the rule the
//! reader announces by.

use super::*;

// ── Hints in state.json ───────────────────────────────────────────────────
//
// The other half of docs/hints.md: resolution is covered in config_tests,
// these cover what the HUD actually receives.

/// Base layout mirroring the live config: R5 → Ctrl, DPad Up/Down → arrows,
/// L1 a real modifier with one combo on it.
fn config_with_ctrl_hints() -> Config {
    let mut config = make_config(
        vec![
            (key(Key::BTN_GRIPR2),    vec![],                  vec![Key::KEY_LEFTCTRL]),
            (key(Key::BTN_DPAD_UP),   vec![],                  vec![Key::KEY_UP]),
            (key(Key::BTN_DPAD_DOWN), vec![],                  vec![Key::KEY_DOWN]),
            (key(Key::BTN_NORTH),     vec![key(Key::BTN_TL)],  vec![Key::KEY_C]),
        ],
        vec![],
        vec![key(Key::BTN_TL)],
    );
    config.bindings.hints = [
        ("KEY_LEFTCTRL-KEY_UP".to_string(),   "Jump to Top".to_string()),
        ("KEY_LEFTCTRL-KEY_DOWN".to_string(), "Jump to Bottom".to_string()),
    ].into_iter().collect();
    config.resolve_hints();
    config
}

/// Holding R5 must surface the hint on DPad Up, even though R5 is not a
/// modifier and therefore never appears in `modifiers`.
#[test]
fn held_hint_modifier_surfaces_hints_in_modifier_active() {
    let config = config_with_ctrl_hints();
    let state = build_state(
        &config, &[], false, false,
        &[key(Key::BTN_GRIPR2)],   // held_keys: R5 down, nothing else
        &None, &["test".to_string()], &GamingModeConfig::default(),
    );

    let entry = &state["modifier_active"]["BTN_DPAD_UP"];
    assert_eq!(entry["label"], "Jump to Top");
    assert_eq!(entry["kind"], "hint");
    assert_eq!(entry["action"], serde_json::json!([]));
}

/// Invariant 5: hints must not leak into held_modifiers — that field means
/// makima's own input modifiers and nothing else.
#[test]
fn held_hint_modifier_stays_out_of_held_modifiers() {
    let config = config_with_ctrl_hints();
    let state = build_state(
        &config, &[], false, false,
        &[key(Key::BTN_GRIPR2)],
        &None, &["test".to_string()], &GamingModeConfig::default(),
    );

    assert_eq!(state["context"]["held_modifiers"], serde_json::json!([]));
}

/// Matching is exact. Adding a real modifier on top of the hint modifier must
/// hide the hint — L1 opens an actual layer, so R5-only hints no longer apply.
#[test]
fn hint_disappears_when_a_real_modifier_is_also_held() {
    let config = config_with_ctrl_hints();
    let state = build_state(
        &config, &[key(Key::BTN_TL)], false, false,
        &[key(Key::BTN_GRIPR2), key(Key::BTN_TL)],
        &None, &["test".to_string()], &GamingModeConfig::default(),
    );

    assert!(state["modifier_active"].get("BTN_DPAD_UP").is_none(),
        "R5-only hint survived L1 being held: {}", state["modifier_active"]);
}

/// Before the button is pressed, the hint modifier is advertised in
/// available_modifiers so the HUD can signal that something is there.
#[test]
fn hint_modifier_is_advertised_as_virtual_when_not_held() {
    let config = config_with_ctrl_hints();
    let state = build_state(
        &config, &[], false, false, &[],
        &None, &["test".to_string()], &GamingModeConfig::default(),
    );

    let avail = &state["context"]["available_modifiers"];
    assert_eq!(avail["BTN_GRIPR2"]["virtual"], true);
    // A real modifier is still listed, and is not marked virtual.
    assert!(avail["BTN_TL"].get("virtual").is_none());
}

/// `has_app_combos` means the same thing for a virtual modifier as for a real
/// one — "something behind this button is app-specific" — so it follows where
/// the hints were written instead of being assumed.
#[test]
fn virtual_modifier_reports_whether_its_hints_are_app_specific() {
    let avail_of = |config: &Config| {
        build_state(
            config, &[], false, false, &[],
            &None, &["test".to_string()], &GamingModeConfig::default(),
        )["context"]["available_modifiers"]["BTN_GRIPR2"]["has_app_combos"].clone()
    };

    assert_eq!(avail_of(&config_with_ctrl_hints()), false,
        "hints written in the base config are not app-specific");

    let mut app = config_with_ctrl_hints();
    app.override_bindings = Some(Bindings {
        hints: app.bindings.hints.clone(),
        ..Default::default()
    });
    app.resolve_hints();

    assert_eq!(avail_of(&app), true);
}

/// Once held, the hint modifier is no longer "available" — it is in use.
#[test]
fn hint_modifier_leaves_available_modifiers_once_held() {
    let config = config_with_ctrl_hints();
    let state = build_state(
        &config, &[], false, false,
        &[key(Key::BTN_GRIPR2)],
        &None, &["test".to_string()], &GamingModeConfig::default(),
    );

    assert!(state["context"]["available_modifiers"].get("BTN_GRIPR2").is_none());
}

/// A held hint modifier counts as held for the real modifiers too. No combo
/// contains R5 alongside L1, so while R5 is down L1 unlocks nothing new and
/// must not be advertised — before, it was matched against an empty held
/// set and every real modifier with any combo showed up.
#[test]
fn held_hint_modifier_hides_real_modifiers_without_a_combo_on_it() {
    let config = config_with_ctrl_hints();
    let state = build_state(
        &config, &[], false, false,
        &[key(Key::BTN_GRIPR2)],
        &None, &["test".to_string()], &GamingModeConfig::default(),
    );

    assert!(state["context"]["available_modifiers"].get("BTN_TL").is_none(),
        "L1 advertised while only R5 is held: {}", state["context"]["available_modifiers"]);
}

// ── Modifier-less hints ───────────────────────────────────────────────────

/// `from_override` mirrors the merge: the hint is in the merged map either way,
/// and `override_bindings.hints` is the only record that it came from the app.
fn config_with_plain_hint(from_override: bool) -> Config {
    let mut config = make_config(
        vec![(key(Key::BTN_SOUTH), vec![], vec![Key::KEY_ENTER])],
        vec![],
        vec![],
    );
    config.name = "Claude Desktop".to_string();
    let hints: HashMap<String, String> = [
        ("KEY_ENTER".to_string(), "Enter (Send Message)".to_string()),
    ].into_iter().collect();
    config.bindings.hints = hints.clone();
    if from_override {
        config.override_bindings = Some(Bindings { hints, ..Default::default() });
    }
    config.resolve_hints();
    config
}

/// A hint with no modifier has nothing to wait for, so it belongs in the base
/// `bindings` map. The action is kept — the label sits on top of it.
#[test]
fn modifier_less_hint_relabels_the_base_binding() {
    let config = config_with_plain_hint(true);
    let state = build_state(
        &config, &[], false, false, &[],
        &None, &["Steam Deck".to_string()], &GamingModeConfig::default(),
    );

    let entry = &state["bindings"]["BTN_SOUTH"];
    assert_eq!(entry["label"], "Enter (Send Message)");
    assert_eq!(entry["kind"], "hint");
    assert_eq!(entry["action"], serde_json::json!(["KEY_ENTER"]));
    // The hint line lives in the app config, so origin flips and the HUD shows
    // it as app-specific.
    assert_eq!(entry["origin"], "Claude Desktop");
}

/// The other direction, and the reason `hints_from_override` exists: a hint
/// written in the base config must keep the base origin while an app config is
/// active, or every base hint would render as an app override everywhere.
#[test]
fn base_hint_keeps_the_base_origin_under_an_app_config() {
    let config = config_with_plain_hint(false);
    let state = build_state(
        &config, &[], false, false, &[],
        &None, &["Steam Deck".to_string()], &GamingModeConfig::default(),
    );

    assert_eq!(state["bindings"]["BTN_SOUTH"]["origin"], "Steam Deck");
}

/// It must not leak into modifier_active — there is no combo to activate.
#[test]
fn modifier_less_hint_stays_out_of_modifier_active() {
    let config = config_with_plain_hint(true);
    let state = build_state(
        &config, &[], false, false, &[key(Key::BTN_SOUTH)],
        &None, &["Steam Deck".to_string()], &GamingModeConfig::default(),
    );

    assert_eq!(state["modifier_active"], serde_json::json!({}));
}

/// …and it must not turn its own trigger into an advertised modifier.
#[test]
fn modifier_less_hint_adds_no_available_modifier() {
    let config = config_with_plain_hint(true);
    let state = build_state(
        &config, &[], false, false, &[],
        &None, &["Steam Deck".to_string()], &GamingModeConfig::default(),
    );

    assert_eq!(state["context"]["available_modifiers"], serde_json::json!({}));
}

// ── matching_hint: the rule the event reader announces by ─────────────────
//
// The HUD grid and the OSD toast must agree about what a press means, so both
// go through `matching_hint`. These pin the agreement: what the grid shows
// under a given held set is exactly what a press under that set announces.

/// Held set and trigger from the ticket's example: R5 + DPad Up is hinted,
/// and that is what the toast has to say.
#[test]
fn matching_hint_names_the_hint_under_its_modifier() {
    let config = config_with_ctrl_hints();
    let held   = vec![key(Key::BTN_GRIPR2), key(Key::BTN_DPAD_UP)];
    let active = active_input_modifiers(&config, &[]);
    let mods   = hint_modifiers(&config, &active, &held);

    let hint = matching_hint(&config, &key(Key::BTN_DPAD_UP), &mods);
    assert_eq!(hint.map(|h| h.label.as_str()), Some("Jump to Top"));
}

/// The trigger needs no special handling: DPad Up is held too, but it is not a
/// hint modifier anywhere, so it never enters the comparison set.
#[test]
fn matching_hint_ignores_the_pressed_button_itself() {
    let config = config_with_ctrl_hints();
    let held   = vec![key(Key::BTN_GRIPR2), key(Key::BTN_DPAD_UP)];
    let mods   = hint_modifiers(&config, &[], &held);
    assert_eq!(mods, vec![key(Key::BTN_GRIPR2)]);
}

/// Nothing held that a hint names means nothing to announce — a plain press
/// must stay silent rather than raise a toast for its base binding.
#[test]
fn matching_hint_is_none_without_the_hint_modifier() {
    let config = config_with_ctrl_hints();
    let mods   = hint_modifiers(&config, &[], &[key(Key::BTN_DPAD_UP)]);
    assert!(matching_hint(&config, &key(Key::BTN_DPAD_UP), &mods).is_none());
}

/// Same exactness as the grid: a real modifier on top of the hint modifier
/// opens an actual layer, so the hint no longer applies and must not be
/// announced either.
#[test]
fn matching_hint_is_none_when_a_real_modifier_is_also_held() {
    let config = config_with_ctrl_hints();
    let held   = vec![key(Key::BTN_GRIPR2), key(Key::BTN_DPAD_UP)];
    let active = active_input_modifiers(&config, &[key(Key::BTN_TL)]);
    let mods   = hint_modifiers(&config, &active, &held);

    assert!(matching_hint(&config, &key(Key::BTN_DPAD_UP), &mods).is_none(),
        "R5-only hint survived L1 being held: {mods:?}");
}

/// A hint on another button must not be announced for this one.
#[test]
fn matching_hint_is_per_trigger() {
    let config = config_with_ctrl_hints();
    let held   = vec![key(Key::BTN_GRIPR2), key(Key::BTN_DPAD_DOWN)];
    let mods   = hint_modifiers(&config, &[], &held);

    assert_eq!(matching_hint(&config, &key(Key::BTN_DPAD_DOWN), &mods)
                   .map(|h| h.label.as_str()), Some("Jump to Bottom"));
    assert_eq!(matching_hint(&config, &key(Key::BTN_SOUTH), &mods).map(|h| h.label.as_str()),
               None);
}
