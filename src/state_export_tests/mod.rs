//! Tests for `state_export`, one file per part of the snapshot.

mod bindings;
mod context;
mod hints;
mod modifiers;

use super::*;
use crate::config::{Bindings, Event, GamingModeConfig, MappedModifiers};
use crate::config::Config;
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
        bindings: Bindings {
            remap: remap_map,
            commands: cmd_map,
            movements: HashMap::new(),
            no_pause: HashSet::new(),
            while_gaming: HashSet::new(),
            labels: HashMap::new(),
            silent: HashSet::new(),
            ..Default::default()
        },
        override_bindings: None,
        settings: HashMap::new(),
        mapped_modifiers: MappedModifiers {
            default: vec![],
            custom: custom_modifiers,
            all: all_mods,
        },
        trackpad: Default::default(),
        gaming_mode_config: Default::default(),
        device: None,
        module: Default::default(),
        aliases: Default::default(),
    }
}

/// Extract active_outputs keys (ignoring silent flag) for easy assertions.
fn active_output_keys(state: &serde_json::Value) -> Vec<String> {
    state["context"]["active_outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["key"].as_str().unwrap().to_string())
        .collect()
}

/// Extract (key, silent) pairs from active_outputs.
fn active_outputs_tagged(state: &serde_json::Value) -> Vec<(String, bool)> {
    state["context"]["active_outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (
            v["key"].as_str().unwrap().to_string(),
            v["silent"].as_bool().unwrap(),
        ))
        .collect()
}

fn active_outputs(state: &serde_json::Value) -> Vec<String> {
    active_output_keys(state)
}

/// Returns the keys of available_modifiers (now a map, not an array).
fn available_modifiers(state: &serde_json::Value) -> Vec<String> {
    state["context"]["available_modifiers"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

/// Returns has_app_combos for a specific modifier key.
fn avail_mod_has_app_combos(state: &serde_json::Value, key: &str) -> bool {
    state["context"]["available_modifiers"][key]["has_app_combos"]
        .as_bool()
        .unwrap_or(false)
}

/// Like make_config but with override_bindings set (simulates an app-specific layer).
fn make_config_with_override(
    remap: Vec<(Event, Vec<Event>, Vec<Key>)>,
    commands: Vec<(Event, Vec<Event>, Vec<String>)>,
    custom_modifiers: Vec<Event>,
    override_remap: Vec<(Event, Vec<Event>, Vec<Key>)>,
) -> Config {
    let mut base = make_config(remap, commands, custom_modifiers);
    let mut ov_remap: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
    for (trigger, combo, keys) in override_remap {
        ov_remap.entry(trigger).or_default().insert(combo, keys);
    }
    base.override_bindings = Some(Bindings {
        remap: ov_remap,
        commands: HashMap::new(),
        movements: HashMap::new(),
        no_pause: HashSet::new(),
        while_gaming: HashSet::new(),
        labels: HashMap::new(),
        silent: HashSet::new(),
        ..Default::default()
    });
    base
}

fn modifier_active_keys(state: &serde_json::Value) -> Vec<String> {
    state["modifier_active"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}
