//! Multi-touch trackpad emulation handler.
//!
//! `pad_hidraw.rs` is a pure raw-frame *producer*: it knows nothing about
//! trackpad modes, gesture sessions, or haptics — it just turns hidraw
//! reports into `PadFrame`s and accepts `HapticCommand`s to write back.
//! `trackpad_router.rs` owns Core routing: gesture-session entry/exit,
//! click-edge-independent state.json export, and deciding which per-channel
//! handler input (`SinglePadFrame`/`CombinedPadFrame`) each frame goes to.
//!
//! This module is the *consumer/interpreter* downstream of the router: it
//! turns a `SinglePadFrame`/`CombinedPadFrame` stream into virtual MT device
//! events and click-edge haptic feedback. That decision is deliberately kept
//! out of `trackpad_router.rs` because it depends entirely on user
//! configuration and is expected to grow siblings: a "trackball" mode
//! (`TRACKPAD_RELATIVE_MOUSE`) or a future multi-zone/radial mode would each
//! be their own handler module consuming the same per-channel frame stream
//! and `HapticCommand` sink, without touching this file or
//! `trackpad_router.rs`.
use crate::pad_hidraw::{HapticCommand, HapticPad};
use crate::trackpad::PadState;
use crate::trackpad_router::SinglePadFrame;
use crate::virtual_devices::VirtualDevices;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

/// Parameters of a single haptic "click tick" — how a physical trackpad
/// click should feel. Kept here (not in `pad_hidraw.rs`) because "what a
/// click feels like" is an emulation policy, not a property of the raw
/// channel; a future handler (e.g. trackball) can pick entirely different
/// values, or trigger pulses on cursor deceleration instead of clicks.
///
/// Deserialised straight out of this handler's `handler_config` TOML
/// sub-table (see `MtTrackpadConfig` below) — Core never sees this shape.
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
fn default_count() -> u16 { 3 }
fn default_movement_pixel_interval() -> u32 { 3000 }

impl Default for HapticPulse {
    fn default() -> Self {
        // A three-pulse burst (8ms on / 8ms off) — tuned on real Steam Deck
        // hardware (2026-07-10) against the Lizard Mode click buzz as a
        // reference feel. `gain_db` is deliberately left at 0 (not raised
        // to compensate): on-hardware A/B testing (0 / +6 / i8::MAX) showed
        // gain_db has no perceptible effect on this hardware at all — see
        // issue #20 — so it isn't a real lever, unlike duration_us/
        // interval_us/count, which are.
        Self { duration_us: default_duration_us(), interval_us: default_interval_us(), count: default_count(), gain_db: 0 }
    }
}

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
    pub on_press: Option<HapticChain>,
    pub on_release: Option<HapticChain>,
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

    /// The haptic chain to fire on the press edge (finger comes down on an
    /// already-touching pad) — user-configured `on_press`, or the built-in
    /// default single-pulse chain.
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

/// One step in a `HapticChain`: a single burst followed by an optional pause
/// before the next step. The last step in a chain typically has `pause_ms =
/// None` (no pause needed after the final pulse).
#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct HapticChainStep {
    #[serde(flatten)]
    pub pulse: HapticPulse,
    /// Milliseconds to wait after this pulse before firing the next one.
    /// `None` (or omitted in TOML) on the last step.
    pub pause_ms: Option<u64>,
}

/// A sequence of haptic pulses, each with an optional inter-pulse pause.
///
/// Deserialises from two TOML forms — existing single-pulse configs need no
/// migration:
///
/// ```toml
/// # Single pulse (backward-compatible):
/// haptic_on = { duration_us = 8000, count = 1 }
///
/// # Chain with pauses:
/// haptic_on = [
///     { duration_us = 8000, count = 1, pause_ms = 150 },
///     { duration_us = 8000, count = 1 },
/// ]
/// ```
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
pub enum HapticChain {
    /// Single burst — the common case and the backward-compatible form.
    Single(HapticPulse),
    /// Ordered sequence of pulses; each step may pause before the next.
    Chain(Vec<HapticChainStep>),
}

impl Default for HapticChain {
    fn default() -> Self { Self::Single(HapticPulse::default()) }
}

impl HapticChain {
    /// Wraps a single pulse as a chain (convenience constructor).
    pub fn single(pulse: HapticPulse) -> Self { Self::Single(pulse) }
}

/// Fires a haptic pulse on `pad` per `pulse`, best-effort: a missing/failed
/// rumble motor must never affect input handling, so send errors are
/// dropped silently (the channel itself already logs failures at the
/// `pad_hidraw` writer level).
///
/// `pub(crate)` rather than private: `gesture_pad.rs` reuses this instead of
/// duplicating the same three-line send — the "how to fire a pulse" plumbing
/// is shared infrastructure, only the "when/with what parameters" policy is
/// per-handler.
pub(crate) async fn pulse(haptic_tx: &Option<mpsc::Sender<HapticCommand>>, pad: HapticPad, pulse: HapticPulse) {
    if let Some(tx) = haptic_tx {
        tx.send(HapticCommand {
            pad,
            duration_us: pulse.duration_us,
            interval_us: pulse.interval_us,
            count: pulse.count,
            gain_db: pulse.gain_db,
        })
        .await
        .ok();
    }
}

/// Fires a `HapticChain` on `pad`: single pulse or ordered sequence with
/// per-step pauses. This is the single execution point for all chain logic —
/// callers never inspect the chain shape themselves.
pub(crate) async fn fire_chain(haptic_tx: &Option<mpsc::Sender<HapticCommand>>, pad: HapticPad, chain: &HapticChain) {
    match chain {
        HapticChain::Single(p) => pulse(haptic_tx, pad, *p).await,
        HapticChain::Chain(steps) => {
            for step in steps {
                pulse(haptic_tx, pad, step.pulse).await;
                if let Some(ms) = step.pause_ms {
                    if ms > 0 {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                    }
                }
            }
        }
    }
}

/// Runs one individual (non-gesture) MT trackpad handler until `rx` closes.
/// Consumes `SinglePadFrame`s already routed to this pad by
/// `trackpad_router::run` and emits them to `pad`'s virtual MT device,
/// firing a haptic tick on the press edge (rising) and, if configured,
/// another on the release edge (falling) of `click` — press and release are
/// deliberately two independent pulses rather than one "on_click", since a
/// real click mechanism has two distinct, separately-feelable edges.
pub async fn run_single(
    mut rx: mpsc::Receiver<SinglePadFrame>,
    virt_dev: &Arc<Mutex<VirtualDevices>>,
    pad: &PadState,
    haptic_tx: Option<mpsc::Sender<HapticCommand>>,
    haptic_pad: HapticPad,
    press_chain: HapticChain,
    release_chain: Option<HapticChain>,
    movement_pulse: Option<MovementHaptic>,
) {
    let mut prev_click = false;
    let mut first_frame = true;
    let mut prev_pos: Option<(i32, i32)> = None;
    let mut move_accum: f64 = 0.0;
    while let Some(frame) = rx.recv().await {
        let press_edge = frame.click && !prev_click;
        let release_edge = !frame.click && prev_click;
        prev_click = frame.click;

        pad.emit(virt_dev, frame.x, frame.y, frame.touching, Some(frame.click)).await;
        if first_frame {
            first_frame = false;
            println!(
                "makima: first trackpad event written to virtual device. +{}ms since startup",
                crate::startup_ms()
            );
        }
        if press_edge {
            fire_chain(&haptic_tx, haptic_pad, &press_chain).await;
        } else if release_edge {
            if let Some(ref c) = release_chain {
                fire_chain(&haptic_tx, haptic_pad, c).await;
            }
        }

        if let Some(m) = movement_pulse {
            if frame.touching {
                if let Some((px, py)) = prev_pos {
                    let dx = (frame.x - px) as f64;
                    let dy = (frame.y - py) as f64;
                    move_accum += (dx * dx + dy * dy).sqrt();
                    if move_accum >= m.pixel_interval as f64 {
                        pulse(&haptic_tx, haptic_pad, m.pulse).await;
                        move_accum = 0.0;
                    }
                }
                prev_pos = Some((frame.x, frame.y));
            } else {
                prev_pos = None;
                move_accum = 0.0;
            }
        }
    }
}

// The combined two-finger gesture handler (`run_combined`) has moved to its
// own module, `gesture_pad.rs` — see that file for why: unlike a single pad,
// the gesture device isn't a distinct physical sensor (no `mode`/
// `click_pressure` of its own), and a click during a two-finger gesture has
// no established touchpad semantics, so it needed its own config shape
// rather than borrowing one from this handler.
