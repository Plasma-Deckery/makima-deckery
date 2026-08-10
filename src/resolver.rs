// ── Binding Resolver ─────────────────────────────────────────────────────────
//
// Pure, side-effect-free binding resolution. Given a config, an input event,
// the current modifier set, and the pause state, returns what action should
// be taken — without performing any I/O.
//
// Both convert_event (event_reader.rs) and write_state (state_export.rs) can
// call this to stay in sync. Divergence between the two was the root cause of
// several bugs (modifier_active showing wrong bindings, active_outputs
// mismatching actual emission).

use crate::config::{Bindings, Event, Relative};
use evdev::Key;

// ── Result type ───────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Clone)]
pub enum ResolvedBinding {
    /// A key remap — emit these keys.
    Keys {
        keys: Vec<Key>,
        label: Option<String>,
        no_pause: bool,
        /// True if `while_gaming = true` was set — this binding fires even when
        /// Gaming Mode is active (all other key-press events are suppressed).
        while_gaming: bool,
        silent: bool,
        /// true when the match was an explicit combo (non-empty modifier set)
        is_combo: bool,
        /// true when modifiers are held but no combo was defined — falls back
        /// to the base binding. The held modifier output keys must stay held.
        is_fallback: bool,
    },
    /// A shell command binding.
    Command {
        commands: Vec<String>,
        label: Option<String>,
        no_pause: bool,
        /// True if `while_gaming = true` was set — this binding fires even when
        /// Gaming Mode is active (all other key-press events are suppressed).
        while_gaming: bool,
        silent: bool,
        is_combo: bool,
    },
    /// An axis movement binding (stick-to-mouse etc.).
    Movement {
        movement: Relative,
        is_combo: bool,
    },
    /// A Hold binding — fires when a modifier is held without a combo match.
    Hold { keys: Vec<Key> },
    /// No binding defined for this event + modifier combination.
    Unbound,
}

// ── Resolver ──────────────────────────────────────────────────────────────────

/// Resolve what action should be taken for `event` given `modifiers`.
///
/// `modifiers` must be sorted and deduplicated before calling.
/// `chain_only` mirrors the `CHAIN_ONLY` setting (Hold bindings only fire when
/// a modifier is held if true).
pub fn resolve_binding(
    bindings: &Bindings,
    event: Event,
    modifiers: &[Event],
    chain_only: bool,
) -> ResolvedBinding {
    let mods = modifiers.to_vec();

    if let Some(remap_map) = bindings.remap.get(&event) {
        // 1. Exact combo remap match.
        if let Some(keys) = remap_map.get(&mods) {
            let no_pause = bindings.no_pause.contains(&(event, mods.clone()))
                || bindings.no_pause.contains(&(event, vec![]));
            let while_gaming = bindings.while_gaming.contains(&(event, mods.clone()))
                || bindings.while_gaming.contains(&(event, vec![]));
            let silent = bindings.silent.contains(&(event, mods.clone()))
                || bindings.silent.contains(&(event, vec![]));
            let label = bindings.labels.get(&(event, mods.clone())).cloned();
            return ResolvedBinding::Keys {
                keys: keys.clone(),
                label,
                no_pause,
                while_gaming,
                silent,
                is_combo: !mods.is_empty(),
                is_fallback: false,
            };
        }

        // 2. Hold binding — fires when modifiers are held (or chain_only=false).
        if let Some(hold_keys) = remap_map.get(&vec![Event::Hold]) {
            if !mods.is_empty() || !chain_only {
                return ResolvedBinding::Hold {
                    keys: hold_keys.clone(),
                };
            }
        }

        // 3. Command with exact modifier match (nested under remap block in
        //    convert_event's lookup order).
        if let Some(cmd_map) = bindings.commands.get(&event) {
            if let Some(commands) = cmd_map.get(&mods) {
                let no_pause = bindings.no_pause.contains(&(event, mods.clone()))
                    || bindings.no_pause.contains(&(event, vec![]));
                let while_gaming = bindings.while_gaming.contains(&(event, mods.clone()))
                    || bindings.while_gaming.contains(&(event, vec![]));
                let silent = bindings.silent.contains(&(event, mods.clone()))
                    || bindings.silent.contains(&(event, vec![]));
                let label = bindings.labels.get(&(event, mods.clone())).cloned();
                return ResolvedBinding::Command {
                    commands: commands.clone(),
                    label,
                    no_pause,
                    while_gaming,
                    silent,
                    is_combo: !mods.is_empty(),
                };
            }
        }

        // 4. Movement with exact modifier match.
        if let Some(mov_map) = bindings.movements.get(&event) {
            if let Some(movement) = mov_map.get(&mods) {
                return ResolvedBinding::Movement {
                    movement: *movement,
                    is_combo: !mods.is_empty(),
                };
            }
        }

        // 5. Fallback: base remap (combo=[]) while modifiers are held.
        //    The held modifier output keys must NOT be released — the caller
        //    is responsible for keeping them held (e.g. Ctrl+Enter).
        if let Some(keys) = remap_map.get(&vec![]) {
            let no_pause = bindings.no_pause.contains(&(event, vec![]));
            let while_gaming = bindings.while_gaming.contains(&(event, vec![]));
            let silent = bindings.silent.contains(&(event, vec![]));
            let label = bindings.labels.get(&(event, vec![])).cloned();
            return ResolvedBinding::Keys {
                keys: keys.clone(),
                label,
                no_pause,
                while_gaming,
                silent,
                is_combo: false,
                is_fallback: !mods.is_empty(),
            };
        }
    }

    // 6. Command with exact modifier match (top-level, outside remap block).
    if let Some(cmd_map) = bindings.commands.get(&event) {
        if let Some(commands) = cmd_map.get(&mods) {
            let no_pause = bindings.no_pause.contains(&(event, mods.clone()))
                || bindings.no_pause.contains(&(event, vec![]));
            let while_gaming = bindings.while_gaming.contains(&(event, mods.clone()))
                || bindings.while_gaming.contains(&(event, vec![]));
            let silent = bindings.silent.contains(&(event, mods.clone()))
                || bindings.silent.contains(&(event, vec![]));
            let label = bindings.labels.get(&(event, mods.clone())).cloned();
            return ResolvedBinding::Command {
                commands: commands.clone(),
                label,
                no_pause,
                while_gaming,
                silent,
                is_combo: !mods.is_empty(),
            };
        }
    }

    // 7. Movement with exact modifier match (top-level).
    if let Some(mov_map) = bindings.movements.get(&event) {
        if let Some(movement) = mov_map.get(&mods) {
            return ResolvedBinding::Movement {
                movement: *movement,
                is_combo: !mods.is_empty(),
            };
        }
    }

    ResolvedBinding::Unbound
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Bindings, Event};
    use evdev::Key;
    use std::collections::{HashMap, HashSet};

    fn key_event(k: Key) -> Event {
        Event::Key(k)
    }

    fn make_bindings(
        remap: Vec<(Event, Vec<Event>, Vec<Key>)>,
        commands: Vec<(Event, Vec<Event>, Vec<String>)>,
        no_pause: Vec<(Event, Vec<Event>)>,
        labels: Vec<((Event, Vec<Event>), String)>,
    ) -> Bindings {
        let mut remap_map: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
        for (trigger, combo, keys) in remap {
            remap_map.entry(trigger).or_default().insert(combo, keys);
        }
        let mut cmd_map: HashMap<Event, HashMap<Vec<Event>, Vec<String>>> = HashMap::new();
        for (trigger, combo, cmds) in commands {
            cmd_map.entry(trigger).or_default().insert(combo, cmds);
        }
        Bindings {
            remap: remap_map,
            commands: cmd_map,
            movements: HashMap::new(),
            no_pause: no_pause.into_iter().collect::<HashSet<_>>(),
            while_gaming: HashSet::new(),
            labels: labels.into_iter().collect(),
            silent: HashSet::new(),
        }
    }

    // ── Basic remap ───────────────────────────────────────────────────────────

    #[test]
    fn base_remap_no_modifiers() {
        let btn_south = key_event(Key::BTN_SOUTH);
        let bindings = make_bindings(
            vec![(btn_south, vec![], vec![Key::KEY_ENTER])],
            vec![], vec![], vec![],
        );
        let result = resolve_binding(&bindings, btn_south, &[], false);
        assert_eq!(result, ResolvedBinding::Keys {
            keys: vec![Key::KEY_ENTER],
            label: None,
            no_pause: false,
            while_gaming: false,
            silent: false,
            is_combo: false,
            is_fallback: false,
        });
    }

    // ── Explicit combo ────────────────────────────────────────────────────────

    #[test]
    fn explicit_combo_remap() {
        let btn_north = key_event(Key::BTN_NORTH);
        let btn_tl = key_event(Key::BTN_TL);
        let bindings = make_bindings(
            vec![(btn_north, vec![btn_tl], vec![Key::KEY_LEFTCTRL, Key::KEY_C])],
            vec![], vec![], vec![],
        );
        let result = resolve_binding(&bindings, btn_north, &[btn_tl], false);
        assert_eq!(result, ResolvedBinding::Keys {
            keys: vec![Key::KEY_LEFTCTRL, Key::KEY_C],
            label: None,
            no_pause: false,
            while_gaming: false,
            silent: false,
            is_combo: true,
            is_fallback: false,
        });
    }

    // ── Fallback: modifier held, no combo defined ─────────────────────────────

    #[test]
    fn fallback_remap_keeps_modifier_held() {
        let btn_south = key_event(Key::BTN_SOUTH);
        let btn_tl = key_event(Key::BTN_TL);
        // Only base remap defined, no BTN_TL-BTN_SOUTH combo.
        let bindings = make_bindings(
            vec![(btn_south, vec![], vec![Key::KEY_ENTER])],
            vec![], vec![], vec![],
        );
        let result = resolve_binding(&bindings, btn_south, &[btn_tl], false);
        // Should fall back to base binding, is_fallback=true so caller keeps
        // held modifier output keys held (e.g. KEY_LEFTCTRL stays held → Ctrl+Enter).
        assert_eq!(result, ResolvedBinding::Keys {
            keys: vec![Key::KEY_ENTER],
            label: None,
            no_pause: false,
            while_gaming: false,
            silent: false,
            is_combo: false,
            is_fallback: true,
        });
    }

    // ── Combo takes priority over fallback ────────────────────────────────────

    #[test]
    fn combo_takes_priority_over_base() {
        let btn_north = key_event(Key::BTN_NORTH);
        let btn_tl = key_event(Key::BTN_TL);
        let bindings = make_bindings(
            vec![
                (btn_north, vec![], vec![Key::KEY_BACKSPACE]),
                (btn_north, vec![btn_tl], vec![Key::KEY_LEFTCTRL, Key::KEY_C]),
            ],
            vec![], vec![], vec![],
        );
        let result = resolve_binding(&bindings, btn_north, &[btn_tl], false);
        assert_eq!(result, ResolvedBinding::Keys {
            keys: vec![Key::KEY_LEFTCTRL, Key::KEY_C],
            label: None,
            no_pause: false,
            while_gaming: false,
            silent: false,
            is_combo: true,
            is_fallback: false,
        });
    }

    // ── no_pause ──────────────────────────────────────────────────────────────

    #[test]
    fn no_pause_remap_flagged() {
        let btn_tl2 = key_event(Key::BTN_TL2);
        let bindings = make_bindings(
            vec![(btn_tl2, vec![], vec![Key::BTN_RIGHT])],
            vec![],
            vec![(btn_tl2, vec![])],
            vec![],
        );
        let result = resolve_binding(&bindings, btn_tl2, &[], false);
        match result {
            ResolvedBinding::Keys { no_pause, .. } => assert!(no_pause),
            _ => panic!("expected Keys"),
        }
    }

    #[test]
    fn no_pause_command_flagged() {
        let btn_thumbl = key_event(Key::BTN_THUMBL);
        let bindings = make_bindings(
            vec![],
            vec![(btn_thumbl, vec![], vec!["deckery-hud-toggle".to_string()])],
            vec![(btn_thumbl, vec![])],
            vec![],
        );
        let result = resolve_binding(&bindings, btn_thumbl, &[], false);
        match result {
            ResolvedBinding::Command { no_pause, .. } => assert!(no_pause),
            _ => panic!("expected Command"),
        }
    }

    // ── while_gaming ──────────────────────────────────────────────────────────

    /// A remap with `while_gaming` in bindings.while_gaming must report
    /// `while_gaming: true` in the resolved binding.
    #[test]
    fn while_gaming_remap_flagged() {
        let btn_south = key_event(Key::BTN_SOUTH);
        let mut bindings = make_bindings(
            vec![(btn_south, vec![], vec![Key::KEY_ENTER])],
            vec![], vec![], vec![],
        );
        bindings.while_gaming.insert((btn_south, vec![]));
        let result = resolve_binding(&bindings, btn_south, &[], false);
        match result {
            ResolvedBinding::Keys { while_gaming, .. } => {
                assert!(while_gaming, "while_gaming must be true for a flagged remap")
            }
            _ => panic!("expected Keys"),
        }
    }

    /// A remap *without* `while_gaming` must report `while_gaming: false`.
    #[test]
    fn while_gaming_remap_not_flagged() {
        let btn_south = key_event(Key::BTN_SOUTH);
        let bindings = make_bindings(
            vec![(btn_south, vec![], vec![Key::KEY_ENTER])],
            vec![], vec![], vec![],
        );
        let result = resolve_binding(&bindings, btn_south, &[], false);
        match result {
            ResolvedBinding::Keys { while_gaming, .. } => {
                assert!(!while_gaming, "while_gaming must be false for a non-flagged remap")
            }
            _ => panic!("expected Keys"),
        }
    }

    /// A command with `while_gaming` in bindings.while_gaming must report
    /// `while_gaming: true` in the resolved binding.
    #[test]
    fn while_gaming_command_flagged() {
        let btn_thumbl = key_event(Key::BTN_THUMBL);
        let mut bindings = make_bindings(
            vec![],
            vec![(btn_thumbl, vec![], vec!["deckery-hud-toggle".to_string()])],
            vec![], vec![],
        );
        bindings.while_gaming.insert((btn_thumbl, vec![]));
        let result = resolve_binding(&bindings, btn_thumbl, &[], false);
        match result {
            ResolvedBinding::Command { while_gaming, .. } => {
                assert!(while_gaming, "while_gaming must be true for a flagged command")
            }
            _ => panic!("expected Command"),
        }
    }

    /// A combo binding with `while_gaming` on the base (no-modifier) entry must
    /// still be flagged when resolved with modifiers held, because the resolver
    /// checks both the combo-specific entry *and* the base (no-modifier) entry.
    #[test]
    fn while_gaming_base_entry_applies_to_combo_match() {
        let btn_south = key_event(Key::BTN_SOUTH);
        let btn_tl    = key_event(Key::BTN_TL);
        let mut bindings = make_bindings(
            vec![
                (btn_south, vec![],         vec![Key::KEY_ENTER]),
                (btn_south, vec![btn_tl],   vec![Key::KEY_SPACE]),
            ],
            vec![], vec![], vec![],
        );
        // Flag the base (no-modifier) entry — the resolver's while_gaming
        // check uses `contains(_, vec![])` as a fallback, so the combo
        // resolution must also pick this up.
        bindings.while_gaming.insert((btn_south, vec![]));
        let result = resolve_binding(&bindings, btn_south, &[btn_tl], false);
        match result {
            ResolvedBinding::Keys { while_gaming, is_combo, .. } => {
                assert!(is_combo, "should be resolved as combo");
                assert!(while_gaming, "while_gaming fallback from base entry must apply to combo");
            }
            _ => panic!("expected Keys"),
        }
    }

    // ── label ─────────────────────────────────────────────────────────────────

    #[test]
    fn label_propagated() {
        let btn_thumbl = key_event(Key::BTN_THUMBL);
        let bindings = make_bindings(
            vec![],
            vec![(btn_thumbl, vec![], vec!["deckery-hud-toggle".to_string()])],
            vec![],
            vec![((btn_thumbl, vec![]), "Toggle HUD".to_string())],
        );
        let result = resolve_binding(&bindings, btn_thumbl, &[], false);
        match result {
            ResolvedBinding::Command { label, .. } => {
                assert_eq!(label, Some("Toggle HUD".to_string()))
            }
            _ => panic!("expected Command"),
        }
    }

    // ── Unbound ───────────────────────────────────────────────────────────────

    #[test]
    fn unbound_event_returns_unbound() {
        let bindings = make_bindings(vec![], vec![], vec![], vec![]);
        let result = resolve_binding(&bindings, key_event(Key::BTN_SOUTH), &[], false);
        assert_eq!(result, ResolvedBinding::Unbound);
    }

    // ── Wrong modifier set → Unbound (no fallback if unrelated combo) ─────────

    #[test]
    fn wrong_modifier_set_no_match() {
        let btn_dpad_up = key_event(Key::BTN_DPAD_UP);
        let btn_tl = key_event(Key::BTN_TL);
        let btn_tr = key_event(Key::BTN_TR);
        // Only L1+R1+DPad_Up defined, not L1+DPad_Up
        let bindings = make_bindings(
            vec![(btn_dpad_up, vec![btn_tl, btn_tr], vec![Key::KEY_UP])],
            vec![], vec![], vec![],
        );
        // Only L1 held — should not match
        let result = resolve_binding(&bindings, btn_dpad_up, &[btn_tl], false);
        assert_eq!(result, ResolvedBinding::Unbound);
    }

    // ── Multi-modifier combo ──────────────────────────────────────────────────

    #[test]
    fn multi_modifier_combo() {
        let btn_tl = key_event(Key::BTN_TL);
        let btn_tr = key_event(Key::BTN_TR);
        let btn_dpad_up = key_event(Key::BTN_DPAD_UP);
        let bindings = make_bindings(
            vec![(btn_dpad_up, vec![btn_tl, btn_tr], vec![Key::KEY_UP])],
            vec![], vec![], vec![],
        );
        let result = resolve_binding(&bindings, btn_dpad_up, &[btn_tl, btn_tr], false);
        assert_eq!(result, ResolvedBinding::Keys {
            keys: vec![Key::KEY_UP],
            label: None,
            no_pause: false,
            while_gaming: false,
            silent: false,
            is_combo: true,
            is_fallback: false,
        });
    }

    // ── Hold binding ──────────────────────────────────────────────────────────

    #[test]
    fn hold_fires_when_modifier_held() {
        // Hold binding fires when at least one modifier is held (chain_only=true).
        let btn_south = key_event(Key::BTN_SOUTH);
        let bindings = make_bindings(
            vec![(btn_south, vec![Event::Hold], vec![Key::KEY_ENTER])],
            vec![], vec![], vec![],
        );
        let result = resolve_binding(&bindings, btn_south, &[key_event(Key::BTN_TL)], true);
        assert_eq!(result, ResolvedBinding::Hold { keys: vec![Key::KEY_ENTER] });
    }

    #[test]
    fn hold_suppressed_chain_only_no_mods() {
        // chain_only=true + no modifiers → Hold must not fire; falls through to Unbound.
        let btn_south = key_event(Key::BTN_SOUTH);
        let bindings = make_bindings(
            vec![(btn_south, vec![Event::Hold], vec![Key::KEY_ENTER])],
            vec![], vec![], vec![],
        );
        let result = resolve_binding(&bindings, btn_south, &[], true);
        assert_eq!(result, ResolvedBinding::Unbound);
    }

    #[test]
    fn hold_fires_chain_only_false_no_mods() {
        // chain_only=false → Hold fires even without modifiers.
        let btn_south = key_event(Key::BTN_SOUTH);
        let bindings = make_bindings(
            vec![(btn_south, vec![Event::Hold], vec![Key::KEY_ENTER])],
            vec![], vec![], vec![],
        );
        let result = resolve_binding(&bindings, btn_south, &[], false);
        assert_eq!(result, ResolvedBinding::Hold { keys: vec![Key::KEY_ENTER] });
    }

    // ── Movement binding ──────────────────────────────────────────────────────

    #[test]
    fn movement_base_binding() {
        use crate::config::{Cursor, Relative};
        let trigger = key_event(Key::BTN_SOUTH);
        let movement = Relative::Cursor(Cursor::CURSOR_UP);
        let mut movements = HashMap::new();
        movements.entry(trigger).or_insert_with(HashMap::new).insert(vec![], movement);
        let bindings = Bindings {
            remap: HashMap::new(),
            commands: HashMap::new(),
            movements,
            no_pause: std::collections::HashSet::new(),
            while_gaming: std::collections::HashSet::new(),
            labels: HashMap::new(),
            silent: std::collections::HashSet::new(),
        };
        let result = resolve_binding(&bindings, trigger, &[], false);
        assert_eq!(result, ResolvedBinding::Movement { movement, is_combo: false });
    }

    #[test]
    fn movement_combo() {
        use crate::config::{Cursor, Relative};
        let trigger = key_event(Key::BTN_SOUTH);
        let btn_tl = key_event(Key::BTN_TL);
        let movement = Relative::Cursor(Cursor::CURSOR_RIGHT);
        let mut movements = HashMap::new();
        movements.entry(trigger).or_insert_with(HashMap::new).insert(vec![btn_tl], movement);
        let bindings = Bindings {
            remap: HashMap::new(),
            commands: HashMap::new(),
            movements,
            no_pause: std::collections::HashSet::new(),
            while_gaming: std::collections::HashSet::new(),
            labels: HashMap::new(),
            silent: std::collections::HashSet::new(),
        };
        let result = resolve_binding(&bindings, trigger, &[btn_tl], false);
        assert_eq!(result, ResolvedBinding::Movement { movement, is_combo: true });
    }

    // ── Top-level command (no remap block for this event) ─────────────────────

    #[test]
    fn top_level_command_no_remap_block() {
        // Event is not in bindings.remap at all — command must still be found (step 6).
        let btn_thumbl = key_event(Key::BTN_THUMBL);
        let bindings = make_bindings(
            vec![],  // no remap entry for btn_thumbl
            vec![(btn_thumbl, vec![], vec!["hud-toggle".to_string()])],
            vec![], vec![],
        );
        let result = resolve_binding(&bindings, btn_thumbl, &[], false);
        assert_eq!(result, ResolvedBinding::Command {
            commands: vec!["hud-toggle".to_string()],
            label: None,
            no_pause: false,
            while_gaming: false,
            silent: false,
            is_combo: false,
        });
    }

    // ── Command under remap block takes priority over base remap fallback ──────

    #[test]
    fn command_under_remap_beats_fallback() {
        // Event IS in bindings.remap (base mapping) AND has a combo command.
        // With the combo modifier held, the command (step 3) must win over the
        // base remap fallback (step 5).
        let btn_dpad_up = key_event(Key::BTN_DPAD_UP);
        let btn_tl = key_event(Key::BTN_TL);
        let bindings = make_bindings(
            vec![(btn_dpad_up, vec![], vec![Key::KEY_UP])],
            vec![(btn_dpad_up, vec![btn_tl], vec!["previous-desktop".to_string()])],
            vec![], vec![],
        );
        let result = resolve_binding(&bindings, btn_dpad_up, &[btn_tl], false);
        assert_eq!(result, ResolvedBinding::Command {
            commands: vec!["previous-desktop".to_string()],
            label: None,
            no_pause: false,
            while_gaming: false,
            silent: false,
            is_combo: true,
        });
    }

    // ── silent = true on a command ────────────────────────────────────────────

    #[test]
    fn silent_command_resolved_with_silent_true() {
        let btn_thumbl = key_event(Key::BTN_THUMBL);
        let mut bindings = make_bindings(
            vec![],
            vec![(btn_thumbl, vec![], vec!["hud-toggle".to_string()])],
            vec![], vec![],
        );
        bindings.silent.insert((btn_thumbl, vec![]));
        let result = resolve_binding(&bindings, btn_thumbl, &[], false);
        assert_eq!(result, ResolvedBinding::Command {
            commands: vec!["hud-toggle".to_string()],
            label: None,
            no_pause: false,
            while_gaming: false,
            silent: true,
            is_combo: false,
        });
    }

    #[test]
    fn non_silent_command_resolved_with_silent_false() {
        let btn_thumbl = key_event(Key::BTN_THUMBL);
        let bindings = make_bindings(
            vec![],
            vec![(btn_thumbl, vec![], vec!["hud-toggle".to_string()])],
            vec![], vec![],
        );
        let result = resolve_binding(&bindings, btn_thumbl, &[], false);
        assert_eq!(result, ResolvedBinding::Command {
            commands: vec!["hud-toggle".to_string()],
            label: None,
            no_pause: false,
            while_gaming: false,
            silent: false,
            is_combo: false,
        });
    }
}
