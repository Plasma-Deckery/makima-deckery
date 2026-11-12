use super::*;

const REST: [f32; 8] = [0.044, 0.012, 0.017, -0.010, 0.0, 0.0, 0.0, 0.0];

#[test]
fn nothing_written_yet_counts_as_moved() {
    assert!(analog_moved(&[f32::NAN; 8], &REST));
}

#[test]
fn deadzone_wander_is_not_a_move() {
    // What a resting Deck stick actually reports from one tick to the next.
    let mut next = REST;
    next[0] = 0.047;
    next[3] = -0.007;
    assert!(!analog_moved(&REST, &next));
}

#[test]
fn a_visible_move_on_any_axis_is_a_move() {
    for i in 0..8 {
        let mut next = REST;
        next[i] = crate::analog::r3(next[i] + 0.005);
        assert!(analog_moved(&REST, &next), "axis {i}");
    }
}

fn cache(config: &Arc<Config>) -> StateCache {
    StateCache {
        config: config.clone(),
        modifiers: vec![],
        paused: false,
        gaming_mode: false,
        held_keys: vec![],
        last_action: None,
        config_stack: vec!["default".into()],
        state: serde_json::Value::Null,
    }
}

#[test]
fn cache_matches_same_inputs() {
    let config = Arc::new(Config::new_empty("default".into()));
    let c = cache(&config);
    assert!(c.matches(&config, &[], false, false, &[], &None, &["default".into()]));
}

#[test]
fn cache_misses_on_any_changed_input() {
    let config = Arc::new(Config::new_empty("default".into()));
    let c = cache(&config);
    let stack = ["default".to_string()];
    let key = Event::Key(Key::BTN_SOUTH);
    // Equal contents, different Arc: a reloaded config must rebuild.
    assert!(!c.matches(&Arc::new(Config::new_empty("default".into())), &[], false, false, &[], &None, &stack));
    assert!(!c.matches(&config, &[key], false, false, &[], &None, &stack));
    assert!(!c.matches(&config, &[], true, false, &[], &None, &stack));
    assert!(!c.matches(&config, &[], false, true, &[], &None, &stack));
    assert!(!c.matches(&config, &[], false, false, &[key], &None, &stack));
    assert!(!c.matches(&config, &[], false, false, &[], &None, &["default".into(), "game".into()]));
    let la = Some(LastAction {
        r#type: "keys".into(), value: serde_json::json!(["KEY_A"]),
        ts: 1.0, label: None, silent: false,
    });
    assert!(!c.matches(&config, &[], false, false, &[], &la, &stack));
}
