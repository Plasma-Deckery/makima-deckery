//! Tests for `machine`, one file per kind of decision.
//!
//! Each decision is tested on its own here, with a hand-built config. How the
//! reader applies them in sequence is tested against the real reader in
//! `event_loop/tests`.

mod emit;
mod haptic;
mod passthrough;
mod pause;
mod routing;
mod sticks;

use super::*;
use crate::config::{Bindings, MappedModifiers};

use std::collections::{HashMap, HashSet};

fn ev(k: Key) -> Event {
    Event::Key(k)
}

/// A config with the given base remaps and custom modifiers.
///
/// Deliberately hand-built rather than parsed: these tests are about what the
/// machine decides, and a TOML round-trip would put a second thing between the
/// assertion and the decision.
fn make_config(remap: Vec<(Event, Vec<Event>, Vec<Key>)>, custom_modifiers: Vec<Event>) -> Config {
    let mut remap_map: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
    for (trigger, combo, keys) in remap {
        remap_map.entry(trigger).or_default().insert(combo, keys);
    }
    let all = custom_modifiers.clone();
    Config {
        name: "test".to_string(),
        bindings: Bindings {
            remap: remap_map,
            commands: HashMap::new(),
            movements: HashMap::new(),
            no_pause: HashSet::new(),
            while_gaming: HashSet::new(),
            labels: HashMap::new(),
            silent: HashSet::new(),
            ..Default::default()
        },
        override_bindings: None,
        settings: HashMap::new(),
        mapped_modifiers: MappedModifiers { default: vec![], custom: custom_modifiers, all },
        trackpad: Default::default(),
        gaming_mode_config: Default::default(),
        device: None,
        module: Default::default(),
        aliases: Default::default(),
    }
}

/// The frames of a plan as (code, value) pairs, which is what the assertions are
/// actually about.
fn frames(plan: &EmitPlan) -> Vec<(u16, i32)> {
    plan.frames
        .iter()
        .flat_map(|a| match a {
            OutputAction::Frame { events, .. } => events.iter().map(|e| (e.code, e.value)).collect::<Vec<_>>(),
            _ => vec![],
        })
        .collect()
}

fn raw(ev_type: EventType, code: u16, value: i32) -> evdev::InputEvent {
    evdev::InputEvent::new_now(ev_type, code, value)
}
