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
use crate::mt_trackpad::pulse;
use deckery_controller::{HapticPad, HapticPulse};
use crate::output::action::OutputAction;
use crate::output::Outputs;
use crate::trackpad::emit_gesture_event;
use crate::trackpad_router::{CombinedPadFrame, GestureEvent};
use crate::virtual_devices::VirtualDevices;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

/// Raw-unit distance a finger must travel (summed across both pads, see
/// `frame_distance`) since the last `on_gesture_move` pulse before another
/// one fires — first live-tested naive time-based throttling (self-gap from
/// the pulse's own duration) produced a continuous buzz during movement
/// instead of discrete ticks, since frames arrive far faster than any
/// reasonable pulse length. Distance-gating instead means faster movement
/// ticks more often and slow movement ticks rarely, matching how a real
/// click-wheel or Steam Input's own trackpad tick feedback behaves. Trackpad
/// coordinates are raw firmware units in roughly ±32767 (see `analog.rs`'s
/// `AXIS_SCALE`), so this default is a small fraction of the pad's full
/// travel per tick.
fn default_move_pixel_interval() -> u32 {
    3000
}

/// Haptic policy for the gesture pad, keyed on gesture lifecycle rather than
/// click (a click has no meaning mid-gesture — see the module doc above).
/// Unlike `mt_trackpad`'s `on_press`/`on_release`, none of these three have
/// an established "feel" to fall back to, so they're silent unless the user
/// explicitly configures a pulse — see `GesturePadConfig::{start,move,end}_pulse`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct GestureHapticConfig {
    /// Pulse fired when a gesture session begins (both pads become
    /// simultaneously touched).
    pub on_gesture_start: Option<HapticPulse>,
    /// Pulse policy for ongoing movement (e.g. per pinch or scroll step) —
    /// see `GestureMoveHaptic`. `None` means silent, same as the other two.
    pub on_gesture_move: Option<GestureMoveHaptic>,
    /// Pulse fired when a gesture session ends (either pad lifts).
    pub on_gesture_end: Option<HapticPulse>,
}

/// `on_gesture_move`'s config shape: the pulse itself, plus the raw-unit
/// distance threshold that gates it (see `default_move_pixel_interval`).
/// Kept as its own struct (not a bare `HapticPulse`) because movement,
/// unlike a click's press/release edges, has no natural single trigger
/// instant — it needs its own firing policy alongside the pulse shape.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct GestureMoveHaptic {
    #[serde(default = "default_move_pixel_interval")]
    pub pixel_interval: u32,
    #[serde(flatten)]
    pub pulse: HapticPulse,
}

/// This handler's own config, self-deserialized from the raw
/// `[trackpad.gestures]` TOML sub-table that Core (`config.rs`) hands it
/// unparsed.
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

    /// The pulse to fire when a gesture session starts, if the user
    /// configured one. `None` (silence) by default — see `GestureHapticConfig`.
    pub fn start_pulse(&self) -> Option<HapticPulse> {
        self.haptic.on_gesture_start
    }

    /// The movement pulse/distance-gate policy, if configured. `None` by
    /// default.
    pub fn move_pulse(&self) -> Option<GestureMoveHaptic> {
        self.haptic.on_gesture_move
    }

    /// The pulse to fire when a gesture session ends, if configured. `None`
    /// by default.
    pub fn end_pulse(&self) -> Option<HapticPulse> {
        self.haptic.on_gesture_end
    }
}

/// Raw-unit distance moved between two consecutive combined-gesture frames,
/// summed across both pads. Only pads touching in *both* frames contribute —
/// a pad touching down mid-gesture (one-pad-then-two) shouldn't register a
/// spurious jump from its previous (untouched, stale) position.
fn frame_distance(prev: CombinedPadFrame, curr: CombinedPadFrame) -> f64 {
    let mut total = 0.0;
    if prev.ltouch && curr.ltouch {
        let dx = (curr.lx - prev.lx) as f64;
        let dy = (curr.ly - prev.ly) as f64;
        total += (dx * dx + dy * dy).sqrt();
    }
    if prev.rtouch && curr.rtouch {
        let dx = (curr.rx - prev.rx) as f64;
        let dy = (curr.ry - prev.ry) as f64;
        total += (dx * dx + dy * dy).sqrt();
    }
    total
}

/// Runs the combined two-finger gesture MT trackpad handler until `rx`
/// closes. Forwards each frame's raw click bit (either pad physically
/// pressed) through to the virtual device's `BTN_LEFT` unconditionally —
/// same as a real multi-touch pad would — but fires no click-based haptic:
/// see the module doc for why click has no gesture-lifecycle meaning. Fires
/// `on_gesture_start`/`on_gesture_move`/`on_gesture_end` instead, keyed off
/// the lifecycle tag `trackpad_router` attaches to each `GestureEvent`.
pub async fn run(
    mut rx: mpsc::Receiver<GestureEvent>,
    outputs: &Outputs,
    config: GesturePadConfig,
) {
    // Emission is still direct — the last step of issue #57.
    let Some(virt_dev) = outputs.devices.as_ref() else { return };

    let mut progress = GestureProgress::default();

    async fn emit(virt_dev: &Arc<Mutex<VirtualDevices>>, frame: CombinedPadFrame) {
        let click = frame.lclick || frame.rclick;
        emit_gesture_event(
            virt_dev, frame.lx, frame.ly, frame.rx, frame.ry, frame.ltouch, frame.rtouch,
            Some(click),
        ).await;
    }

    while let Some(event) = rx.recv().await {
        match event {
            GestureEvent::Start(frame) | GestureEvent::Move(frame) => {
                emit(virt_dev, frame).await;
            }
            GestureEvent::End => {
                emit_gesture_event(virt_dev, 0, 0, 0, 0, false, false, Some(false)).await;
            }
        }

        let (next, actions) = plan_haptics(progress, event, &config);
        progress = next;
        outputs.dispatch(&actions).await;
    }
}

/// What the gesture's movement haptic has to remember between frames.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GestureProgress {
    prev_move_frame: Option<CombinedPadFrame>,
    move_accum: f64,
}

/// Decide what one gesture event should feel like.
///
/// Pure, and keyed on the lifecycle tag `trackpad_router` attaches to each
/// event rather than on the click bit: a click during a two-finger gesture has
/// no established touchpad semantics, so there is deliberately no click haptic
/// here at all. What is meaningful is start, movement and end.
pub fn plan_haptics(
    progress: GestureProgress,
    event: GestureEvent,
    config: &GesturePadConfig,
) -> (GestureProgress, Vec<OutputAction>) {
    let mut next = progress;
    let mut actions = Vec::new();

    match event {
        GestureEvent::Start(frame) => {
            if let Some(p) = config.start_pulse() {
                actions.push(pulse(HapticPad::Both, p));
            }
            next.prev_move_frame = Some(frame);
            next.move_accum = 0.0;
        }
        GestureEvent::Move(frame) => {
            if let Some(m) = config.move_pulse() {
                if let Some(prev) = progress.prev_move_frame {
                    next.move_accum += frame_distance(prev, frame);
                }
                if next.move_accum >= m.pixel_interval as f64 {
                    actions.push(pulse(HapticPad::Both, m.pulse));
                    next.move_accum = 0.0;
                }
            }
            next.prev_move_frame = Some(frame);
        }
        GestureEvent::End => {
            if let Some(p) = config.end_pulse() {
                actions.push(pulse(HapticPad::Both, p));
            }
            next.prev_move_frame = None;
            next.move_accum = 0.0;
        }
    }

    (next, actions)
}

#[cfg(test)]
#[path = "gesture_pad_tests.rs"]
mod tests;
