//! Multi-touch trackpad emulation handler.
//!
//! `steam_deck_controller/hidraw.rs` is a pure raw-frame *producer*: it
//! knows nothing about trackpad modes, gesture sessions, or haptics — it just
//! turns hidraw reports into `PadFrame`s. `trackpad_router.rs` owns Core
//! routing: gesture-session entry/exit, click-edge-independent state.json
//! export, and deciding which per-channel handler input each frame goes to.
//!
//! This module is the *consumer/interpreter* downstream of the router: it
//! turns a `SinglePadFrame` stream into virtual MT device events and
//! click-edge haptic feedback. Haptic chain evaluation (including inter-step
//! sleeps) happens inside the controller's haptic player task — this module
//! simply sends `HapticRequest`s and returns immediately.
//!
//! The haptic types (`HapticPulse`, `HapticChain`, `HapticChainStep`) live in
//! `steam_deck_controller/haptic.rs` so any tool (makima, deckery-auth, etc.)
//! can play chains without re-implementing the evaluation logic.
use deckery_controller::{HapticChain, HapticPad, HapticPulse};
use crate::output::action::OutputAction;
use crate::output::Outputs;
use crate::trackpad::PadState;
use crate::trackpad_router::SinglePadFrame;
use tokio::sync::mpsc;

fn default_movement_pixel_interval() -> u32 { 3000 }

/// Distance-gated movement haptic: the pulse to fire plus the raw-unit
/// distance threshold that gates it. Kept as its own struct (not a bare
/// `HapticPulse`) for the same reason as `gesture_pad::GestureMoveHaptic` —
/// movement has no natural single trigger instant, so it needs its own firing
/// policy alongside the pulse shape. Live-tested naive time-based throttling
/// produced a continuous buzz; distance-gating means faster movement ticks
/// more often and slow movement ticks rarely, matching Steam Input's own
/// trackpad feedback feel.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct MovementHaptic {
    #[serde(default = "default_movement_pixel_interval")]
    pub pixel_interval: u32,
    #[serde(flatten)]
    pub pulse: HapticPulse,
}

/// Haptic policy for this handler: which events fire a pulse, and with what
/// parameters. A physical click is really two distinct edges — press and
/// release — and they deserve independent chains rather than being collapsed
/// into a single "on_click". `on_movement` gates on cumulative pixel distance
/// (see `MovementHaptic`) rather than time, to avoid continuous buzzing.
/// `on_movement` intentionally keeps a single `HapticPulse` (not a chain):
/// it fires on every distance threshold crossing, so a multi-step chain would
/// overlap itself under fast movement.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct HapticConfig {
    pub on_press:    Option<HapticChain>,
    pub on_release:  Option<HapticChain>,
    pub on_movement: Option<MovementHaptic>,
}

/// This handler's own config, self-deserialized from the raw `handler_config`
/// TOML sub-table that Core (`config.rs`) hands it unparsed — Core only knows
/// `mode` and `click_pressure`; everything below is owned by this module.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct MtTrackpadConfig {
    #[serde(default)]
    pub haptic: HapticConfig,
}

impl MtTrackpadConfig {
    /// Parses `value` (a `[trackpad.left]`/`[trackpad.right]` TOML table) into
    /// this handler's config. Falls back to defaults (and logs a warning) on
    /// any shape mismatch — a config typo in a handler-owned field must never
    /// crash makima or block input handling on the pad.
    pub fn from_toml_value(value: &toml::Value) -> Self {
        value.clone().try_into::<MtTrackpadConfig>().unwrap_or_else(|e| {
            eprintln!("[makima] Warning: invalid mt-trackpad handler config ({}), using defaults.", e);
            MtTrackpadConfig::default()
        })
    }

    /// The haptic chain to fire on the press edge — user-configured `on_press`,
    /// or the built-in default single-pulse chain.
    pub fn press_chain(&self) -> HapticChain {
        self.haptic.on_press.clone().unwrap_or_default()
    }

    /// The haptic chain to fire on the release edge — user-configured
    /// `on_release`, or the same built-in default as `press_chain`.
    /// Returns `Option` so a user can silence release-only by omitting it,
    /// while press still fires.
    pub fn release_chain(&self) -> Option<HapticChain> {
        Some(self.haptic.on_release.clone().unwrap_or_default())
    }

    /// The movement pulse/distance-gate policy, if configured. `None` by
    /// default — movement haptics have no established "feel" to fall back to,
    /// unlike click edges, so silence is the right default.
    pub fn movement_pulse(&self) -> Option<MovementHaptic> {
        self.haptic.on_movement
    }
}

// ── Haptic helpers ────────────────────────────────────────────────────────────

/// A single-step haptic, as an action.
///
/// `pub(crate)`: shared with `gesture_pad.rs`, which wraps its pulses the same
/// way.
pub(crate) fn pulse(pad: HapticPad, pulse: HapticPulse) -> OutputAction {
    OutputAction::Haptic { pad, chain: HapticChain::single(pulse) }
}

/// A whole chain, as an action. The controller's haptic player evaluates it
/// (including the inter-step sleeps) once `Outputs::dispatch` hands it over, so
/// deciding one costs nothing here.
pub(crate) fn chain(pad: HapticPad, chain: &HapticChain) -> OutputAction {
    OutputAction::Haptic { pad, chain: chain.clone() }
}

/// What the click and movement haptics have to remember between frames.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HapticProgress {
    prev_click: bool,
    /// Only tracked while a movement pulse is configured, which is why it stays
    /// `None` forever when none is.
    prev_pos: Option<(i32, i32)>,
    move_accum: f64,
}

/// Decide what one frame should feel like.
///
/// Pure: takes the progress so far, returns the next progress and whatever
/// should be felt, in the order it should be felt. Nothing is sent.
///
/// Press and release are two independent chains rather than one "on_click",
/// because a real click mechanism has two distinct, separately-feelable edges.
pub fn plan_haptics(
    progress: HapticProgress,
    frame: &SinglePadFrame,
    pad: HapticPad,
    press_chain: &HapticChain,
    release_chain: Option<&HapticChain>,
    movement_pulse: Option<MovementHaptic>,
) -> (HapticProgress, Vec<OutputAction>) {
    let mut next = progress;
    let mut actions = Vec::new();

    let press_edge   = frame.click && !progress.prev_click;
    let release_edge = !frame.click && progress.prev_click;
    next.prev_click  = frame.click;

    if press_edge {
        actions.push(chain(pad, press_chain));
    } else if release_edge {
        if let Some(c) = release_chain {
            actions.push(chain(pad, c));
        }
    }

    if let Some(m) = movement_pulse {
        if frame.touching {
            if let Some((px, py)) = progress.prev_pos {
                let dx = (frame.x - px) as f64;
                let dy = (frame.y - py) as f64;
                next.move_accum += (dx * dx + dy * dy).sqrt();
                if next.move_accum >= m.pixel_interval as f64 {
                    actions.push(pulse(pad, m.pulse));
                    next.move_accum = 0.0;
                }
            }
            next.prev_pos = Some((frame.x, frame.y));
        } else {
            next.prev_pos   = None;
            next.move_accum = 0.0;
        }
    }

    (next, actions)
}

// ── Single-pad MT handler ─────────────────────────────────────────────────────

/// Runs one individual (non-gesture) MT trackpad handler until `rx` closes.
/// Consumes `SinglePadFrame`s already routed to this pad by
/// `trackpad_router::run` and emits them to `pad`'s virtual MT device,
/// firing a haptic tick on the press edge (rising) and, if configured,
/// another on the release edge (falling) of `click` — press and release are
/// deliberately two independent pulses rather than one "on_click", since a
/// real click mechanism has two distinct, separately-feelable edges.
pub async fn run_single(
    mut rx:          mpsc::Receiver<SinglePadFrame>,
    outputs:         &Outputs,
    pad:             &PadState,
    haptic_pad:      HapticPad,
    press_chain:     HapticChain,
    release_chain:   Option<HapticChain>,
    movement_pulse:  Option<MovementHaptic>,
) {
    // The emission still goes straight to the device; that is the last step of
    // issue #57, and it is the one that needs `PadState::emit` taken apart.
    let Some(virt_dev) = outputs.devices.as_ref() else { return };

    let mut progress    = HapticProgress::default();
    let mut first_frame = true;

    while let Some(frame) = rx.recv().await {
        pad.emit(virt_dev, frame.x, frame.y, frame.touching, Some(frame.click)).await;
        if first_frame {
            first_frame = false;
            println!(
                "deckery: first trackpad event written to virtual device. +{}ms since startup",
                crate::startup_ms()
            );
        }

        let (next, actions) = plan_haptics(
            progress, &frame, haptic_pad,
            &press_chain, release_chain.as_ref(), movement_pulse,
        );
        progress = next;
        outputs.dispatch(&actions).await;
    }
}

// The combined two-finger gesture handler (`run_combined`) has moved to its
// own module, `gesture_pad.rs` — see that file for why: unlike a single pad,
// the gesture device isn't a distinct physical sensor (no `mode`/
// `click_pressure` of its own), and a click during a two-finger gesture has
// no established touchpad semantics, so it needed its own config shape
// rather than borrowing one from this handler.

#[cfg(test)]
#[path = "mt_trackpad_tests.rs"]
mod tests;
