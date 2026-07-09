//! Combined two-finger gesture handler.
//!
//! Consumes `CombinedPadFrame`s routed by `trackpad_router::run` while a
//! gesture session is active (both pads touching simultaneously) and emits
//! them to the combined "Deckery Gesture Pad" virtual device — see
//! `trackpad.rs::emit_gesture_event`. libinput derives everything it needs
//! (two-finger scroll/pan, pinch-zoom) purely from the two ABS_MT_POSITION
//! slots we send; there is no separate gesture-type input we produce.
//!
//! This used to be `mt_trackpad::run_combined`, sharing that handler's
//! per-pad `HapticPulse` config. It was split out into its own module
//! because the combined device isn't a distinct physical sensor — it has no
//! `mode`/`click_pressure` of its own in `[trackpad]` — and because a click
//! during a two-finger gesture has no established touchpad semantics (unlike
//! a single-finger tap/click): there is deliberately no `on_click` haptic
//! here at all. What *is* meaningful for a gesture is its lifecycle — start,
//! ongoing movement, end — so `GestureHapticConfig` is keyed on that instead.
//! This module owns that judgment call itself via `GesturePadConfig`,
//! self-parsed from `[trackpad.gestures]` exactly like
//! `mt_trackpad::MtTrackpadConfig` parses `[trackpad.left]`/`[trackpad.right]`.
use crate::mt_trackpad::HapticPulse;
use crate::pad_hidraw::HapticCommand;
use crate::trackpad::emit_gesture_event;
use crate::trackpad_router::CombinedPadFrame;
use crate::virtual_devices::VirtualDevices;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

/// Haptic policy for the gesture pad, keyed on gesture lifecycle rather than
/// click (a click has no meaning mid-gesture — see the module doc above).
/// None of these are wired up yet: `gesture_pad::run` only sees a flat
/// stream of `CombinedPadFrame`s, not session-transition events —
/// recognising "session just started/ended" needs a signal from
/// `trackpad_router`'s gesture-session tracking, which isn't plumbed through
/// here yet. Parsed now so the config shape is settled; wiring is a
/// follow-up.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct GestureHapticConfig {
    /// Pulse fired when a gesture session begins (both pads become
    /// simultaneously touched).
    pub on_gesture_start: Option<HapticPulse>,
    /// Pulse fired repeatedly while the gesture is moving (e.g. per pinch or
    /// scroll step). Lowest priority of the three — likely to feel noisy if
    /// ever wired up naively; kept for completeness.
    pub on_gesture_move: Option<HapticPulse>,
    /// Pulse fired when a gesture session ends (either pad lifts).
    pub on_gesture_end: Option<HapticPulse>,
}

/// This handler's own config, self-deserialized from the raw
/// `[trackpad.gestures]` TOML sub-table that Core (`config.rs`) hands it
/// unparsed.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct GesturePadConfig {
    #[serde(default)]
    pub haptic: GestureHapticConfig,
}

impl GesturePadConfig {
    /// Parses `value` (the `[trackpad.gestures]` TOML table) into this
    /// handler's config. Falls back to defaults (and logs a warning) on any
    /// shape mismatch — a config typo here must never crash makima or block
    /// gesture handling.
    pub fn from_toml_value(value: &toml::Value) -> Self {
        value.clone().try_into::<GesturePadConfig>().unwrap_or_else(|e| {
            eprintln!("[makima] Warning: invalid gesture-pad handler config ({}), using defaults.", e);
            GesturePadConfig::default()
        })
    }
}

/// Runs the combined two-finger gesture MT trackpad handler until `rx`
/// closes. Forwards each frame's raw click bit (either pad physically
/// pressed) through to the virtual device's `BTN_LEFT` unconditionally —
/// same as a real multi-touch pad would — but fires no haptic feedback for
/// it: see the module doc for why click has no gesture-lifecycle meaning.
/// `config`/`haptic_tx` are accepted already for when gesture-lifecycle
/// haptics (`GestureHapticConfig`) get wired up; both are currently unused.
pub async fn run(
    mut rx: mpsc::Receiver<CombinedPadFrame>,
    virt_dev: &Arc<Mutex<VirtualDevices>>,
    _haptic_tx: Option<mpsc::Sender<HapticCommand>>,
    _config: GesturePadConfig,
) {
    while let Some(frame) = rx.recv().await {
        let click = frame.lclick || frame.rclick;
        emit_gesture_event(
            virt_dev, frame.lx, frame.ly, frame.rx, frame.ry, frame.ltouch, frame.rtouch,
            Some(click),
        ).await;
    }
}
