// ── Deckery State Export ──────────────────────────────────────────────────────
//
// Provides `build_state()` — a pure function that assembles the full
// state snapshot as a JSON value, consumed by the Deckery HUD overlay
// to display live button mappings without re-implementing makima's lookup
// logic.
//
// Actual I/O (writing /tmp/makima-state.json) is handled exclusively by
// `state_writer::flush()` via the `StateWriterHandle` channel.  Nothing in
// this module touches the filesystem.

use crate::config::{Event, GamingModeConfig, Hint};
use crate::resolver::{resolve_binding, ResolvedBinding};
use crate::config::Config;
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
    pub silent: bool,             // true → suppress toast in HUD
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
///
/// `paused` — whether makima is currently in paused mode (all remaps suppressed).
/// `gaming_mode` — whether makima is in Gaming Mode (remaps + trackpad routing
/// suppressed so games can use the controller directly via Steam Input).
pub fn build_state(
    config: &Config,
    modifiers: &[Event],
    layout: u16,
    paused: bool,
    gaming_mode: bool,
    held_keys: &[Event],
    last_action: &Option<LastAction>,
    config_stack: &[String],
    gaming_mode_config: &GamingModeConfig,
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
            let silent = config.bindings.silent.contains(&(*trigger, combo.clone()));
            bindings.insert(
                key,
                serde_json::json!({
                    "action": action_list,
                    "origin": origin_remap(trigger, combo),
                    "label": label,
                    "kind": "remap",
                    "silent": silent,
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

    // The merge flattens base and override hints into one map, so the hint
    // itself is the only record of where its line was written — the active
    // config's name would report a base hint as app-specific in every app.
    let origin_hint = |hint: &Hint| -> &str {
        if hint.from_override { &config.name } else { base_name }
    };

    // Modifier-less hints relabel a plain button, so they belong here and not in
    // modifier_active — there is no modifier to wait for. The existing action is
    // left in place: it stays the HUD's fallback text and keeps active_outputs
    // honest. resolve_hints already refused any button that carries a written
    // label, so nothing hand-written is overwritten.
    for ((trigger, combo), hint) in &config.bindings.hints_resolved {
        if !combo.is_empty() {
            continue;
        }
        let key = event_to_str(trigger);
        match bindings.get_mut(&key) {
            Some(entry) => {
                entry["label"]  = serde_json::json!(hint.label);
                entry["origin"] = serde_json::json!(origin_hint(hint));
                entry["kind"]   = serde_json::json!("hint");
            }
            None => {
                bindings.insert(key, serde_json::json!({
                    "action": [],
                    "origin": origin_hint(hint),
                    "label":  hint.label,
                    "kind":   "hint",
                    "silent": false,
                }));
            }
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

    // Hints are display-only, so their modifiers were never registered in
    // mapped_modifiers and never enter `modifiers`. They are recognised from
    // held_keys instead, which tracks every pressed button regardless of role.
    // Real modifiers stay in the comparison set: holding L1 *and* R5 must hide
    // R5-only hints, because L1 opens an actual layer.
    let hint_modifier_buttons: std::collections::HashSet<Event> = config
        .bindings
        .hints_resolved
        .keys()
        .flat_map(|(_, combo)| combo.iter().copied())
        .collect();
    let mut hint_mods: Vec<Event> = active_input_mods.clone();
    hint_mods.extend(held_keys.iter().filter(|e| hint_modifier_buttons.contains(e)).copied());
    hint_mods.sort();
    hint_mods.dedup();

    // A combo is shown only when the held set is exactly it — same rule as the
    // resolver, so the HUD never advertises something that would not fire.
    let shown_under = |combo: &Vec<Event>, held: &[Event]| -> bool {
        !combo.is_empty() && combo.len() == held.len() && combo.iter().all(|m| held.contains(m))
    };
    let mut modifier_active: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    if !active_input_mods.is_empty() {
        // Remap combos
        for (trigger, modifier_map) in &config.bindings.remap {
            for (combo, actions) in modifier_map {
                if shown_under(combo, &active_input_mods) {
                    let key = event_to_str(trigger);
                    let action_list: Vec<serde_json::Value> = actions
                        .iter()
                        .map(|k| serde_json::Value::String(format!("{:?}", k)))
                        .collect();
                    let label = config.bindings.labels.get(&(*trigger, combo.clone()));
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
                if shown_under(combo, &active_input_mods) {
                    let key = event_to_str(trigger);
                    let action_list: Vec<serde_json::Value> = commands
                        .iter()
                        .map(|s| serde_json::Value::String(s.clone()))
                        .collect();
                    let label = config.bindings.labels.get(&(*trigger, combo.clone()));
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
                if shown_under(combo, &active_input_mods) {
                    let key = event_to_str(trigger);
                    let label = config.bindings.labels.get(&(*trigger, combo.clone()));
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

    // Hints: a fourth pass, matched against hint_mods rather than
    // active_input_mods. Precedence against real bindings was already applied in
    // resolve_hints, so the only guard needed here is against another hint of
    // equal length landing on the same button.
    if !hint_mods.is_empty() {
        for ((trigger, combo), hint) in &config.bindings.hints_resolved {
            if !shown_under(combo, &hint_mods) {
                continue;
            }
            let key = event_to_str(trigger);
            if modifier_active.contains_key(&key) {
                continue;
            }
            modifier_active.insert(
                key,
                serde_json::json!({
                    "action": [],
                    "origin": origin_hint(hint),
                    "kind": "hint",
                    "label": hint.label,
                }),
            );
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
    // active_outputs: array of { key, silent } objects.
    // All resolved key outputs are included; silent=true entries are tagged
    // so the HUD can choose to suppress or dim them — not omitted by the backend.
    let mut active_outputs_map: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
    for held_event in held_keys {
        let is_silent = config.bindings.silent.contains(&(*held_event, sorted_mods.clone()))
            || config.bindings.silent.contains(&(*held_event, vec![]));
        match resolve_binding(&config.bindings, *held_event, &sorted_mods, chain_only) {
            ResolvedBinding::Keys { keys, .. } => {
                for k in &keys {
                    let key_str = format!("{:?}", k);
                    // If the key already exists as non-silent, keep it non-silent.
                    active_outputs_map.entry(key_str).or_insert(is_silent);
                }
            }
            // Command or movement handles this event — no key output.
            ResolvedBinding::Command { .. } | ResolvedBinding::Movement { .. } => {}
            // Hold binding or unbound — no keys to add.
            ResolvedBinding::Hold { .. } | ResolvedBinding::Unbound => {}
        }
    }
    // Sort by modifier rank first, then alphabetically (BTreeMap gives alpha order).
    let mut active_outputs_sorted: Vec<(String, bool)> = active_outputs_map.into_iter().collect();
    active_outputs_sorted.sort_by(|(a, _), (b, _)| {
        modifier_sort_key(a.as_str()).cmp(&modifier_sort_key(b.as_str()))
    });
    let active_outputs: Vec<serde_json::Value> = active_outputs_sorted
        .into_iter()
        .map(|(key, silent)| serde_json::json!({ "key": key, "silent": silent }))
        .collect();

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

    // For each available modifier, check whether any qualifying combo originates
    // from an app-specific override (config.override_bindings). If yes →
    // has_app_combos: true in the emitted object, so the HUD can signal it.
    // A combo qualifies for modifier m when it contains m and all currently
    // held modifiers — shared predicate used by both the availability filter
    // and the has_app_combos check so the logic can't drift out of sync.
    let qualifies = |m: &Event, combo: &Vec<Event>| -> bool {
        combo.contains(m) && active_input_mods.iter().all(|held| combo.contains(held))
    };
    let has_app_combos = |m: &Event| -> bool {
        match &config.override_bindings {
            None => false,
            Some(ov) => {
                ov.remap.values().flat_map(|map| map.keys()).any(|c| qualifies(m, c))
                    || ov.commands.values().flat_map(|map| map.keys()).any(|c| qualifies(m, c))
                    || ov.movements.values().flat_map(|map| map.keys()).any(|c| qualifies(m, c))
            }
        }
    };

    let mut available_modifiers: serde_json::Map<String, serde_json::Value> = config.mapped_modifiers.custom
        .iter()
        .filter(|m| !active_input_mods.contains(m))
        .filter(|m| all_combos.iter().any(|combo| qualifies(m, combo)))
        .map(|m| (event_to_str(m), serde_json::json!({
            "has_app_combos": has_app_combos(m),
        })))
        .collect();

    // Hint modifiers join the same list, marked virtual. The field's meaning
    // widens from "pressing this unlocks a combo" to "…a combo *or* hints" —
    // that is deliberate: the point is to signal that something is there to
    // discover before the button is pressed. They are NOT added to
    // held_modifiers, which stays reserved for makima's own input modifiers.
    let hint_qualifies = |m: &Event, combo: &Vec<Event>| -> bool {
        combo.contains(m) && hint_mods.iter().all(|held| combo.contains(held))
    };
    for m in &hint_modifier_buttons {
        if hint_mods.contains(m) {
            continue;
        }
        let key = event_to_str(m);
        if available_modifiers.contains_key(&key) {
            continue;
        }
        // `has_app_combos` asks the same question here as for real modifiers —
        // "is at least one of the things behind this button app-specific?" — so
        // it is answered the same way, from where the hints were written.
        let mut unlocks_any = false;
        let mut unlocks_app = false;
        for ((_, combo), hint) in &config.bindings.hints_resolved {
            if !hint_qualifies(m, combo) {
                continue;
            }
            unlocks_any = true;
            unlocks_app |= hint.from_override;
        }
        if unlocks_any {
            available_modifiers.insert(key, serde_json::json!({
                "has_app_combos": unlocks_app,
                "virtual": true,
            }));
        }
    }

    // Determine active app name from config_stack (e.g., "org.mozilla.firefox" or "default")
    let active_app = if config_stack.len() > 1 {
        config_stack[1].clone()
    } else {
        "default".to_string()
    };

    let gaming_mode_trigger = gaming_mode_config.trigger.as_ref().map(|t| {
        serde_json::json!({
            "key":   event_to_str(&Event::Key(t.key)),
            "label": "Gaming Mode",
        })
    });

    serde_json::json!({
        "context": {
            "active_app": active_app,
            "config_stack": config_stack,
            "layout": layout,
            "paused": paused,
            "gaming_mode": gaming_mode,
            "held_modifiers": held_modifiers,
            "active_buttons": active_buttons,
            "active_outputs": active_outputs,
            "available_modifiers": available_modifiers,
        },
        "last_action": last_action,
        "bindings": bindings,
        "modifier_active": modifier_active,
        "gaming_mode_trigger": gaming_mode_trigger,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────


#[cfg(test)]
#[path = "state_export_tests.rs"]
mod tests;
