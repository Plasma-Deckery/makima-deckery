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
//! A real ball has one detent spacing, so dragging and rolling share one
//! raster: the pad's `grid`, `grid` × `grid` cells over the pad (a cell is
//! `65536 / grid` raw units). A tick is a crossing into a new cell; a cell is
//! only left once the position is [`GRID_HYSTERESIS`] of a cell past its edge,
//! so a finger resting on a boundary does not buzz.
//!
//!   - `on_press` / `on_release`: click edges, the same chains as `mt-trackpad`.
//!   - `on_movement`: while the finger drags, one tick per cell its position
//!     enters. A finger resting inside a cell never ticks. Off by default, as
//!     in `mt-trackpad`; `on_movement = true` switches on a short tick.
//!   - `on_roll`: while the ball rolls, one tick per cell it rolls into. The
//!     ball keeps a virtual position that carries on from where the finger
//!     lifted, in the cell the finger was in, so the raster continues across
//!     the lift with no extra tick there, and the tick rate follows the speed.
//!     At most one per step, which caps it at the roll tick rate. On by
//!     default; `on_roll = false` silences it.
//!
//! Every haptic value takes the same form as a binding's `haptic =`, minus
//! `pad` (the section decides the pad): a single event or a list of steps —
//! see `config::parse_haptic_value`.
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
//! grid            = 8       # Detents across the pad, for dragging and rolling alike
//!
//! [trackpad.right.haptic]
//! on_press   = { duration_us = 8000, interval_us = 8000, count = 3 }
//! on_release = [{ duration_us = 8000, pause_ms = 40 }, { duration_us = 4000, count = 1 }]
//! # One tick per grid cell the ball rolls into; `on_roll = false` to silence.
//! on_roll    = { duration_us = 4000, interval_us = 4000, count = 1 }
//! # One tick per grid cell the finger drags into; off unless set.
//! # on_movement = { duration_us = 4000, interval_us = 4000, count = 1 }
//! ```
//!
//! Every key is optional and parsed on its own: a bad value logs a warning and
//! falls back to that key's default, leaving the others as written. So does
//! an unknown key, which is far more likely a typo than anything else.

use crate::config::{default_tick, parse_pad_haptic};
use crate::mt_trackpad::chain;
use crate::output::action::{Ev, OutputAction};
use crate::output::{OutputDevice, Outputs};
use crate::trackpad_router::SinglePadFrame;
use deckery_controller::{HapticChain, HapticPad};
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

const DEFAULT_GRID: u32 = 8;
/// Above this a cell would be narrower than one raw unit.
const MAX_GRID: u32 = 65_536;

/// How far past a cell's edge, in cells, the finger must be to leave it.
pub const GRID_HYSTERESIS: f64 = 0.1;

/// The pad's raw range per axis: the full i16.
const PAD_MIN: f64 = i16::MIN as f64;
const PAD_SPAN: f64 = 65_536.0;

/// The cell `pos` is in along one axis, staying in `cell` until `pos` is
/// [`GRID_HYSTERESIS`] past its edge. `i32::MIN` means no cell yet. `pos` may
/// lie off the pad: a rolling ball's virtual position does.
fn grid_cell(cell: i32, pos: f64, grid: u32) -> i32 {
    let at = (pos - PAD_MIN) * grid as f64 / PAD_SPAN;
    if at < cell as f64 - GRID_HYSTERESIS || at >= (cell + 1) as f64 + GRID_HYSTERESIS {
        at.floor() as i32
    } else {
        cell
    }
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
    /// Cells across the pad per axis: the detent spacing of `on_movement`
    /// and `on_roll` alike.
    pub grid: u32,
    pub on_press: Option<HapticChain>,
    pub on_release: Option<HapticChain>,
    pub on_movement: Option<HapticChain>,
    pub on_roll: Option<HapticChain>,
}

impl Default for TrackballConfig {
    fn default() -> Self {
        Self {
            sensitivity: DEFAULT_SENSITIVITY,
            friction: DEFAULT_FRICTION,
            min_fling_speed: DEFAULT_MIN_FLING_SPEED,
            stop_speed: DEFAULT_STOP_SPEED,
            grid: DEFAULT_GRID,
            // The same defaults as `MtTrackpadConfig::press_chain` /
            // `release_chain`, so switching a pad between modes keeps the feel
            // of its click.
            on_press: Some(HapticChain::default()),
            on_release: Some(HapticChain::default()),
            on_movement: None,
            on_roll: Some(default_tick()),
        }
    }
}

/// Keys of `[trackpad.<side>]` that Core reads, or that hold sub-tables.
const CORE_KEYS: &[&str] = &["mode", "click_pressure", "kde", "haptic"];
const PHYSICS_KEYS: &[&str] = &["sensitivity", "friction", "min_fling_speed", "stop_speed", "grid"];
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
        c.grid = match table.get("grid") {
            None => DEFAULT_GRID,
            Some(v) => match v.as_integer() {
                Some(n) if (1..=MAX_GRID as i64).contains(&n) => n as u32,
                _ => {
                    warnings.push(format!(
                        "grid = {} is not valid (must be a whole number from 1 to {}); using {}",
                        v, MAX_GRID, DEFAULT_GRID
                    ));
                    DEFAULT_GRID
                }
            },
        };

        match table.get("haptic") {
            None => {}
            Some(toml::Value::Table(h)) => {
                for key in h.keys() {
                    if !HAPTIC_KEYS.contains(&key.as_str()) {
                        warnings.push(format!("unknown key haptic.{:?} ignored", key));
                    }
                }
                let gate = "the tick spacing is `grid` in the pad section";
                let click = || Some(HapticChain::default());
                c.on_press = parse_pad_haptic(h, "on_press", click(), HapticChain::default(), gate, &mut warnings);
                c.on_release = parse_pad_haptic(h, "on_release", click(), HapticChain::default(), gate, &mut warnings);
                c.on_movement = parse_pad_haptic(h, "on_movement", None, default_tick(), gate, &mut warnings);
                c.on_roll = parse_pad_haptic(h, "on_roll", Some(default_tick()), default_tick(), gate, &mut warnings);
            }
            Some(other) => warnings.push(format!(
                "haptic must be a table, got {}; using the default haptics", other.type_str()
            )),
        }

        (c, warnings)
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
    /// The grid cell the finger is in.
    cell: (i32, i32),
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
    /// Where the ball would be on the pad had the finger kept going — pad
    /// orientation, unbounded — and the grid cell that is in. Both carry on
    /// from the lift, so the roll's ticks continue the drag's raster.
    pos: (f64, f64),
    cell: (i32, i32),
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

    // The pad's y grows upward, the screen's downward.
    roll.pos = (roll.pos.0 + dx, roll.pos.1 - dy);
    let cell = (grid_cell(roll.cell.0, roll.pos.0, cfg.grid), grid_cell(roll.cell.1, roll.pos.1, cfg.grid));
    if cell != roll.cell {
        roll.cell = cell;
        if let Some(c) = &cfg.on_roll {
            actions.push(chain(pad, c));
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
                cell: (
                    grid_cell(i32::MIN, frame.x as f64, cfg.grid),
                    grid_cell(i32::MIN, frame.y as f64, cfg.grid),
                ),
            })
        }

        (Phase::Touching(mut touch), true) => {
            let prev = touch.last();
            touch.clock += dt;
            let raw_dx = (frame.x - prev.x) as f64;
            // The pad's y grows upward, the screen's downward.
            let raw_dy = -((frame.y - prev.y) as f64);
            move_pointer(&mut s.remainder, raw_dx, raw_dy, cfg.sensitivity, &mut actions);

            let cell = (
                grid_cell(touch.cell.0, frame.x as f64, cfg.grid),
                grid_cell(touch.cell.1, frame.y as f64, cfg.grid),
            );
            if cell != touch.cell {
                touch.cell = cell;
                if let Some(c) = &cfg.on_movement {
                    actions.push(chain(pad, c));
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
                let last = touch.last();
                Phase::Rolling(Roll { vx, vy, pos: (last.x as f64, last.y as f64), cell: touch.cell })
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
