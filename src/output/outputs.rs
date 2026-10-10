//! Five ports. Each one holds the real thing, or nothing.
//!
//! The part that decides returns a `Vec<OutputAction>` and touches no device, no
//! socket and no process. This takes that list and pushes each action through the
//! port for its kind. An empty port means the action has nowhere to go and
//! nothing happens — which is what lets a test run with no `/dev/uinput` and no
//! session bus, and still see exactly what was decided.
//!
//! ```text
//!   Vec<OutputAction> ──▶ Outputs ──┬─ devices  ──▶ uinput
//!                                   ├─ state    ──▶ state.json
//!                                   ├─ commands ──▶ user session
//!                                   ├─ internal ──▶ back in as the next input
//!                                   ├─ haptics  ──▶ hidraw
//!                                   └─ tap      ──▶ the log, or a test
//! ```
//!
//! No trait and no registry: a port holds a concrete thing, the owner is allowed
//! to know what that thing is, and anything that only wants to watch takes the
//! tap. `internal` is the one port that loops back — Gaming Mode leaves as an
//! action and returns as the next input — and that is an advantage for testing
//! rather than a wart, because it means a mode change is observable as an action
//! before it is believed as state.
//!
//! Frames are handed over as a batch: the device lock is taken once, so every
//! frame of one decision reaches the kernel with nothing else in between.

use super::action::{Emission, InternalCommand, OutputAction, StateAction, Urgency};
use crate::session::Environment;
use crate::state_export::LastAction;
use crate::state_writer::{StateCommand, StateWriterHandle};
use crate::virtual_devices::VirtualDevices;

use deckery_controller::HapticRequest;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, Mutex};

/// Everything an action can be pushed through.
#[derive(Default, Clone)]
pub struct Outputs {
    /// The kernel actuator. Empty means nothing is emitted and no uinput node
    /// has to exist.
    pub devices: Option<Arc<Mutex<VirtualDevices>>>,
    /// Snapshots and `last_action`, on their way to state.json.
    pub state: Option<StateWriterHandle>,
    /// Command bindings, run in the user's session.
    pub commands: Option<Environment>,
    /// Gaming Mode, back into the machine on its next step.
    pub internal: Option<mpsc::Sender<bool>>,
    /// Feedback to the controller.
    pub haptics: Option<mpsc::Sender<HapticRequest>>,
    /// Sees every action, whatever the ports do with it.
    pub tap: Option<Tap>,
}

/// The watching end: one channel, used by the log in production and by a test
/// otherwise.
#[derive(Clone)]
pub struct Tap {
    tx: mpsc::Sender<Emission>,
    seq: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
}

impl Tap {
    pub fn new(tx: mpsc::Sender<Emission>) -> Self {
        Self { tx, seq: Arc::new(AtomicU64::new(0)), dropped: Arc::new(AtomicU64::new(0)) }
    }

    /// How many emissions did not fit.
    ///
    /// A recording with a silent gap would let a later comparison claim an
    /// agreement it never saw, so the count is readable and the log writes it
    /// down as its own line.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Never blocks: this runs inside the owner, which may hold the device lock.
    fn offer(&self, site: &'static str, action: &OutputAction) {
        let emission = Emission {
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            t_us: crate::startup_us(),
            site,
            action: action.clone(),
        };
        if self.tx.try_send(emission).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Outputs {
    /// Whether anything would see an action.
    ///
    /// Checked before optional work is done, so a run with nothing watching and
    /// nothing to clone costs one `Option` check.
    pub fn watched(&self) -> bool {
        self.tap.is_some()
    }

    /// Push every action through its port.
    pub async fn dispatch(&self, site: &'static str, actions: &[OutputAction]) {
        if let Some(tap) = &self.tap {
            for action in actions {
                tap.offer(site, action);
            }
        }

        // Frames first and under one lock, so one decision's frames are
        // contiguous as the kernel reads them.
        let emits = |a: &OutputAction| {
            matches!(a, OutputAction::Frame { .. } | OutputAction::Forward { .. })
        };
        if let Some(devices) = &self.devices {
            if actions.iter().any(emits) {
                let mut devices = devices.lock().await;
                for action in actions {
                    match action {
                        // Synthesised, so it is stamped as it goes out — which is
                        // what the old path did with `new_now`.
                        OutputAction::Frame { device, events } => {
                            let stamped: Vec<evdev::InputEvent> = events
                                .iter()
                                .map(|e| evdev::InputEvent::new_now(
                                    evdev::EventType(e.ev_type), e.code, e.value,
                                ))
                                .collect();
                            let _ = devices.emit(*device, &stamped);
                        }
                        // Forwarded, so it keeps the stamp it arrived with.
                        OutputAction::Forward { device, events } => {
                            let raw: Vec<evdev::InputEvent> =
                                events.iter().map(|r| r.0).collect();
                            let _ = devices.emit(*device, &raw);
                        }
                        _ => {}
                    }
                }
            }
        }

        for action in actions {
            match action {
                // Done above.
                OutputAction::Frame { .. } | OutputAction::Forward { .. } => {}

                OutputAction::Internal(InternalCommand::SetGamingMode { enabled, .. }) => {
                    if let Some(tx) = &self.internal {
                        let _ = tx.send(*enabled).await;
                    }
                }

                OutputAction::Haptic { pad, chain } => {
                    if let Some(tx) = &self.haptics {
                        let _ = tx.send(HapticRequest { pad: *pad, chain: chain.clone() }).await;
                    }
                }

                // Not wired yet, and for a reason worth settling rather than
                // papering over: `run_command_binding` reports a failure by
                // writing `last_action` and asking for a state write, which is
                // two effects performed from inside a port. It should instead
                // produce an `OutputAction` of its own, late — which needs a way
                // back into `dispatch`. That lands with the conversion.
                OutputAction::Command { .. } => {}

                OutputAction::State(StateAction::Publish { snapshot, urgency }) => {
                    if let Some(tx) = &self.state {
                        let command = StateCommand::SetEventState(Some((**snapshot).clone()));
                        match urgency {
                            // An analog tick at 60 Hz: dropped rather than
                            // allowed to hold up the event loop.
                            Urgency::Analog => { let _ = tx.try_send(command); }
                            // A discrete event: must not be lost.
                            Urgency::Immediate => { let _ = tx.send(command).await; }
                        }
                    }
                }

                // The owner holds `last_action` itself, because the next snapshot
                // it assembles has to contain it. Pushing it through a port would
                // mean asking the port to read it back.
                OutputAction::State(StateAction::SetLastAction(_)) => {}
            }
        }
    }
}

/// A `LastAction` from a spec plus the moment it happened.
///
/// The spec carries no timestamp: `now_ts()` is wall clock, and keeping it would
/// make two runs of the same decision differ for a reason that has nothing to do
/// with the decision.
pub fn last_action_at(spec: super::action::LastActionSpec, ts: f64) -> LastAction {
    LastAction {
        r#type: spec.r#type,
        value: spec.value,
        ts,
        label: spec.label,
        silent: spec.silent,
    }
}

#[cfg(test)]
#[path = "outputs_tests.rs"]
mod tests;
