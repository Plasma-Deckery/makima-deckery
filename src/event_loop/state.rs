//! Gathering what the overlay reads.
//!
//! `state_export::build_state` is pure and assembles the snapshot; this is the
//! half that collects the live values for it — stick positions, trackpads, the
//! IMU, held keys, modifiers, `last_action`, the config stack.
//!
//! It stays inside the reader rather than being assembled from outside, and the
//! reason is ownership: every one of those values sits behind a mutex this type
//! owns, so an outside assembler would have to take the same locks. There would
//! be nothing to gain and a lock order to get wrong.

use super::*;

/// Below this, a change in a normalized stick or pad value is not worth a
/// write. A resting stick wanders by a few thousandths inside its deadzone,
/// and with the analog export on that alone was ~45 full state.json writes a
/// second. Five thousandths is a fraction of a pixel in the HUD's stick ring,
/// and the comparison is against the last *written* value, so slow drift still
/// lands once it adds up. The values sit on `r3`'s 0.001 grid; the half step
/// keeps f32 error from deciding whether a step of exactly five counts.
const ANALOG_WRITE_EPSILON: f32 = 0.0045;

/// Whether any value moved by at least `ANALOG_WRITE_EPSILON` since `prev`.
/// A NaN in `prev` (nothing written yet) always counts as moved.
pub(super) fn analog_moved(prev: &[f32; 8], new: &[f32; 8]) -> bool {
    prev.iter().zip(new).any(|(a, b)| !((a - b).abs() < ANALOG_WRITE_EPSILON))
}

/// What `build_state` was last called with, and what it returned.
///
/// `build_state` walks every binding of the merged config, and an analog tick
/// changes none of its inputs — only the trackpads/sticks/imu laid over it
/// afterwards. With the export on that rebuild ran up to 90 times a second
/// for an identical result. The config compares by pointer: a new config is
/// always a new `Arc`, never one mutated in place.
pub(super) struct StateCache {
    pub(super) config:       Arc<Config>,
    pub(super) modifiers:    Vec<Event>,
    pub(super) paused:       bool,
    pub(super) gaming_mode:  bool,
    pub(super) held_keys:    Vec<Event>,
    pub(super) last_action:  Option<LastAction>,
    pub(super) config_stack: Vec<String>,
    pub(super) state:        serde_json::Value,
}

impl StateCache {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn matches(
        &self,
        config: &Arc<Config>,
        modifiers: &[Event],
        paused: bool,
        gaming_mode: bool,
        held_keys: &[Event],
        last_action: &Option<LastAction>,
        config_stack: &[String],
    ) -> bool {
        Arc::ptr_eq(&self.config, config)
            && self.modifiers == modifiers
            && self.paused == paused
            && self.gaming_mode == gaming_mode
            && self.held_keys == held_keys
            && self.last_action == *last_action
            && self.config_stack == config_stack
    }
}

impl EventReader {
    pub(super) async fn write_state(&self) {
        self.write_state_inner(false).await;
    }

    pub(super) async fn write_state_analog(&self) {
        if !*self.analog_state_export.lock().await {
            return;
        }
        self.write_state_inner(true).await;
    }

    pub(super) async fn write_state_inner(&self, analog_triggered: bool) {
        // Each lock acquisition is guarded by a timeout so that a deadlock
        // (same task trying to re-acquire a lock it already holds) surfaces
        // immediately in the journal instead of silently freezing makima.
        const TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

        let config = match tokio::time::timeout(TIMEOUT, self.current_config.lock()).await {
            Ok(guard) => guard.clone(),
            Err(_) => {
                eprintln!("deckery: write_state: current_config lock timed out — possible deadlock");
                return;
            }
        };
        let modifiers = match tokio::time::timeout(TIMEOUT, self.modifiers.lock()).await {
            Ok(guard) => guard.clone(),
            Err(_) => {
                eprintln!("deckery: write_state: modifiers lock timed out — possible deadlock");
                return;
            }
        };

        let paused = match tokio::time::timeout(TIMEOUT, self.paused.lock()).await {
            Ok(guard) => *guard,
            Err(_) => {
                eprintln!("deckery: write_state: paused lock timed out — possible deadlock");
                return;
            }
        };
        let gaming_mode = match tokio::time::timeout(TIMEOUT, self.gaming_mode.lock()).await {
            Ok(guard) => *guard,
            Err(_) => {
                eprintln!("deckery: write_state: gaming_mode lock timed out — possible deadlock");
                return;
            }
        };
        let last_action = match tokio::time::timeout(TIMEOUT, self.last_action.lock()).await {
            Ok(guard) => guard.clone(),
            Err(_) => {
                eprintln!("deckery: write_state: last_action lock timed out — possible deadlock");
                return;
            }
        };
        let held_keys = match tokio::time::timeout(TIMEOUT, self.held_keys.lock()).await {
            Ok(guard) => guard.clone(),
            Err(_) => {
                eprintln!("deckery: write_state: held_keys lock timed out — possible deadlock");
                return;
            }
        };
        let base_name = self.base_name.clone();
        let config_stack = if config.name == base_name {
            vec![base_name]
        } else {
            vec![base_name, config.name.clone()]
        };
        let lpad_pos =                     *self.lpad.position.lock().await;
        let lpad_pressed = *self.lpad.pressed.lock().await;
        let rpad_pos =                     *self.rpad.position.lock().await;
        let rpad_pressed = *self.rpad.pressed.lock().await;
        let lpad_x = crate::analog::normalize(lpad_pos.0);
        let lpad_y = crate::analog::normalize(lpad_pos.1);
        let rpad_x = crate::analog::normalize(rpad_pos.0);
        let rpad_y = crate::analog::normalize(rpad_pos.1);
        let lstick_raw = *self.lstick_raw.lock().await;
        let rstick_raw = *self.rstick_raw.lock().await;
        let imu_raw = *self.imu_raw.lock().await;
        let analog_state_export = *self.analog_state_export.lock().await;
        let lstick_dz = crate::analog::normalize_dz(self.settings.lstick.deadzone);
        let rstick_dz = crate::analog::normalize_dz(self.settings.rstick.deadzone);
        let lstick_x = crate::analog::normalize(lstick_raw.0);
        let lstick_y = crate::analog::normalize_y(lstick_raw.1); // negated: hardware up = negative ABS_Y
        let rstick_x = crate::analog::normalize(rstick_raw.0);
        let rstick_y = crate::analog::normalize_y(rstick_raw.1); // negated: hardware up = negative ABS_Y
        // For analog-triggered writes (SYN_REPORT): skip unless something moved
        // visibly since the last write — see `analog_moved`.
        // For button-triggered writes: always write, and take the snapshot along.
        let new_snap: [f32; 8] = [
            lstick_x, lstick_y, rstick_x, rstick_y,
            lpad_x, lpad_y, rpad_x, rpad_y,
        ];
        {
            let mut snap = self.analog_snapshot.lock().await;
            if analog_triggered && !analog_moved(&snap, &new_snap) {
                return;
            }
            *snap = new_snap;
        }
        let gesture_touching = *self.gesture_session.lock().await;
        // During an active gesture session both pads operate as a combined device —
        // report "gesture" as their mode so the HUD knows not to render them individually.
        let lpad_mode = if gesture_touching {
            "gestures"
        } else {
            self.trackpad.left.mode.as_str()
        };
        let rpad_mode = if gesture_touching {
            "gestures"
        } else {
            self.trackpad.right.mode.as_str()
        };
        // During a gesture session the individual pad devices are not sending events —
        // suppress their touching flag so the HUD doesn't render them as independent
        // single-finger actions at the same time as the gesture indicator.
        let lpad_touching = !gesture_touching &&                     *self.lpad.touching_hw.lock().await;
        let rpad_touching = !gesture_touching &&                     *self.rpad.touching_hw.lock().await;
        let trackpads = serde_json::json!({
            "left": {
                "mode": lpad_mode,
                "x": lpad_x,
                "y": lpad_y,
                "touching": lpad_touching,
                "pressed": lpad_pressed,
            },
            "right": {
                "mode": rpad_mode,
                "x": rpad_x,
                "y": rpad_y,
                "touching": rpad_touching,
                "pressed": rpad_pressed,
            },
            "gesture": {
                "enabled": self.trackpad.combined_gesture_device,
                "touching": gesture_touching,
            },
        });
        let sticks = serde_json::json!({
            "lstick": {
                "mode": self.settings.lstick.function,
                "x": lstick_x,
                "y": lstick_y,
                "deadzone": lstick_dz,
                "active": crate::analog::is_active(lstick_x, lstick_y, lstick_dz),
            },
            "rstick": {
                "mode": self.settings.rstick.function,
                "x": rstick_x,
                "y": rstick_y,
                "deadzone": rstick_dz,
                "active": crate::analog::is_active(rstick_x, rstick_y, rstick_dz),
            },
        });
        // IMU: normalize 0..32767 → 0.0..1.0 (unsigned range).
        let imu = serde_json::json!({
            "x": crate::analog::normalize(imu_raw.0),
            "y": crate::analog::normalize(imu_raw.1),
        });
        let mut event_state = {
            let mut cache = self.state_cache.lock().await;
            match cache.as_ref() {
                Some(c) if c.matches(&config, &modifiers, paused, gaming_mode,
                                     &held_keys, &last_action, &config_stack) => c.state.clone(),
                _ => {
                    let state = crate::state_export::build_state(
                        &config, &modifiers, paused, gaming_mode,
                        &held_keys, &last_action, &config_stack, &self.gaming_mode_config,
                    );
                    *cache = Some(StateCache {
                        config: config.clone(), modifiers, paused, gaming_mode,
                        held_keys, last_action, config_stack, state: state.clone(),
                    });
                    state
                }
            }
        };
        event_state["trackpads"] = trackpads;
        event_state["sticks"]    = sticks;
        event_state["imu"]       = imu;
        event_state["context"]["analog_state_export"] = serde_json::Value::Bool(analog_state_export);
        // Through the state port like everything else, carrying both the
        // snapshot and how urgently it is meant: an analog tick at 60 Hz is
        // dropped rather than allowed to hold up the event loop, a button or a
        // pause must never be lost.
        self.outputs.dispatch(&[crate::output::action::OutputAction::State(
            crate::output::action::StateAction::Publish {
                snapshot: Box::new(event_state),
                urgency: if analog_triggered {
                    crate::output::action::Urgency::Analog
                } else {
                    crate::output::action::Urgency::Immediate
                },
            },
        )]).await;
    }

    /// The hint `trigger` carries under the buttons held right now.
    ///
    /// Same inputs and same rule as the HUD's binding grid — both go through
    /// `state_export::matching_hint`, so a hint cannot be shown in one place
    /// and missed in the other.
    pub(super) async fn hint_label(&self, trigger: &Event) -> Option<String> {
        let config = self.current_config.lock().await.clone();
        if config.bindings.hints_resolved.is_empty() {
            return None;
        }
        let modifiers = self.modifiers.lock().await.clone();
        let held_keys = self.held_keys.lock().await.clone();
        let active = crate::state_export::active_input_modifiers(&config, &modifiers);
        let hint_mods = crate::state_export::hint_modifiers(&config, &active, &held_keys);
        crate::state_export::matching_hint(&config, trigger, &hint_mods)
            .map(|hint| hint.label.clone())
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
