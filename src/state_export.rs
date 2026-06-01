// ── Deckery State Export ──────────────────────────────────────────────────────
//
// Writes /tmp/makima-state.json atomically on every config or modifier change.
// The file is consumed by the Deckery HUD overlay to display live button
// mappings without re-implementing any of makima's lookup logic.
//
// Called from EventReader::write_state() in event_reader.rs, which handles
// all Arc/Mutex locking and passes plain values here.

use crate::config::Event;
use crate::resolver::{resolve_binding, ResolvedBinding};
use crate::Config;
use evdev::Key;
use serde::Serialize;
use serde_json;

// ── Structs ───────────────────────────────────────────────────────────────────

/// Fire-and-forget record of the last discrete user action.
/// Written by the backend on press; never deleted by the backend.
/// The HUD reads `ts` and fades the display after 1.5 s.
#[derive(Debug, Clone, Serialize)]
pub struct LastAction {
    pub r#type: String,           // "keys" | "command" | "exec"
    pub value: serde_json::Value, // [KEY_*] for keys, string for command/exec
    pub ts: f64,                  // Unix timestamp (secs + fractional)
    pub label: Option<String>,    // human-readable label, if set on the binding
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn event_to_str(event: &Event) -> String {
    match event {
        Event::Key(k)  => format!("{:?}", k),
        Event::Axis(a) => format!("{:?}", a),
        Event::Hold    => "Hold".to_string(),
    }
}

/// Current Unix timestamp as f64 (seconds + fractional).
pub fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Canonical modifier order: Meta → Ctrl → Alt → Shift → everything else.
/// Matches the standard shortcut notation used by most desktop environments.
fn modifier_sort_key(key: &str) -> (u8, String) {
    let rank = match key {
        "KEY_LEFTMETA"  | "KEY_RIGHTMETA"  => 0,
        "KEY_LEFTCTRL"  | "KEY_RIGHTCTRL"  => 1,
        "KEY_LEFTALT"   | "KEY_RIGHTALT"   => 2,
        "KEY_LEFTSHIFT" | "KEY_RIGHTSHIFT" => 3,
        _ => 4,
    };
    (rank, key.to_owned())
}

// ── Pure state builder ────────────────────────────────────────────────────────

/// Build the full state snapshot as a JSON value.
///
/// Pure and side-effect-free — no I/O. Extracted from write_state so it can
/// be called from unit tests without touching the filesystem.
pub fn build_state(
    config: &Config,
    modifiers: &[Event],
    layout: u16,
    paused: bool,
    held_keys: &[Event],
    last_action: &Option<LastAction>,
    config_stack: &[String],
) -> serde_json::Value {
    // Determine the origin name for a given trigger+combo.
    // If this config has override_bindings (i.e. it's an app-specific config
    // that was merged with a base), check whether the binding existed in the
    // original override. If yes → came from this config; if no → inherited
    // from base (config_stack[0]).
    let base_name = config_stack.first().map(|s| s.as_str()).unwrap_or(&config.name);
    let origin_remap = |trigger: &Event, combo: &Vec<Event>| -> &str {
        match &config.override_bindings {
            Some(ov) => {
                if ov.remap.get(trigger).and_then(|m| m.get(combo)).is_some() {
                    &config.name
                } else {
                    base_name
                }
            }
            None => &config.name,
        }
    };
    let origin_cmd = |trigger: &Event, combo: &Vec<Event>| -> &str {
        match &config.override_bindings {
            Some(ov) => {
                if ov.commands.get(trigger).and_then(|m| m.get(combo)).is_some() {
                    &config.name
                } else {
                    base_name
                }
            }
            None => &config.name,
        }
    };
    let origin_mov = |trigger: &Event, combo: &Vec<Event>| -> &str {
        match &config.override_bindings {
            Some(ov) => {
                if ov.movements.get(trigger).and_then(|m| m.get(combo)).is_some() {
                    &config.name
                } else {
                    base_name
                }
            }
            None => &config.name,
        }
    };

    // Build bindings map: all remaps + commands from current config.
    // Key format: "BTN_FOO" for plain bindings, "MOD-BTN_FOO" for combos.
    let mut bindings = serde_json::Map::new();
    for (trigger, modifier_map) in &config.bindings.remap {
        for (combo, actions) in modifier_map {
            let key = if combo.is_empty() {
                event_to_str(trigger)
            } else {
                let parts: Vec<String> = combo.iter().map(event_to_str).collect();
                format!("{}-{}", parts.join("-"), event_to_str(trigger))
            };
            let action_list: Vec<serde_json::Value> = actions
                .iter()
                .map(|k| serde_json::Value::String(format!("{:?}", k)))
                .collect();
            let label = config.bindings.labels.get(&(*trigger, combo.clone()));
            bindings.insert(
                key,
                serde_json::json!({
                    "action": action_list,
                    "origin": origin_remap(trigger, combo),
                    "label": label,
                    "kind": "remap",
                }),
            );
        }
    }
    for (trigger, modifier_map) in &config.bindings.commands {
        for (combo, cmds) in modifier_map {
            let key = if combo.is_empty() {
                event_to_str(trigger)
            } else {
                let parts: Vec<String> = combo.iter().map(event_to_str).collect();
                format!("{}-{}", parts.join("-"), event_to_str(trigger))
            };
            let action_list: Vec<serde_json::Value> = cmds
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect();
            let label = config.bindings.labels.get(&(*trigger, combo.clone()));
            let no_pause = config.bindings.no_pause.contains(&(*trigger, combo.clone()));
            bindings.insert(
                key,
                serde_json::json!({
                    "action": action_list,
                    "origin": origin_cmd(trigger, combo),
                    "label": label,
                    "kind": "command",
                    "no_pause": no_pause,
                }),
            );
        }
    }

    // Build modifier_active: combos reachable given the currently held modifiers.
    let active_input_mods: Vec<Event> = config
        .mapped_modifiers
        .custom
        .iter()
        .filter(|input_mod| {
            if modifiers.contains(input_mod) {
                return true;
            }
            config
                .bindings
                .remap
                .get(*input_mod)
                .and_then(|m| m.get(&vec![]))
                .map(|output_keys: &Vec<Key>| {
                    output_keys
                        .iter()
                        .any(|k| modifiers.contains(&Event::Key(*k)))
                })
                .unwrap_or(false)
        })
        .cloned()
        .collect();

    // Track the combo length of each inserted entry so a more specific combo
    // (longer = more modifiers) always wins over a less specific one.
    let mut modifier_active: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    let mut modifier_active_combo_len: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    if !active_input_mods.is_empty() {
        // Remap combos
        for (trigger, modifier_map) in &config.bindings.remap {
            for (combo, actions) in modifier_map {
                if !combo.is_empty() && combo.len() == active_input_mods.len() && combo.iter().all(|m| active_input_mods.contains(m)) {
                    let key = event_to_str(trigger);
                    if combo.len() < *modifier_active_combo_len.get(&key).unwrap_or(&0) {
                        continue;
                    }
                    let action_list: Vec<serde_json::Value> = actions
                        .iter()
                        .map(|k| serde_json::Value::String(format!("{:?}", k)))
                        .collect();
                    let label = config.bindings.labels.get(&(*trigger, combo.clone()));
                    modifier_active_combo_len.insert(key.clone(), combo.len());
                    modifier_active.insert(
                        key,
                        serde_json::json!({
                            "action": action_list,
                            "origin": origin_remap(trigger, combo),
                            "kind": "remap",
                            "label": label,
                        }),
                    );
                }
            }
        }
        // Command combos (e.g. BTN_TL-BTN_DPAD_LEFT = ["qdbus ..."])
        for (trigger, modifier_map) in &config.bindings.commands {
            for (combo, commands) in modifier_map {
                if !combo.is_empty() && combo.len() == active_input_mods.len() && combo.iter().all(|m| active_input_mods.contains(m)) {
                    let key = event_to_str(trigger);
                    if combo.len() < *modifier_active_combo_len.get(&key).unwrap_or(&0) {
                        continue;
                    }
                    let action_list: Vec<serde_json::Value> = commands
                        .iter()
                        .map(|s| serde_json::Value::String(s.clone()))
                        .collect();
                    let label = config.bindings.labels.get(&(*trigger, combo.clone()));
                    modifier_active_combo_len.insert(key.clone(), combo.len());
                    modifier_active.insert(
                        key,
                        serde_json::json!({
                            "action": action_list,
                            "origin": origin_cmd(trigger, combo),
                            "kind": "command",
                            "label": label,
                        }),
                    );
                }
            }
        }
        // Movement combos (e.g. LSTICK_UP = ["KEY_W"] in bind-mode)
        for (trigger, modifier_map) in &config.bindings.movements {
            for (combo, movement) in modifier_map {
                if !combo.is_empty() && combo.len() == active_input_mods.len() && combo.iter().all(|m| active_input_mods.contains(m)) {
                    let key = event_to_str(trigger);
                    if combo.len() < *modifier_active_combo_len.get(&key).unwrap_or(&0) {
                        continue;
                    }
                    let label = config.bindings.labels.get(&(*trigger, combo.clone()));
                    modifier_active_combo_len.insert(key.clone(), combo.len());
                    modifier_active.insert(
                        key,
                        serde_json::json!({
                            "action": [format!("{:?}", movement)],
                            "origin": origin_mov(trigger, combo),
                            "kind": "movement",
                            "label": label,
                        }),
                    );
                }
            }
        }
    }

    // held_modifiers: modifier buttons currently held (for combo-view switching).
    // active_buttons: ALL input buttons currently held (for per-button highlighting).
    let held_modifiers: Vec<String> = modifiers.iter().map(event_to_str).collect();
    let active_buttons: Vec<String> = held_keys.iter().map(event_to_str).collect();

    // active_outputs: the union of all evdev output keys currently being held,
    // derived from held_keys + current modifier context via resolve_binding().
    // Used by the HUD to highlight active system-level keys in the center strip.
    let chain_only = config.settings
        .get("CHAIN_ONLY")
        .map(|v| v == "true")
        .unwrap_or(true);
    let mut sorted_mods = modifiers.to_vec();
    sorted_mods.sort();
    sorted_mods.dedup();
    let mut active_outputs: Vec<String> = Vec::new();
    for held_event in held_keys {
        match resolve_binding(&config.bindings, *held_event, &sorted_mods, chain_only) {
            ResolvedBinding::Keys { keys, .. } => {
                for k in &keys {
                    active_outputs.push(format!("{:?}", k));
                }
            }
            // Command or movement handles this event — no key output.
            ResolvedBinding::Command { .. } | ResolvedBinding::Movement { .. } => {}
            // Hold binding or unbound — no keys to add.
            ResolvedBinding::Hold { .. } | ResolvedBinding::Unbound => {}
        }
    }
    active_outputs.sort_by_key(|k| modifier_sort_key(k.as_str()));
    active_outputs.dedup();

    // available_modifiers: custom modifier buttons that, if pressed next,
    // would unlock at least one additional combo binding.
    // A candidate modifier `m` qualifies when there exists a combo that:
    //   - contains `m`
    //   - contains all currently held modifiers as a subset
    //   - is not yet fully satisfied (i.e. m ∉ active_input_mods)
    let all_combos: Vec<&Vec<Event>> = config.bindings.remap.values()
        .flat_map(|m| m.keys())
        .chain(config.bindings.commands.values().flat_map(|m| m.keys()))
        .chain(config.bindings.movements.values().flat_map(|m| m.keys()))
        .filter(|c| !c.is_empty())
        .collect();
    let mut available_modifiers: Vec<String> = config.mapped_modifiers.custom
        .iter()
        .filter(|m| !active_input_mods.contains(m))
        .filter(|m| {
            all_combos.iter().any(|combo| {
                combo.contains(m)
                    && active_input_mods.iter().all(|held| combo.contains(held))
            })
        })
        .map(event_to_str)
        .collect();
    available_modifiers.sort();
    available_modifiers.dedup();

    serde_json::json!({
        "context": {
            "config_stack": config_stack,
            "layout": layout,
            "paused": paused,
            "held_modifiers": held_modifiers,
            "active_buttons": active_buttons,
            "active_outputs": active_outputs,
            "available_modifiers": available_modifiers,
        },
        "last_action": last_action,
        "bindings": bindings,
        "modifier_active": modifier_active,
    })
}

// ── Main export function ──────────────────────────────────────────────────────

pub async fn write_state(
    config: &Config,
    modifiers: &[Event],
    layout: u16,
    paused: bool,
    held_keys: &[Event],
    last_action: &Option<LastAction>,
    config_stack: &[String],
) {
    let state = build_state(config, modifiers, layout, paused, held_keys, last_action, config_stack);

    let tmp_path = "/tmp/makima-state.json.tmp";
    let final_path = "/tmp/makima-state.json";
    match serde_json::to_string_pretty(&state) {
        Ok(json) => {
            if std::fs::write(tmp_path, json).is_ok() {
                let _ = std::fs::rename(tmp_path, final_path);
            }
        }
        Err(e) => eprintln!("makima: state export failed: {}", e),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Bindings, Event, MappedModifiers};
    use crate::Config;
    use evdev::Key;
    use std::collections::{HashMap, HashSet};

    fn key(k: Key) -> Event { Event::Key(k) }

    /// Build a minimal Config with the given bindings and custom modifiers.
    fn make_config(
        remap: Vec<(Event, Vec<Event>, Vec<Key>)>,
        commands: Vec<(Event, Vec<Event>, Vec<String>)>,
        custom_modifiers: Vec<Event>,
    ) -> Config {
        let mut remap_map: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
        for (trigger, combo, keys) in remap {
            remap_map.entry(trigger).or_default().insert(combo, keys);
        }
        let mut cmd_map: HashMap<Event, HashMap<Vec<Event>, Vec<String>>> = HashMap::new();
        for (trigger, combo, cmds) in commands {
            cmd_map.entry(trigger).or_default().insert(combo, cmds);
        }
        let all_mods = custom_modifiers.clone();
        Config {
            name: "test".to_string(),
            associations: Default::default(),
            bindings: Bindings {
                remap: remap_map,
                commands: cmd_map,
                movements: HashMap::new(),
                no_pause: HashSet::new(),
                labels: HashMap::new(),
            },
            override_bindings: None,
            settings: HashMap::new(),
            mapped_modifiers: MappedModifiers {
                default: vec![],
                custom: custom_modifiers,
                all: all_mods,
            },
        }
    }

    fn active_outputs(state: &serde_json::Value) -> Vec<String> {
        state["context"]["active_outputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    fn available_modifiers(state: &serde_json::Value) -> Vec<String> {
        state["context"]["available_modifiers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    fn modifier_active_keys(state: &serde_json::Value) -> Vec<String> {
        state["modifier_active"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    // ── active_outputs ────────────────────────────────────────────────────────

    #[test]
    fn active_outputs_base_remap() {
        // BTN_SOUTH → KEY_ENTER, no modifiers held.
        let config = make_config(
            vec![(key(Key::BTN_SOUTH), vec![], vec![Key::KEY_ENTER])],
            vec![], vec![],
        );
        let state = build_state(&config, &[], 0, false, &[key(Key::BTN_SOUTH)], &None, &["test".to_string()]);
        assert_eq!(active_outputs(&state), vec!["KEY_ENTER"]);
    }

    #[test]
    fn active_outputs_combo_remap() {
        // BTN_TL-BTN_NORTH → [KEY_LEFTCTRL, KEY_C]. BTN_TL held as modifier.
        let btn_tl = key(Key::BTN_TL);
        let btn_north = key(Key::BTN_NORTH);
        let config = make_config(
            vec![(btn_north, vec![btn_tl], vec![Key::KEY_LEFTCTRL, Key::KEY_C])],
            vec![], vec![btn_tl],
        );
        // modifiers = [BTN_TL], held_keys = [BTN_NORTH]
        let state = build_state(&config, &[btn_tl], 0, false, &[btn_north], &None, &["test".to_string()]);
        // KEY_LEFTCTRL sorts before KEY_C
        assert_eq!(active_outputs(&state), vec!["KEY_LEFTCTRL", "KEY_C"]);
    }

    #[test]
    fn active_outputs_fallback_remap() {
        // BTN_SOUTH has base remap KEY_ENTER but no BTN_TL combo.
        // Holding BTN_TL should fall back to KEY_ENTER (the Ctrl+Enter scenario).
        let btn_tl = key(Key::BTN_TL);
        let btn_south = key(Key::BTN_SOUTH);
        let config = make_config(
            vec![(btn_south, vec![], vec![Key::KEY_ENTER])],
            vec![], vec![btn_tl],
        );
        let state = build_state(&config, &[btn_tl], 0, false, &[btn_south], &None, &["test".to_string()]);
        assert_eq!(active_outputs(&state), vec!["KEY_ENTER"]);
    }

    #[test]
    fn active_outputs_command_suppresses_remap() {
        // BTN_DPAD_UP has base remap KEY_UP, but BTN_TL-BTN_DPAD_UP is a command.
        // When BTN_TL held + BTN_DPAD_UP pressed, no key output (command takes over).
        let btn_tl = key(Key::BTN_TL);
        let btn_dpad_up = key(Key::BTN_DPAD_UP);
        let config = make_config(
            vec![(btn_dpad_up, vec![], vec![Key::KEY_UP])],
            vec![(btn_dpad_up, vec![btn_tl], vec!["previous-desktop".to_string()])],
            vec![btn_tl],
        );
        let state = build_state(&config, &[btn_tl], 0, false, &[btn_dpad_up], &None, &["test".to_string()]);
        assert_eq!(active_outputs(&state), Vec::<String>::new());
    }

    #[test]
    fn active_outputs_unbound_is_empty() {
        let config = make_config(vec![], vec![], vec![]);
        let state = build_state(&config, &[], 0, false, &[key(Key::BTN_SOUTH)], &None, &["test".to_string()]);
        assert_eq!(active_outputs(&state), Vec::<String>::new());
    }

    // ── available_modifiers ───────────────────────────────────────────────────

    #[test]
    fn available_modifiers_shows_both_when_none_held() {
        // Config has BTN_TL and BTN_TR combos. With no mods held, both should appear.
        let btn_tl = key(Key::BTN_TL);
        let btn_tr = key(Key::BTN_TR);
        let btn_north = key(Key::BTN_NORTH);
        let config = make_config(
            vec![
                (btn_north, vec![btn_tl], vec![Key::KEY_LEFTCTRL, Key::KEY_C]),
                (btn_north, vec![btn_tr], vec![Key::KEY_F5]),
            ],
            vec![], vec![btn_tl, btn_tr],
        );
        let state = build_state(&config, &[], 0, false, &[], &None, &["test".to_string()]);
        let mut avail = available_modifiers(&state);
        avail.sort();
        assert!(avail.contains(&"BTN_TL".to_string()));
        assert!(avail.contains(&"BTN_TR".to_string()));
    }

    #[test]
    fn available_modifiers_filters_satisfied() {
        // BTN_TL held. BTN_TL-only combos are now active (satisfied).
        // BTN_TR has a BTN_TL-BTN_TR combo → should still appear as available.
        let btn_tl = key(Key::BTN_TL);
        let btn_tr = key(Key::BTN_TR);
        let btn_north = key(Key::BTN_NORTH);
        let btn_south = key(Key::BTN_SOUTH);
        let config = make_config(
            vec![
                (btn_north, vec![btn_tl], vec![Key::KEY_LEFTCTRL, Key::KEY_C]),
                (btn_south, vec![btn_tl, btn_tr], vec![Key::KEY_F1]),
            ],
            vec![], vec![btn_tl, btn_tr],
        );
        // BTN_TL is active_input_mod → filtered out of available_modifiers.
        // BTN_TR qualifies because BTN_TL-BTN_TR-BTN_SOUTH exists and BTN_TL ⊆ that combo.
        let state = build_state(&config, &[btn_tl], 0, false, &[], &None, &["test".to_string()]);
        let avail = available_modifiers(&state);
        assert!(!avail.contains(&"BTN_TL".to_string()), "BTN_TL is already active");
        assert!(avail.contains(&"BTN_TR".to_string()), "BTN_TR unlocks a BTN_TL+BTN_TR combo");
    }

    // ── modifier_active ───────────────────────────────────────────────────────

    #[test]
    fn modifier_active_exact_match_only() {
        // BTN_TL held. Should show BTN_TL combos but NOT BTN_TL-BTN_TR combos.
        let btn_tl = key(Key::BTN_TL);
        let btn_tr = key(Key::BTN_TR);
        let btn_north = key(Key::BTN_NORTH);
        let btn_south = key(Key::BTN_SOUTH);
        let config = make_config(
            vec![
                (btn_north, vec![btn_tl], vec![Key::KEY_LEFTCTRL, Key::KEY_C]),
                (btn_south, vec![btn_tl, btn_tr], vec![Key::KEY_F1]),
            ],
            vec![], vec![btn_tl, btn_tr],
        );
        let state = build_state(&config, &[btn_tl], 0, false, &[], &None, &["test".to_string()]);
        let keys = modifier_active_keys(&state);
        assert!(keys.contains(&"BTN_NORTH".to_string()), "BTN_TL-BTN_NORTH should appear");
        assert!(!keys.contains(&"BTN_SOUTH".to_string()), "BTN_TL-BTN_TR-BTN_SOUTH must not leak in");
    }

    #[test]
    fn modifier_active_includes_commands() {
        // Commands under a modifier should appear in modifier_active, not just remaps.
        let btn_tl = key(Key::BTN_TL);
        let btn_dpad_up = key(Key::BTN_DPAD_UP);
        let config = make_config(
            vec![],
            vec![(btn_dpad_up, vec![btn_tl], vec!["previous-desktop".to_string()])],
            vec![btn_tl],
        );
        let state = build_state(&config, &[btn_tl], 0, false, &[], &None, &["test".to_string()]);
        let keys = modifier_active_keys(&state);
        assert!(keys.contains(&"BTN_DPAD_UP".to_string()));
        assert_eq!(
            state["modifier_active"]["BTN_DPAD_UP"]["kind"].as_str().unwrap(),
            "command"
        );
    }

    #[test]
    fn modifier_active_label_propagated() {
        // label set on a combo binding should appear in modifier_active.
        let btn_tl = key(Key::BTN_TL);
        let btn_north = key(Key::BTN_NORTH);
        let mut config = make_config(
            vec![(btn_north, vec![btn_tl], vec![Key::KEY_LEFTCTRL, Key::KEY_C])],
            vec![], vec![btn_tl],
        );
        config.bindings.labels.insert((btn_north, vec![btn_tl]), "Copy".to_string());
        let state = build_state(&config, &[btn_tl], 0, false, &[], &None, &["test".to_string()]);
        assert_eq!(
            state["modifier_active"]["BTN_NORTH"]["label"].as_str().unwrap(),
            "Copy"
        );
    }

    // ── bindings JSON fields ──────────────────────────────────────────────────

    #[test]
    fn bindings_json_no_pause_flag() {
        // no_pause = true on a command binding must appear in the bindings JSON.
        let btn_thumbl = key(Key::BTN_THUMBL);
        let mut config = make_config(
            vec![],
            vec![(btn_thumbl, vec![], vec!["hud-toggle".to_string()])],
            vec![],
        );
        config.bindings.no_pause.insert((btn_thumbl, vec![]));
        let state = build_state(&config, &[], 0, false, &[], &None, &["test".to_string()]);
        assert_eq!(
            state["bindings"]["BTN_THUMBL"]["no_pause"].as_bool().unwrap(),
            true
        );
    }

    // ── context fields ────────────────────────────────────────────────────────

    #[test]
    fn context_active_buttons_and_held_modifiers() {
        // active_buttons = all held input events; held_modifiers = only modifier subset.
        let btn_tl = key(Key::BTN_TL);
        let btn_south = key(Key::BTN_SOUTH);
        let config = make_config(
            vec![(btn_south, vec![], vec![Key::KEY_ENTER])],
            vec![], vec![btn_tl],
        );
        // BTN_TL is in modifiers (held modifier); BTN_SOUTH is in held_keys.
        let state = build_state(
            &config,
            &[btn_tl],
            0, false,
            &[btn_tl, btn_south],
            &None,
            &["test".to_string()],
        );
        let active_btns: Vec<_> = state["context"]["active_buttons"]
            .as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap()).collect();
        let held_mods: Vec<_> = state["context"]["held_modifiers"]
            .as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap()).collect();
        assert!(active_btns.contains(&"BTN_TL"));
        assert!(active_btns.contains(&"BTN_SOUTH"));
        assert!(held_mods.contains(&"BTN_TL"));
        assert!(!held_mods.contains(&"BTN_SOUTH"), "BTN_SOUTH is not a modifier");
    }

    // ── active_outputs: multi-modifier combo ──────────────────────────────────

    #[test]
    fn active_outputs_multi_modifier_combo() {
        // L1+R1+DPad_Up combo should produce its own output, not fall back to base.
        let btn_tl = key(Key::BTN_TL);
        let btn_tr = key(Key::BTN_TR);
        let btn_dpad_up = key(Key::BTN_DPAD_UP);
        let config = make_config(
            vec![
                (btn_dpad_up, vec![], vec![Key::KEY_UP]),
                (btn_dpad_up, vec![btn_tl, btn_tr], vec![Key::KEY_F1]),
            ],
            vec![], vec![btn_tl, btn_tr],
        );
        // Sort the modifier combo the same way resolve_binding expects it.
        let mut mods = vec![btn_tl, btn_tr];
        mods.sort();
        let state = build_state(&config, &mods, 0, false, &[btn_dpad_up], &None, &["test".to_string()]);
        assert_eq!(active_outputs(&state), vec!["KEY_F1"]);
    }

    // ── indirect modifier detection ───────────────────────────────────────────

    #[test]
    fn indirect_modifier_match_via_output_key() {
        // BTN_TL is a custom modifier whose base remap is KEY_LEFTCTRL.
        // If modifiers contains Event::Key(KEY_LEFTCTRL) instead of BTN_TL directly,
        // BTN_TL should still be detected as an active modifier for combo resolution.
        let btn_tl = key(Key::BTN_TL);
        let btn_north = key(Key::BTN_NORTH);
        let config = make_config(
            vec![
                (btn_tl, vec![], vec![Key::KEY_LEFTCTRL]),  // BTN_TL → KEY_LEFTCTRL
                (btn_north, vec![btn_tl], vec![Key::KEY_LEFTCTRL, Key::KEY_C]),
            ],
            vec![], vec![btn_tl],
        );
        // modifiers contains KEY_LEFTCTRL (the output key), not BTN_TL
        let state = build_state(
            &config,
            &[Event::Key(Key::KEY_LEFTCTRL)],
            0, false, &[], &None, &["test".to_string()],
        );
        // BTN_TL should be detected as active → modifier_active shows BTN_TL-BTN_NORTH combo
        let keys = modifier_active_keys(&state);
        assert!(keys.contains(&"BTN_NORTH".to_string()),
            "indirect modifier detection: BTN_NORTH combo should appear when KEY_LEFTCTRL held");
    }

    // ── override: command→remap type change doesn't leak base command ─────────

    #[test]
    fn override_remap_hides_base_command() {
        // Base: BTN_TL-BTN_DPAD_UP = command "previous-desktop"
        // Override (firefox): BTN_TL-BTN_DPAD_UP = remap [KEY_LEFTCTRL, KEY_R]
        //
        // After merge_base(), modifier_active must show the remap, NOT the command.
        // Before the fix, merge_base() let both coexist; the command loop in
        // build_state then overwrote the remap entry in modifier_active.
        let btn_tl     = key(Key::BTN_TL);
        let btn_dpad_up = key(Key::BTN_DPAD_UP);
        let combo       = vec![btn_tl];

        // Base config: BTN_TL-BTN_DPAD_UP → command
        let base = {
            let mut commands: HashMap<Event, HashMap<Vec<Event>, Vec<String>>> = HashMap::new();
            commands.entry(btn_dpad_up).or_default()
                .insert(combo.clone(), vec!["previous-desktop".to_string()]);
            Config {
                name: "Steam Deck".to_string(),
                associations: Default::default(),
                bindings: crate::config::Bindings {
                    commands,
                    ..Default::default()
                },
                override_bindings: None,
                settings: HashMap::new(),
                mapped_modifiers: crate::config::MappedModifiers {
                    default: vec![],
                    custom: vec![btn_tl],
                    all: vec![btn_tl],
                },
            }
        };

        // App config: BTN_TL-BTN_DPAD_UP → remap (overrides base command)
        let mut app = {
            let mut remap: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
            remap.entry(btn_dpad_up).or_default()
                .insert(combo.clone(), vec![Key::KEY_LEFTCTRL, Key::KEY_R]);
            Config {
                name: "firefox".to_string(),
                associations: Default::default(),
                bindings: crate::config::Bindings {
                    remap,
                    ..Default::default()
                },
                override_bindings: None,
                settings: HashMap::new(),
                mapped_modifiers: Default::default(),
            }
        };

        // This is the actual code path that runs at runtime.
        app.merge_base(&base);

        let state = build_state(
            &app, &[btn_tl], 0, false, &[], &None,
            &["Steam Deck".to_string(), "firefox".to_string()],
        );

        assert_eq!(
            state["modifier_active"]["BTN_DPAD_UP"]["kind"].as_str().unwrap(),
            "remap",
            "modifier_active must show the override remap, not the inherited command"
        );
        assert_eq!(
            state["modifier_active"]["BTN_DPAD_UP"]["action"][0].as_str().unwrap(),
            "KEY_LEFTCTRL",
        );
    }

    // ── origin: base config vs. app-specific override ─────────────────────────

    #[test]
    fn origin_override_vs_base() {
        // Simulate a Firefox app config layered on top of a Steam Deck base config.
        // Inherited bindings get origin="Steam Deck"; overridden ones get origin="firefox".
        use crate::config::{Bindings, MappedModifiers};
        use std::collections::{HashMap, HashSet};

        let btn_south = key(Key::BTN_SOUTH);
        let btn_tl = key(Key::BTN_TL);
        let btn_dpad_up = key(Key::BTN_DPAD_UP);

        // Merged bindings: base BTN_SOUTH + overridden BTN_TL-BTN_DPAD_UP
        let mut merged_remap: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
        merged_remap.entry(btn_south).or_default().insert(vec![], vec![Key::KEY_ENTER]);
        merged_remap.entry(btn_dpad_up).or_default().insert(
            vec![btn_tl], vec![Key::KEY_LEFTALT, Key::KEY_LEFT],
        );

        // override_bindings: only what the app config itself defined
        let mut override_remap: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
        override_remap.entry(btn_dpad_up).or_default().insert(
            vec![btn_tl], vec![Key::KEY_LEFTALT, Key::KEY_LEFT],
        );

        let config = Config {
            name: "firefox".to_string(),
            associations: Default::default(),
            bindings: Bindings {
                remap: merged_remap,
                commands: HashMap::new(),
                movements: HashMap::new(),
                no_pause: HashSet::new(),
                labels: HashMap::new(),
            },
            override_bindings: Some(Bindings {
                remap: override_remap,
                commands: HashMap::new(),
                movements: HashMap::new(),
                no_pause: HashSet::new(),
                labels: HashMap::new(),
            }),
            settings: HashMap::new(),
            mapped_modifiers: MappedModifiers {
                default: vec![],
                custom: vec![btn_tl],
                all: vec![btn_tl],
            },
        };

        let state = build_state(
            &config, &[], 0, false, &[], &None,
            &["Steam Deck".to_string(), "firefox".to_string()],
        );

        assert_eq!(
            state["bindings"]["BTN_SOUTH"]["origin"].as_str().unwrap(),
            "Steam Deck",
            "inherited binding should come from base config"
        );
        assert_eq!(
            state["bindings"]["BTN_TL-BTN_DPAD_UP"]["origin"].as_str().unwrap(),
            "firefox",
            "overridden binding should come from app config"
        );
    }
}
