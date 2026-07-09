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
    #[serde(default)]
    pub interval_us: u16,
    #[serde(default = "default_count")]
    pub count: u16,
    #[serde(default)]
    pub gain_db: i8,
}

fn default_duration_us() -> u16 { 2000 }
fn default_count() -> u16 { 1 }

impl Default for HapticPulse {
    fn default() -> Self {
        // A short, quiet "tick" — conservative default, not user-tuned yet.
        Self { duration_us: default_duration_us(), interval_us: 0, count: default_count(), gain_db: 0 }
    }
}

/// Haptic policy for this handler: which events fire a pulse, and with what
/// parameters. `on_movement` is parsed but not yet wired up to anything —
/// placeholder for the mode-specific haptic policy work tracked in issue #18.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct HapticConfig {
    pub on_click: Option<HapticPulse>,
    pub on_movement: Option<HapticPulse>,
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

    /// The haptic pulse to fire on a click edge — user-configured `on_click`,
    /// or the conservative built-in default.
    pub fn click_pulse(&self) -> HapticPulse {
        self.haptic.on_click.unwrap_or_default()
    }
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
        let _ = tx
            .send(HapticCommand {
                pad,
                duration_us: pulse.duration_us,
                interval_us: pulse.interval_us,
                count: pulse.count,
                gain_db: pulse.gain_db,
            })
            .await;
    }
}

/// Runs one individual (non-gesture) MT trackpad handler until `rx` closes.
/// Consumes `SinglePadFrame`s already routed to this pad by
/// `trackpad_router::run` and emits them to `pad`'s virtual MT device,
/// firing a haptic click-tick on the rising edge of `click`.
pub async fn run_single(
    mut rx: mpsc::Receiver<SinglePadFrame>,
    virt_dev: &Arc<Mutex<VirtualDevices>>,
    pad: &PadState,
    haptic_tx: Option<mpsc::Sender<HapticCommand>>,
    haptic_pad: HapticPad,
    click_pulse: HapticPulse,
) {
    let mut prev_click = false;
    while let Some(frame) = rx.recv().await {
        let click_edge = frame.click && !prev_click;
        prev_click = frame.click;
        pad.emit(virt_dev, frame.x, frame.y, frame.touching, Some(frame.click)).await;
        if click_edge {
            pulse(&haptic_tx, haptic_pad, click_pulse).await;
        }
    }
}

// The combined two-finger gesture handler (`run_combined`) has moved to its
// own module, `gesture_pad.rs` — see that file for why: unlike a single pad,
// the gesture device isn't a distinct physical sensor (no `mode`/
// `click_pressure` of its own), and a click during a two-finger gesture has
// no established touchpad semantics, so it needed its own config shape
// rather than borrowing one from this handler.
