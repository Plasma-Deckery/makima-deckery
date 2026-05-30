// ── Deckery State Export ──────────────────────────────────────────────────────
//
// Writes /tmp/makima-state.json atomically on every config or modifier change.
// The file is consumed by the Deckery HUD overlay to display live button
// mappings without re-implementing any of makima's lookup logic.
//
// Called from EventReader::write_state() in event_reader.rs, which handles
// all Arc/Mutex locking and passes plain values here.

use crate::config::Event;
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
    // derived from held_keys + current modifier context.
    // Used by the HUD to highlight active system-level keys in the center strip.
    let mut sorted_mods = modifiers.to_vec();
    sorted_mods.sort();
    sorted_mods.dedup();
    let mut active_outputs: Vec<String> = Vec::new();
    for held_event in held_keys {
        if let Some(modifier_map) = config.bindings.remap.get(held_event) {
            // If a combo remap exists for the current modifier set, use it.
            if let Some(keys) = modifier_map.get(&sorted_mods) {
                for k in keys {
                    active_outputs.push(format!("{:?}", k));
                }
                continue;
            }
            // If a combo command or movement handles this button+modifier combination,
            // no keys are sent — don't fall back to the base remap output.
            if !sorted_mods.is_empty() {
                let command_handles = config.bindings.commands
                    .get(held_event)
                    .and_then(|m| m.get(&sorted_mods))
                    .is_some();
                let movement_handles = config.bindings.movements
                    .get(held_event)
                    .and_then(|m| m.get(&sorted_mods))
                    .is_some();
                if command_handles || movement_handles {
                    continue;
                }
            }
            // No combo override — fall back to base remap (e.g. Ctrl+Up via held Ctrl).
            if let Some(keys) = modifier_map.get(&vec![]) {
                for k in keys {
                    active_outputs.push(format!("{:?}", k));
                }
            }
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
            // A modifier m is available if there exists a combo C where:
            // - m ∈ C
            // - all currently held modifiers are in C (active_input_mods ⊆ C)
            // This means pressing m (possibly with others) could unlock a binding.
            all_combos.iter().any(|combo| {
                combo.contains(m)
                    && active_input_mods.iter().all(|held| combo.contains(held))
            })
        })
        .map(event_to_str)
        .collect();
    available_modifiers.sort();
    available_modifiers.dedup();

    let state = serde_json::json!({
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
    });

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
