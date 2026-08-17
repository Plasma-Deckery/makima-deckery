//! Haptic chain API for the Steam Deck controller.
//!
//! Defines the public haptic types and the internal player task that evaluates
//! chains and emits individual wire-level commands to the hidraw writer.
//!
//! ## Public API surface
//!
//! External callers (mt_trackpad, gesture_pad, event_reader) send
//! `HapticRequest { pad, chain }` through `ControllerSession::haptic_tx`.
//! The chain is evaluated — including inter-step sleeps — inside this module's
//! player task, so callers return immediately after sending and are never
//! blocked by haptic timing.
//!
//! ## Internal pipeline
//!
//! ```text
//! [caller] → Sender<HapticRequest>
//!                ↓
//! [run_haptic_player]  evaluates chain, sleeps between steps
//!                ↓  (pub(super)) Sender<HapticCommand>
//! [run_hidraw_writer]  serialises onto the shared hidraw fd
//! ```
//!
//! `HapticCommand` is the wire-level primitive — it is never exposed outside
//! this module group. Callers always work with `HapticChain` / `HapticPulse`.

use std::time::Duration;
use tokio::sync::mpsc;

// ── HapticPad ─────────────────────────────────────────────────────────────────

/// Which pad(s) a haptic pulse should play on.
///
/// Wire values are swapped relative to the hid-steam.c constant names
/// (`STEAM_PAD_LEFT`/`RIGHT` = 0/1): on-hardware testing (2026-07-09) showed
/// pressing the right pad buzzing the left actuator with the "obvious" values,
/// so `Left → 1` and `Right → 0`. `Both` is unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HapticPad {
    Left,
    Right,
    Both,
}

impl HapticPad {
    pub(super) fn wire_value(self) -> u8 {
        match self {
            HapticPad::Left  => 1,
            HapticPad::Right => 0,
            HapticPad::Both  => 2,
        }
    }
}

// ── HapticCommand (internal wire primitive) ────────────────────────────────────

/// One haptic burst on one pad — the wire-level primitive sent from the player
/// to the writer. Never exposed outside `steam_deck_controller/`.
#[derive(Debug, Clone, Copy)]
pub(super) struct HapticCommand {
    pub(super) pad:         HapticPad,
    pub(super) duration_us: u16,
    pub(super) interval_us: u16,
    pub(super) count:       u16,
    pub(super) gain_db:     i8,
}

// ── HapticPulse ───────────────────────────────────────────────────────────────

/// Parameters of a single haptic burst — how one "tick" of feedback feels.
///
/// Deserialised from the handler config TOML sub-tables
/// (`[trackpad.left.haptic]`, `[trackpad.gestures.haptic]`, etc.).
///
/// Default: a three-pulse burst (8 ms on / 8 ms off), tuned on real Steam Deck
/// hardware (2026-07-10) against the Lizard Mode click buzz as a reference
/// feel. `gain_db` defaults to 0 — on-hardware A/B testing showed it has no
/// perceptible effect on this hardware (see issue #20), so the meaningful
/// levers are `duration_us`, `interval_us`, and `count`.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct HapticPulse {
    #[serde(default = "default_duration_us")]
    pub duration_us: u16,
    #[serde(default = "default_interval_us")]
    pub interval_us: u16,
    #[serde(default = "default_count")]
    pub count: u16,
    #[serde(default)]
    pub gain_db: i8,
}

fn default_duration_us() -> u16 { 8000 }
fn default_interval_us() -> u16 { 8000 }
fn default_count()       -> u16 { 3 }

impl Default for HapticPulse {
    fn default() -> Self {
        Self { duration_us: 8000, interval_us: 8000, count: 3, gain_db: 0 }
    }
}

// ── HapticChainStep ───────────────────────────────────────────────────────────

/// One step in a `HapticChain`: a single burst followed by an optional pause
/// before the next step. The last step in a chain typically omits `pause_ms`.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct HapticChainStep {
    #[serde(flatten)]
    pub pulse: HapticPulse,
    /// Milliseconds to wait after this pulse before firing the next one.
    /// Omit (or set `None`) on the last step.
    pub pause_ms: Option<u64>,
}

// ── HapticChain ───────────────────────────────────────────────────────────────

/// A sequence of haptic pulses with optional inter-pulse pauses.
///
/// Deserialises from two TOML forms — existing single-pulse configs need no
/// migration:
///
/// ```toml
/// # Single pulse (backward-compatible short form):
/// on_press = { duration_us = 8000, count = 1 }
///
/// # Multi-step chain with pauses:
/// on_press = [
///     { duration_us = 8000, count = 1, pause_ms = 150 },
///     { duration_us = 8000, count = 1 },
/// ]
/// ```
///
/// Chain evaluation (including `pause_ms` sleeps) happens inside the
/// controller's haptic player task — callers send the chain and return
/// immediately.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
pub enum HapticChain {
    /// Single burst — the common case and the backward-compatible form.
    Single(HapticPulse),
    /// Ordered sequence; each step may pause before the next.
    Chain(Vec<HapticChainStep>),
}

impl Default for HapticChain {
    fn default() -> Self { Self::Single(HapticPulse::default()) }
}

impl HapticChain {
    /// Convenience constructor for a single-pulse chain.
    pub fn single(pulse: HapticPulse) -> Self { Self::Single(pulse) }
}

// ── HapticRequest ─────────────────────────────────────────────────────────────

/// A haptic playback request sent by callers to `ControllerSession::haptic_tx`.
///
/// Specifies which pad(s) to vibrate and what chain to play. Evaluation
/// (including inter-step sleeps) happens inside the controller.
#[derive(Debug, Clone)]
pub struct HapticRequest {
    pub pad:   HapticPad,
    pub chain: HapticChain,
}

// ── Haptic player task ────────────────────────────────────────────────────────

/// Runs the haptic player: receives `HapticRequest`s, evaluates chains
/// (sleeping between steps), and sends individual `HapticCommand`s to the
/// hidraw writer task.
///
/// Requests are processed sequentially — overlapping chains are naturally
/// queued. Exits when `rx` closes (all senders dropped, i.e. session ending).
pub(super) async fn run_haptic_player(
    mut rx:     mpsc::Receiver<HapticRequest>,
    cmd_tx:     mpsc::Sender<HapticCommand>,
) {
    while let Some(req) = rx.recv().await {
        match req.chain {
            HapticChain::Single(p) => {
                let _ = cmd_tx.send(to_command(req.pad, p)).await;
            }
            HapticChain::Chain(steps) => {
                for step in &steps {
                    let _ = cmd_tx.send(to_command(req.pad, step.pulse)).await;
                    if let Some(ms) = step.pause_ms {
                        if ms > 0 {
                            tokio::time::sleep(Duration::from_millis(ms)).await;
                        }
                    }
                }
            }
        }
    }
}

fn to_command(pad: HapticPad, pulse: HapticPulse) -> HapticCommand {
    HapticCommand {
        pad,
        duration_us: pulse.duration_us,
        interval_us: pulse.interval_us,
        count:       pulse.count,
        gain_db:     pulse.gain_db,
    }
}
