//! Trackball trackpad mode: relative pointer motion with momentum.
//!
//! A pad in this mode behaves like the ball of a trackball, the way Steam
//! Input's "trackball" option does. While the finger is down, its movement
//! moves the pointer. On lift the ball keeps rolling with the release
//! velocity and slows down through friction. Touching the pad again grabs the
//! ball and stops it, and moving from there carries on without a jump. As it
//! rolls, the pad ticks — faster at speed, slower as it decays.
//!
//! ## Shape
//!
//! A module of its own rather than a movement variant of `mt_trackpad`: the
//! output is relative motion on the shared virtual pointer
//! (`OutputDevice::Axis`, the same device `RSTICK = "cursor"` writes to), not
//! absolute MT frames on a per-pad device, so the two share nothing but the
//! click haptic vocabulary. The physical click becomes `BTN_LEFT` on
//! `OutputDevice::Keys`, which is where every other mouse-button binding goes.
//!
//! Like the rest of the trackpad path since issue #57, deciding and doing are
//! split. [`step`] is pure: it takes the state, one [`Input`] and the time that
//! has passed, and returns the next state and the actions. [`run_single`]
//! measures time, calls it, and hands the actions to `Outputs::dispatch`.
//!
//! ## Timing
//!
//! Momentum needs ticks when no hidraw frame arrives — frames are deduped
//! upstream, so a pad nobody touches is silent. While the ball rolls,
//! `run_single` adds a fixed-rate tick ([`ROLL_TICK`]); when it is not rolling
//! there is no timer at all and the handler just waits on its channel. The
//! physics integrate over the elapsed time they are given, never over a
//! tick count, so the tick rate only decides how smooth the roll looks — and
//! a test can drive the whole thing with made-up durations.
//!
//! ## Physics
//!
//! Velocity is kept in raw pad units per second, in screen orientation
//! (y grows downward). Raw units rather than pixels so that "how fast was the
//! finger" — which decides whether a lift flings at all — does not change
//! with `sensitivity`.
//!
//!   - **Release velocity** is the average over the last [`FLING_WINDOW`] before
//!     the lift, measured with real timestamps. A finger that stopped before it
//!     lifted has no motion in that window and does not fling.
//!   - **Decay** is exponential, `v(t) = v0 · e^(−friction · t)`, integrated in
//!     closed form so a long gap between ticks moves the ball exactly as far
//!     as many short ones would. The ball stops once its speed falls below
//!     `stop_speed`. A fling travels `v0 / friction` raw units in total.
//!   - **Sub-pixel remainders** carry over between steps, so slow finger
//!     movement and the slow end of a roll still move the pointer.
//!
//! ## Haptics
//!
//!   - `on_press` / `on_release`: click edges, the same chains as `mt-trackpad`.
//!   - `on_roll`: one pulse per `pixel_interval` raw units the ball rolls, so
//!     the tick rate follows the speed. At most one per step, which caps it at
//!     the roll tick rate. On by default; `on_roll = false` silences it.
//!   - `on_movement`: the same distance gate while the finger drags. Off by
//!     default, as in `mt-trackpad`.
//!
//! ## Being interrupted
//!
//! When a two-finger gesture session starts, or Gaming Mode takes over, the
//! router takes the pad away and sends a lift marked
//! [`SinglePadFrame::interrupted`]. That is not a lift of the finger, so it
//! must not fling: the ball stops where it is and a held button is released.
//!
//! ## Config
//!
//! ```toml
//! [trackpad.right]
//! mode = "trackball"
//! sensitivity     = 0.02    # Pointer pixels per raw pad unit (the pad is ~65 000 units wide)
//! friction        = 4.0     # Decay rate per second; higher stops sooner, 0 rolls until touched
//! min_fling_speed = 40000   # Raw units/s the finger must be moving at lift to set the ball rolling
//! stop_speed      = 1500    # Raw units/s below which a rolling ball stops
//!
//! [trackpad.right.haptic]
//! on_press   = { duration_us = 8000, interval_us = 8000, count = 3, gain_db = 0 }
//! on_release = { duration_us = 8000, interval_us = 8000, count = 3, gain_db = 0 }
//! # One tick per `pixel_interval` raw units rolled; `on_roll = false` to silence.
//! on_roll    = { pixel_interval = 3000, duration_us = 4000, interval_us = 4000, count = 1, gain_db = 0 }
//! # Ticks while the finger drags; off unless set.
//! # on_movement = { pixel_interval = 3000, duration_us = 4000, interval_us = 4000, count = 1, gain_db = 0 }
//! ```
//!
//! Every key is optional and parsed on its own: a bad value logs a warning and
//! falls back to that key's default, leaving the others as written. So does
//! an unknown key, which is far more likely a typo than anything else.

use crate::mt_trackpad::{chain, pulse, MovementHaptic};
use crate::output::action::{Ev, OutputAction};
use crate::output::{OutputDevice, Outputs};
use crate::trackpad_router::SinglePadFrame;
use deckery_controller::{HapticChain, HapticPad, HapticPulse};
use evdev::{EventType, Key, RelativeAxisType};
use std::collections::VecDeque;
use std::time::Duration;
use tokio::sync::mpsc;

/// How often a rolling ball is advanced. 8 ms is 125 Hz — above the refresh
/// rate of the Deck's own panel and most desktop monitors, so the roll looks
/// continuous, and still idle the moment the ball stops.
pub const ROLL_TICK: Duration = Duration::from_millis(8);

/// How far back the release velocity looks. Long enough to average over a
/// dozen hidraw frames and smooth out the jitter of a finger leaving the
/// pad; short enough that a flick is measured at its end, not its start.
pub const FLING_WINDOW: f64 = 0.050;

/// The longest step the physics will take at once. A suspend in the middle of
/// a roll, or a stalled runtime, would otherwise land the whole remaining
/// roll as one jump. Several roll ticks long, so it never shortens a normal
/// step.
pub const MAX_STEP: f64 = 0.050;

// ── Config ────────────────────────────────────────────────────────────────────

const DEFAULT_SENSITIVITY: f64 = 0.02;
const DEFAULT_FRICTION: f64 = 4.0;
const DEFAULT_MIN_FLING_SPEED: f64 = 40_000.0;
const DEFAULT_STOP_SPEED: f64 = 1_500.0;

/// The roll tick: one short single pulse per 3000 raw units rolled — the
/// same shape as the movement tick suggested in the shipped trackpad config.
fn default_roll_haptic() -> MovementHaptic {
    MovementHaptic {
        pixel_interval: 3000,
        pulse: HapticPulse { duration_us: 4000, interval_us: 4000, count: 1, gain_db: 0 },
    }
}

/// The finger-drag tick, when `on_movement = true` asks for it without a shape.
fn default_movement_haptic() -> MovementHaptic {
    default_roll_haptic()
}

/// This handler's own config, parsed from the raw `[trackpad.<side>]` table
/// Core hands it unparsed. Core owns `mode`, `click_pressure` and `kde`; every
/// other key belongs here.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackballConfig {
    /// Pointer pixels per raw pad unit, for the finger and for the roll alike.
    pub sensitivity: f64,
    /// Exponential decay rate of a rolling ball, per second. 0 never slows.
    pub friction: f64,
    /// Raw units/s the finger must be moving at lift for the ball to roll.
    pub min_fling_speed: f64,
    /// Raw units/s below which a rolling ball stops. Always above 0: an
    /// exponential decay never reaches 0 on its own.
    pub stop_speed: f64,
    pub on_press: Option<HapticChain>,
    pub on_release: Option<HapticChain>,
    pub on_movement: Option<MovementHaptic>,
    pub on_roll: Option<MovementHaptic>,
}

impl Default for TrackballConfig {
    fn default() -> Self {
        Self {
            sensitivity: DEFAULT_SENSITIVITY,
            friction: DEFAULT_FRICTION,
            min_fling_speed: DEFAULT_MIN_FLING_SPEED,
            stop_speed: DEFAULT_STOP_SPEED,
            // The same defaults as `MtTrackpadConfig::press_chain` /
            // `release_chain`, so switching a pad between modes keeps the feel
            // of its click.
            on_press: Some(HapticChain::default()),
            on_release: Some(HapticChain::default()),
            on_movement: None,
            on_roll: Some(default_roll_haptic()),
        }
    }
}

/// Keys of `[trackpad.<side>]` that Core reads, or that hold sub-tables.
const CORE_KEYS: &[&str] = &["mode", "click_pressure", "kde", "haptic"];
const PHYSICS_KEYS: &[&str] = &["sensitivity", "friction", "min_fling_speed", "stop_speed"];
const HAPTIC_KEYS: &[&str] = &["on_press", "on_release", "on_movement", "on_roll"];

impl TrackballConfig {
    /// Parses `value` (a `[trackpad.left]`/`[trackpad.right]` table) and logs
    /// every problem found. Never fails: a value it cannot use is replaced by
    /// its default, so a typo can never cost the pad.
    pub fn from_toml_value(value: &toml::Value) -> Self {
        let (config, warnings) = Self::parse(value);
        for w in warnings {
            eprintln!("[makima] Warning: trackball config: {}", w);
        }
        config
    }

    /// The parse itself, returning the warnings instead of printing them so
    /// the fallbacks can be tested.
    pub fn parse(value: &toml::Value) -> (Self, Vec<String>) {
        let mut c = Self::default();
        let mut warnings = Vec::new();

        let Some(table) = value.as_table() else {
            warnings.push(format!("expected a table, got {}; using defaults", value.type_str()));
            return (c, warnings);
        };

        for key in table.keys() {
            if !CORE_KEYS.contains(&key.as_str()) && !PHYSICS_KEYS.contains(&key.as_str()) {
                warnings.push(format!("unknown key {:?} ignored", key));
            }
        }

        let number = |key: &str, valid: fn(f64) -> bool, rule: &str, default: f64,
                      warnings: &mut Vec<String>| -> f64 {
            match table.get(key) {
                None => default,
                Some(v) => {
                    let n = v.as_float().or_else(|| v.as_integer().map(|i| i as f64));
                    match n {
                        Some(n) if n.is_finite() && valid(n) => n,
                        _ => {
                            warnings.push(format!(
                                "{} = {} is not valid (must be {}); using {}",
                                key, v, rule, default
                            ));
                            default
                        }
                    }
                }
            }
        };

        c.sensitivity =
            number("sensitivity", |n| n > 0.0, "a number above 0", DEFAULT_SENSITIVITY, &mut warnings);
        c.friction =
            number("friction", |n| n >= 0.0, "a number of at least 0", DEFAULT_FRICTION, &mut warnings);
        c.min_fling_speed = number(
            "min_fling_speed", |n| n >= 0.0, "a number of at least 0",
            DEFAULT_MIN_FLING_SPEED, &mut warnings,
        );
        c.stop_speed =
            number("stop_speed", |n| n > 0.0, "a number above 0", DEFAULT_STOP_SPEED, &mut warnings);

        match table.get("haptic") {
            None => {}
            Some(toml::Value::Table(h)) => {
                for key in h.keys() {
                    if !HAPTIC_KEYS.contains(&key.as_str()) {
                        warnings.push(format!("unknown key haptic.{:?} ignored", key));
                    }
                }
                c.on_press = parse_chain(h.get("on_press"), "on_press", &mut warnings);
                c.on_release = parse_chain(h.get("on_release"), "on_release", &mut warnings);
                c.on_movement = parse_distance_haptic(
                    h.get("on_movement"), "on_movement", None, default_movement_haptic(), &mut warnings,
                );
                c.on_roll = parse_distance_haptic(
                    h.get("on_roll"), "on_roll", Some(default_roll_haptic()), default_roll_haptic(),
                    &mut warnings,
                );
            }
            Some(other) => warnings.push(format!(
                "haptic must be a table, got {}; using the default haptics", other.type_str()
            )),
        }

        (c, warnings)
    }
}

/// A click chain: absent or `true` is the default chain, `false` silences it,
/// anything else is a `HapticChain` or falls back to the default.
fn parse_chain(v: Option<&toml::Value>, key: &str, warnings: &mut Vec<String>) -> Option<HapticChain> {
    match v {
        None | Some(toml::Value::Boolean(true)) => Some(HapticChain::default()),
        Some(toml::Value::Boolean(false)) => None,
        Some(v) => match v.clone().try_into::<HapticChain>() {
            Ok(c) => Some(c),
            Err(e) => {
                warnings.push(format!("haptic.{} is not a valid pulse or chain ({}); using the default", key, e.to_string().trim()));
                Some(HapticChain::default())
            }
        },
    }
}

/// A distance-gated tick. `absent` is what a missing or unusable key means,
/// `shape` is what `true` and a partial table start from — fields the table leaves out
/// keep `shape`'s values rather than the generic 3-pulse burst.
fn parse_distance_haptic(
    v: Option<&toml::Value>,
    key: &str,
    absent: Option<MovementHaptic>,
    shape: MovementHaptic,
    warnings: &mut Vec<String>,
) -> Option<MovementHaptic> {
    // A value that cannot be used counts as not given.
    let fallback = || absent;
    match v {
        None => absent,
        Some(toml::Value::Boolean(true)) => Some(shape),
        Some(toml::Value::Boolean(false)) => None,
        Some(toml::Value::Table(t)) => {
            let mut merged = toml::value::Table::new();
            merged.insert("pixel_interval".into(), (shape.pixel_interval as i64).into());
            merged.insert("duration_us".into(), (shape.pulse.duration_us as i64).into());
            merged.insert("interval_us".into(), (shape.pulse.interval_us as i64).into());
            merged.insert("count".into(), (shape.pulse.count as i64).into());
            merged.insert("gain_db".into(), (shape.pulse.gain_db as i64).into());
            for (k, val) in t {
                merged.insert(k.clone(), val.clone());
            }
            match toml::Value::Table(merged).try_into::<MovementHaptic>() {
                Ok(m) if m.pixel_interval > 0 => Some(m),
                Ok(_) => {
                    warnings.push(format!("haptic.{}.pixel_interval must be above 0; using the default", key));
                    fallback()
                }
                Err(e) => {
                    warnings.push(format!("haptic.{} is not a valid tick ({}); using the default", key, e.to_string().trim()));
                    fallback()
                }
            }
        }
        Some(other) => {
            warnings.push(format!(
                "haptic.{} must be a table or true/false, got {}; using the default", key, other.type_str()
            ));
            fallback()
        }
    }
}

// ── State ─────────────────────────────────────────────────────────────────────

/// One thing the handler reacts to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Input {
    /// A frame from the router.
    Frame(SinglePadFrame),
    /// The roll timer fired. Only ever produced while the ball rolls.
    Tick,
}

/// A position the finger was at, and when — seconds since the touch began.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sample {
    t: f64,
    x: i32,
    y: i32,
}

/// A finger on the pad.
#[derive(Debug, Clone, PartialEq)]
pub struct Touch {
    /// Seconds since the finger came down.
    clock: f64,
    /// The recent path, oldest first; only as much as the fling window needs.
    samples: VecDeque<Sample>,
    /// Raw distance dragged since the last movement tick.
    move_accum: f64,
}

impl Touch {
    fn last(&self) -> Sample {
        *self.samples.back().expect("a touch always has its first sample")
    }
}

/// A ball rolling on its own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Roll {
    /// Velocity in raw units/s, screen orientation (y down).
    pub vx: f64,
    pub vy: f64,
    /// Raw distance rolled since the last roll tick.
    accum: f64,
}

impl Roll {
    pub fn speed(&self) -> f64 {
        self.vx.hypot(self.vy)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Phase {
    /// No finger, no motion.
    #[default]
    Idle,
    Touching(Touch),
    Rolling(Roll),
}

/// Everything the handler remembers between inputs.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackballState {
    pub phase: Phase,
    /// Whether the button is down as far as the desktop knows.
    pub click: bool,
    /// Pointer motion not yet sent because it is less than a pixel.
    remainder: (f64, f64),
}

impl TrackballState {
    pub fn is_rolling(&self) -> bool {
        matches!(self.phase, Phase::Rolling(_))
    }
}

// ── Planner ───────────────────────────────────────────────────────────────────

fn rel_frame(dx: i32, dy: i32) -> Option<OutputAction> {
    let mut events = Vec::new();
    if dx != 0 {
        events.push(Ev { ev_type: EventType::RELATIVE.0, code: RelativeAxisType::REL_X.0, value: dx });
    }
    if dy != 0 {
        events.push(Ev { ev_type: EventType::RELATIVE.0, code: RelativeAxisType::REL_Y.0, value: dy });
    }
    // One frame for both axes: a diagonal is one motion, not two.
    (!events.is_empty()).then_some(OutputAction::Frame { device: OutputDevice::Axis, events })
}

fn button_frame(pressed: bool) -> OutputAction {
    OutputAction::Frame {
        device: OutputDevice::Keys,
        events: vec![Ev { ev_type: EventType::KEY.0, code: Key::BTN_LEFT.code(), value: pressed as i32 }],
    }
}

/// Turn raw motion (screen orientation) into whole pixels, keeping the rest.
fn move_pointer(
    remainder: &mut (f64, f64),
    raw_dx: f64,
    raw_dy: f64,
    sensitivity: f64,
    actions: &mut Vec<OutputAction>,
) {
    let fx = remainder.0 + raw_dx * sensitivity;
    let fy = remainder.1 + raw_dy * sensitivity;
    let (px, py) = (fx.trunc(), fy.trunc());
    *remainder = (fx - px, fy - py);
    actions.extend(rel_frame(px as i32, py as i32));
}

/// The average velocity over the last `FLING_WINDOW` before `t_lift`, in raw
/// units/s, screen orientation. Measured from the newest sample at or before
/// the window's start (or the first one, for a touch shorter than the
/// window) to the newest sample, over the time up to the lift — so a finger
/// that rested before lifting has moved nowhere in the window and reads as
/// still.
fn release_velocity(touch: &Touch, t_lift: f64) -> (f64, f64) {
    let start = t_lift - FLING_WINDOW;
    let reference = touch
        .samples
        .iter()
        .rev()
        .find(|s| s.t <= start)
        .or_else(|| touch.samples.front())
        .copied()
        .expect("a touch always has its first sample");
    let last = touch.last();
    let dt = t_lift - reference.t;
    if dt <= 0.0 {
        return (0.0, 0.0);
    }
    (
        (last.x - reference.x) as f64 / dt,
        -((last.y - reference.y) as f64) / dt,
    )
}

/// Advance a rolling ball by `dt` seconds. Returns `None` once it has stopped.
fn roll_for(
    mut roll: Roll,
    dt: f64,
    remainder: &mut (f64, f64),
    pad: HapticPad,
    cfg: &TrackballConfig,
    actions: &mut Vec<OutputAction>,
) -> Option<Roll> {
    let dt = dt.clamp(0.0, MAX_STEP);
    // ∫ v0·e^(−k t) dt from 0 to dt, which is v0·dt when k is 0.
    let k = cfg.friction;
    let (travel, decay) = if k > 0.0 {
        let decay = (-k * dt).exp();
        ((1.0 - decay) / k, decay)
    } else {
        (dt, 1.0)
    };
    let (dx, dy) = (roll.vx * travel, roll.vy * travel);
    move_pointer(remainder, dx, dy, cfg.sensitivity, actions);
    roll.vx *= decay;
    roll.vy *= decay;

    if let Some(tick) = cfg.on_roll {
        roll.accum += dx.hypot(dy);
        if roll.accum >= tick.pixel_interval as f64 {
            actions.push(pulse(pad, tick.pulse));
            roll.accum = 0.0;
        }
    }

    if roll.speed() < cfg.stop_speed {
        *remainder = (0.0, 0.0);
        None
    } else {
        Some(roll)
    }
}

/// Decide what one input means.
///
/// Pure: the state so far, the input, and how long it has been since the
/// previous input go in; the next state and everything to send come out.
/// `elapsed` is the only clock — nothing here reads the time.
pub fn step(
    state: TrackballState,
    input: Input,
    elapsed: Duration,
    cfg: &TrackballConfig,
    pad: HapticPad,
) -> (TrackballState, Vec<OutputAction>) {
    let mut s = state;
    let mut actions = Vec::new();
    let dt = elapsed.as_secs_f64();

    // A rolling ball rolls for as long as has passed, whatever arrives next.
    if let Phase::Rolling(roll) = s.phase {
        s.phase = match roll_for(roll, dt, &mut s.remainder, pad, cfg, &mut actions) {
            Some(r) => Phase::Rolling(r),
            None => Phase::Idle,
        };
    }

    let frame = match input {
        Input::Tick => return (s, actions),
        Input::Frame(f) => f,
    };

    if frame.interrupted {
        // The router took the pad away; the finger did not leave it. Stop dead
        // and let go of the button, but do not fling and do not buzz — nothing
        // was felt.
        if s.click {
            actions.push(button_frame(false));
        }
        return (TrackballState::default(), actions);
    }

    // The click, on its edges only, so a held button is not re-sent.
    if frame.click != s.click {
        s.click = frame.click;
        actions.push(button_frame(frame.click));
        if frame.click {
            if let Some(c) = &cfg.on_press {
                actions.push(chain(pad, c));
            }
        } else if let Some(c) = &cfg.on_release {
            actions.push(chain(pad, c));
        }
    }

    s.phase = match (std::mem::take(&mut s.phase), frame.touching) {
        // A finger coming down grabs the ball: whatever it was doing, it stops
        // here, and this position is where movement counts from.
        (Phase::Idle | Phase::Rolling(_), true) => {
            s.remainder = (0.0, 0.0);
            Phase::Touching(Touch {
                clock: 0.0,
                samples: VecDeque::from([Sample { t: 0.0, x: frame.x, y: frame.y }]),
                move_accum: 0.0,
            })
        }

        (Phase::Touching(mut touch), true) => {
            let prev = touch.last();
            touch.clock += dt;
            let raw_dx = (frame.x - prev.x) as f64;
            // The pad's y grows upward, the screen's downward.
            let raw_dy = -((frame.y - prev.y) as f64);
            move_pointer(&mut s.remainder, raw_dx, raw_dy, cfg.sensitivity, &mut actions);

            if let Some(m) = cfg.on_movement {
                touch.move_accum += raw_dx.hypot(raw_dy);
                if touch.move_accum >= m.pixel_interval as f64 {
                    actions.push(pulse(pad, m.pulse));
                    touch.move_accum = 0.0;
                }
            }

            touch.samples.push_back(Sample { t: touch.clock, x: frame.x, y: frame.y });
            // Keep one sample at or before the window's start, drop the rest.
            let start = touch.clock - FLING_WINDOW;
            while touch.samples.len() > 2 && touch.samples[1].t <= start {
                touch.samples.pop_front();
            }
            Phase::Touching(touch)
        }

        // The finger left. The lift frame's own position is not trusted —
        // the pad reports wherever it likes as contact breaks — only its time.
        (Phase::Touching(touch), false) => {
            let (vx, vy) = release_velocity(&touch, touch.clock + dt);
            let speed = vx.hypot(vy);
            if speed >= cfg.min_fling_speed && speed >= cfg.stop_speed {
                Phase::Rolling(Roll { vx, vy, accum: 0.0 })
            } else {
                s.remainder = (0.0, 0.0);
                Phase::Idle
            }
        }

        // No finger before, none now — a click edge on its own, or a repeat.
        (phase @ (Phase::Idle | Phase::Rolling(_)), false) => phase,
    };

    (s, actions)
}

// ── Handler ───────────────────────────────────────────────────────────────────

/// Runs one trackball handler until `rx` closes.
///
/// The only things decided here are *when* to call [`step`] and how much time
/// has passed: a frame whenever one arrives, plus a [`ROLL_TICK`] timer for as
/// long as the ball rolls. A frame always wins over a due tick, so a touch
/// grabs the ball at once.
pub async fn run_single(
    mut rx: mpsc::Receiver<SinglePadFrame>,
    outputs: &Outputs,
    haptic_pad: HapticPad,
    config: TrackballConfig,
) {
    let mut state = TrackballState::default();
    let mut last = tokio::time::Instant::now();
    let mut ticker: Option<tokio::time::Interval> = None;

    loop {
        let input = if state.is_rolling() {
            let t = ticker.get_or_insert_with(|| {
                let mut i = tokio::time::interval_at(last + ROLL_TICK, ROLL_TICK);
                i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                i
            });
            tokio::select! {
                biased;
                frame = rx.recv() => match frame {
                    Some(f) => Input::Frame(f),
                    None => break,
                },
                _ = t.tick() => Input::Tick,
            }
        } else {
            ticker = None;
            match rx.recv().await {
                Some(f) => Input::Frame(f),
                None => break,
            }
        };

        let now = tokio::time::Instant::now();
        let elapsed = now - last;
        last = now;

        let (next, actions) = step(state, input, elapsed, &config, haptic_pad);
        state = next;
        outputs.dispatch(&actions).await;
    }

    // The channel closed (session over). A button the desktop still thinks is
    // down would stay down.
    if state.click {
        outputs.dispatch(&[button_frame(false)]).await;
    }
}

#[cfg(test)]
#[path = "trackball_tests.rs"]
mod tests;
