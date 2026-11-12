//! Combined two-finger gesture handler.
//!
//! Consumes `CombinedPadFrame`s routed by `trackpad_router::run` while a
//! gesture session is active (both pads touching simultaneously) and
//! decides the frames for the combined "Deckery Gesture Pad" virtual device —
//! see `trackpad::plan_gesture_frame`. libinput derives everything it needs
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
//!
//! ## Config
//!
//! ```toml
//! [trackpad.gestures]
//! pixel_interval = 3000   # Raw units (both fingers summed) per on_gesture_move tick
//!
//! [trackpad.gestures.haptic]
//! on_gesture_start = { duration_us = 8000, count = 1 }
//! on_gesture_move  = { duration_us = 4000, interval_us = 4000, count = 1 }
//! on_gesture_end   = [{ duration_us = 4000, count = 1, pause_ms = 40 }, { duration_us = 4000, count = 1 }]
//! ```
//!
//! Each value takes the same form as a binding's `haptic =`, minus `pad`
//! (always both): a single event or a list of steps. All three are off
//! unless set; `true` switches one on with a default, `false` keeps it off.
//! The distance gate is a key of the section, never of the haptic.
use crate::config::{default_tick, parse_pad_haptic};
use crate::mt_trackpad::{chain, parse_pixel_interval};
use deckery_controller::{HapticChain, HapticPad};
use crate::output::action::OutputAction;
use crate::output::Outputs;
use crate::trackpad::plan_gesture_frame;
use crate::trackpad_router::{CombinedPadFrame, GestureEvent};
use tokio::sync::mpsc;

/// Haptic policy for the gesture pad, keyed on gesture lifecycle rather than
/// click (a click has no meaning mid-gesture — see the module doc above).
/// Unlike `mt_trackpad`'s `on_press`/`on_release`, none of these three have
/// an established "feel" to fall back to, so they're silent unless the user
/// explicitly configures one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GestureHapticConfig {
    /// Played when a gesture session begins (both pads become
    /// simultaneously touched).
    pub on_gesture_start: Option<HapticChain>,
    /// Played on ongoing movement — see `GestureMoveHaptic`.
    pub on_gesture_move: Option<GestureMoveHaptic>,
    /// Played when a gesture session ends (either pad lifts).
    pub on_gesture_end: Option<HapticChain>,
}

/// `on_gesture_move` with its gate: the raw-unit distance a finger must travel
/// (summed across both pads, see `frame_distance`) since the last tick before
/// another one fires — the section's `pixel_interval`. Live-tested naive
/// time-based throttling produced a continuous buzz during movement instead of
/// discrete ticks, since frames arrive far faster than any reasonable pulse
/// length. Distance-gating instead means faster movement ticks more often and
/// slow movement ticks rarely, matching how a real click-wheel or Steam
/// Input's own trackpad tick feedback behaves.
#[derive(Debug, Clone, PartialEq)]
pub struct GestureMoveHaptic {
    pub pixel_interval: u32,
    pub chain: HapticChain,
}

/// This handler's own config, parsed from the raw `[trackpad.gestures]` TOML
/// table that Core (`config.rs`) hands it unparsed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GesturePadConfig {
    pub haptic: GestureHapticConfig,
}

const HAPTIC_KEYS: &[&str] = &["on_gesture_start", "on_gesture_move", "on_gesture_end"];

impl GesturePadConfig {
    /// Parses `value` (the `[trackpad.gestures]` TOML table) and logs every
    /// problem. Never fails — a config typo here must never crash makima or
    /// block gesture handling.
    pub fn from_toml_value(value: &toml::Value) -> Self {
        let (config, warnings) = Self::parse(value);
        for w in warnings {
            eprintln!("[makima] Warning: gesture-pad config: {}", w);
        }
        config
    }

    /// The parse itself, returning the warnings instead of printing them.
    pub fn parse(value: &toml::Value) -> (Self, Vec<String>) {
        let mut c = Self::default();
        let mut warnings = Vec::new();
        let Some(table) = value.as_table() else {
            warnings.push(format!("expected a table, got {}; using defaults", value.type_str()));
            return (c, warnings);
        };
        let pixel_interval = parse_pixel_interval(table, &mut warnings);
        match table.get("haptic") {
            None => {}
            Some(toml::Value::Table(h)) => {
                for key in h.keys() {
                    if !HAPTIC_KEYS.contains(&key.as_str()) {
                        warnings.push(format!("unknown key haptic.{:?} ignored", key));
                    }
                }
                let gate = "the distance gate is `pixel_interval` in [trackpad.gestures]";
                let h_ = &mut c.haptic;
                h_.on_gesture_start = parse_pad_haptic(h, "on_gesture_start", None, HapticChain::default(), gate, &mut warnings);
                h_.on_gesture_move = parse_pad_haptic(h, "on_gesture_move", None, default_tick(), gate, &mut warnings)
                    .map(|chain| GestureMoveHaptic { pixel_interval, chain });
                h_.on_gesture_end = parse_pad_haptic(h, "on_gesture_end", None, HapticChain::default(), gate, &mut warnings);
            }
            Some(other) => warnings.push(format!(
                "haptic must be a table, got {}; using the default haptics", other.type_str()
            )),
        }
        (c, warnings)
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
    let mut progress = GestureProgress::default();

    /// A physical click on either half reads as one click on the combined
    /// device.
    fn frame_actions(frame: CombinedPadFrame) -> Vec<OutputAction> {
        plan_gesture_frame(
            frame.lx, frame.ly, frame.rx, frame.ry, frame.ltouch, frame.rtouch,
            Some(frame.lclick || frame.rclick),
        )
    }

    while let Some(event) = rx.recv().await {
        match event {
            GestureEvent::Start(frame) | GestureEvent::Move(frame) => {
                outputs.dispatch(&frame_actions(frame)).await;
            }
            GestureEvent::End => {
                outputs.dispatch(&plan_gesture_frame(0, 0, 0, 0, false, false, Some(false))).await;
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
            if let Some(c) = &config.haptic.on_gesture_start {
                actions.push(chain(HapticPad::Both, c));
            }
            next.prev_move_frame = Some(frame);
            next.move_accum = 0.0;
        }
        GestureEvent::Move(frame) => {
            if let Some(m) = &config.haptic.on_gesture_move {
                if let Some(prev) = progress.prev_move_frame {
                    next.move_accum += frame_distance(prev, frame);
                }
                if next.move_accum >= m.pixel_interval as f64 {
                    actions.push(chain(HapticPad::Both, &m.chain));
                    next.move_accum = 0.0;
                }
            }
            next.prev_move_frame = Some(frame);
        }
        GestureEvent::End => {
            if let Some(c) = &config.haptic.on_gesture_end {
                actions.push(chain(HapticPad::Both, c));
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
