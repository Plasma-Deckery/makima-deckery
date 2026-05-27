// ── Deckery State Export ──────────────────────────────────────────────────────
//
// Writes /tmp/makima-state.json atomically on every config or modifier change.
// The file is consumed by the Deckery HUD overlay (eww widget) to display
// live button mappings without re-implementing any of makima's lookup logic.
//
// Called from EventReader::write_state() in event_reader.rs, which handles
// all Arc/Mutex locking and passes plain values here.

use crate::config::Event;
use crate::Config;
use evdev::Key;
use serde_json;

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn event_to_str(event: &Event) -> String {
    match event {
        Event::Key(k)  => format!("{:?}", k),
        Event::Axis(a) => format!("{:?}", a),
        Event::Hold    => "Hold".to_string(),
    }
}

// ── Main export function ──────────────────────────────────────────────────────

pub async fn write_state(config: &Config, modifiers: &[Event], layout: u16) {
    // Build bindings map: all remaps from current config.
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
            bindings.insert(
                key,
                serde_json::json!({
                    "action": action_list,
                    "origin": config.name,
                }),
            );
        }
    }

    // Build modifier_active: combos reachable given the currently held modifiers.
    //
    // makima tracks held modifiers by their OUTPUT key (e.g. KEY_LEFTCTRL when
    // L1/BTN_TL is held), because toggle_modifiers is called with the remap
    // output. Combo entries in config.bindings.remap use the INPUT key (BTN_TL)
    // as the HashMap key. To bridge the gap we look up each custom modifier's
    // base output via remap[input_mod][vec![]] and check if that output key is
    // currently held. Any input modifier whose output is active is "live".
    let active_input_mods: Vec<Event> = config
        .mapped_modifiers
        .custom
        .iter()
        .filter(|input_mod| {
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

    let mut modifier_active = serde_json::Map::new();
    if !active_input_mods.is_empty() {
        for (trigger, modifier_map) in &config.bindings.remap {
            for (combo, actions) in modifier_map {
                // Show combos whose modifier set is fully covered by active input mods.
                if !combo.is_empty() && combo.iter().all(|m| active_input_mods.contains(m)) {
                    let action_list: Vec<serde_json::Value> = actions
                        .iter()
                        .map(|k| serde_json::Value::String(format!("{:?}", k)))
                        .collect();
                    modifier_active.insert(
                        event_to_str(trigger),
                        serde_json::json!({
                            "action": action_list,
                            "origin": config.name,
                        }),
                    );
                }
            }
        }
    }

    let state = serde_json::json!({
        "context": {
            "config_stack": [config.name],
            "layout": layout,
        },
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
