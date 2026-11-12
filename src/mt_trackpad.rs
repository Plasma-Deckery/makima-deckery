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
//!
//! ## Config
//!
//! ```toml
//! [trackpad.left]
//! mode = "mt-trackpad"
//! pixel_interval = 3000   # Raw units the finger travels per on_movement tick
//!
//! [trackpad.left.haptic]
//! on_press    = { duration_us = 8000, interval_us = 8000, count = 3 }
//! on_release  = [{ duration_us = 8000, pause_ms = 40 }, { duration_us = 4000, count = 1 }]
//! # Off unless set; `true` is one short tick.
//! # on_movement = { duration_us = 4000, interval_us = 4000, count = 1 }
//! ```
//!
//! Each haptic value takes the same form as a binding's `haptic =`, minus
//! `pad`: a single event or a list of steps (`config::parse_haptic_value`).
//! `false` silences a key. What gates a movement tick is a key of the pad
//! section, never of the haptic. A bad value warns and falls back to that
//! key's default.
use deckery_controller::{HapticChain, HapticPad};
use crate::config::{default_tick, parse_pad_haptic};
use crate::output::action::OutputAction;
use crate::output::Outputs;
use crate::trackpad::PadState;
use crate::trackpad_router::SinglePadFrame;
use tokio::sync::mpsc;

const DEFAULT_PIXEL_INTERVAL: u32 = 3000;

/// Distance-gated movement haptic: what to play, plus the raw-unit distance
/// that gates it. Movement has no natural single trigger instant, so it needs
/// a firing policy alongside the haptic. Live-tested naive time-based
/// throttling produced a continuous buzz; distance-gating means faster
/// movement ticks more often and slow movement ticks rarely, matching Steam
/// Input's own trackpad feedback feel.
#[derive(Debug, Clone, PartialEq)]
pub struct MovementHaptic {
    pub pixel_interval: u32,
    pub chain: HapticChain,
}

/// This handler's own config, parsed from the raw `[trackpad.<side>]` table
/// Core hands it unparsed — Core only knows `mode` and `click_pressure`.
/// A physical click is two distinct edges, press and release, with a chain
/// each.
#[derive(Debug, Clone, PartialEq)]
pub struct MtTrackpadConfig {
    pub on_press:    Option<HapticChain>,
    pub on_release:  Option<HapticChain>,
    pub on_movement: Option<MovementHaptic>,
}

impl Default for MtTrackpadConfig {
    fn default() -> Self {
        Self {
            on_press:    Some(HapticChain::default()),
            on_release:  Some(HapticChain::default()),
            // Movement haptics have no established feel to fall back to,
            // unlike click edges, so silence is the right default.
            on_movement: None,
        }
    }
}

/// `pixel_interval` of a pad or gesture section: a whole number of raw units
/// of at least 0, default 3000.
pub(crate) fn parse_pixel_interval(table: &toml::value::Table, warnings: &mut Vec<String>) -> u32 {
    match table.get("pixel_interval") {
        None => DEFAULT_PIXEL_INTERVAL,
        Some(v) => match v.as_integer().and_then(|n| u32::try_from(n).ok()) {
            Some(n) => n,
            None => {
                warnings.push(format!(
                    "pixel_interval = {} is not valid (must be a whole number of at least 0); using {}",
                    v, DEFAULT_PIXEL_INTERVAL
                ));
                DEFAULT_PIXEL_INTERVAL
            }
        },
    }
}

const HAPTIC_KEYS: &[&str] = &["on_press", "on_release", "on_movement"];

impl MtTrackpadConfig {
    /// Parses `value` (a `[trackpad.left]`/`[trackpad.right]` TOML table) and
    /// logs every problem. Never fails — a config typo in a handler-owned
    /// field must never crash makima or block input handling on the pad.
    pub fn from_toml_value(value: &toml::Value) -> Self {
        let (config, warnings) = Self::parse(value);
        for w in warnings {
            eprintln!("[makima] Warning: mt-trackpad config: {}", w);
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
                let gate = "the distance gate is `pixel_interval` in the pad section";
                let click = || Some(HapticChain::default());
                c.on_press = parse_pad_haptic(h, "on_press", click(), HapticChain::default(), gate, &mut warnings);
                c.on_release = parse_pad_haptic(h, "on_release", click(), HapticChain::default(), gate, &mut warnings);
                c.on_movement = parse_pad_haptic(h, "on_movement", None, default_tick(), gate, &mut warnings)
                    .map(|chain| MovementHaptic { pixel_interval, chain });
            }
            Some(other) => warnings.push(format!(
                "haptic must be a table, got {}; using the default haptics", other.type_str()
            )),
        }
        (c, warnings)
    }

    /// The haptic chain to fire on the press edge, `None` when silenced.
    pub fn press_chain(&self) -> Option<HapticChain> {
        self.on_press.clone()
    }

    /// The haptic chain to fire on the release edge, `None` when silenced.
    pub fn release_chain(&self) -> Option<HapticChain> {
        self.on_release.clone()
    }

    /// The movement haptic and its distance gate, if switched on.
    pub fn movement_pulse(&self) -> Option<MovementHaptic> {
        self.on_movement.clone()
    }
}

// ── Haptic helpers ────────────────────────────────────────────────────────────

/// A haptic chain, as an action. `pub(crate)`: shared with `gesture_pad.rs`
/// and `trackball.rs`. The controller's haptic player evaluates it
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
    press_chain: Option<&HapticChain>,
    release_chain: Option<&HapticChain>,
    movement_pulse: Option<&MovementHaptic>,
) -> (HapticProgress, Vec<OutputAction>) {
    let mut next = progress;
    let mut actions = Vec::new();

    let press_edge   = frame.click && !progress.prev_click;
    let release_edge = !frame.click && progress.prev_click;
    next.prev_click  = frame.click;

    if press_edge {
        if let Some(c) = press_chain {
            actions.push(chain(pad, c));
        }
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
                    actions.push(chain(pad, &m.chain));
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
    press_chain:     Option<HapticChain>,
    release_chain:   Option<HapticChain>,
    movement_pulse:  Option<MovementHaptic>,
) {
    let mut progress    = HapticProgress::default();
    let mut first_frame = true;

    while let Some(frame) = rx.recv().await {
        outputs.dispatch(&crate::trackpad::plan_pad_frame(
            pad.device(), frame.x, frame.y, frame.touching, Some(frame.click),
        )).await;
        if first_frame {
            first_frame = false;
            println!(
                "deckery: first trackpad event written to virtual device. +{}ms since startup",
                crate::startup_ms()
            );
        }

        let (next, actions) = plan_haptics(
            progress, &frame, haptic_pad,
            press_chain.as_ref(), release_chain.as_ref(), movement_pulse.as_ref(),
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
