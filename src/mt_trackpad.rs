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
use deckery_controller::{HapticChain, HapticPad, HapticPulse, HapticRequest};
use crate::trackpad::PadState;
use crate::trackpad_router::SinglePadFrame;
use crate::virtual_devices::VirtualDevices;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

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

/// Sends a single haptic pulse on `pad`, best-effort.
///
/// Constructs a single-step `HapticRequest` and sends it to the controller's
/// haptic player — chain evaluation and fd I/O happen there, not here.
/// Send errors are dropped silently: a missing haptic motor must never affect
/// input handling.
///
/// `pub(crate)`: shared with `gesture_pad.rs` to avoid duplicating the
/// three-line send.
pub(crate) async fn pulse(
    haptic_tx: &Option<mpsc::Sender<HapticRequest>>,
    pad: HapticPad,
    pulse: HapticPulse,
) {
    if let Some(tx) = haptic_tx {
        tx.send(HapticRequest { pad, chain: HapticChain::single(pulse) }).await.ok();
    }
}

/// Sends a `HapticChain` on `pad`, best-effort.
///
/// The chain is cloned and sent to the controller's haptic player task, which
/// evaluates it (including inter-step sleeps) without blocking this task.
pub(crate) async fn fire_chain(
    haptic_tx: &Option<mpsc::Sender<HapticRequest>>,
    pad: HapticPad,
    chain: &HapticChain,
) {
    if let Some(tx) = haptic_tx {
        tx.send(HapticRequest { pad, chain: chain.clone() }).await.ok();
    }
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
    virt_dev:        &Arc<Mutex<VirtualDevices>>,
    pad:             &PadState,
    haptic_tx:       Option<mpsc::Sender<HapticRequest>>,
    haptic_pad:      HapticPad,
    press_chain:     HapticChain,
    release_chain:   Option<HapticChain>,
    movement_pulse:  Option<MovementHaptic>,
) {
    let mut prev_click  = false;
    let mut first_frame = true;
    let mut prev_pos:   Option<(i32, i32)> = None;
    let mut move_accum: f64 = 0.0;

    while let Some(frame) = rx.recv().await {
        let press_edge   = frame.click && !prev_click;
        let release_edge = !frame.click && prev_click;
        prev_click = frame.click;

        pad.emit(virt_dev, frame.x, frame.y, frame.touching, Some(frame.click)).await;
        if first_frame {
            first_frame = false;
            println!(
                "deckery: first trackpad event written to virtual device. +{}ms since startup",
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
                prev_pos  = None;
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
