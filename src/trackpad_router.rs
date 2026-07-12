//! Core trackpad routing — owned by `EventReader`, not by any interpretation
//! handler (see `mt_trackpad.rs`'s module docs and GitHub issue #17 for the
//! full rationale). This module consumes the raw `PadFrame` stream produced
//! by `pad_hidraw.rs` and is responsible for:
//!
//!   - always updating `PadState` (position/touch/pressed) for state.json
//!     export, regardless of which (if any) handler is attached to a pad —
//!     state.json must keep reporting raw touch/position/click even with
//!     `mode = "disabled"`.
//!   - deciding combined two-finger gesture-session entry/exit, independent
//!     of pad order (see `decide_gesture_transition`).
//!   - routing each frame to the correct per-channel handler input
//!     (`left_tx`/`right_tx`/`combined_tx`), skipping channels that have no
//!     handler attached (`None`) so a disabled channel never blocks the loop.
//!   - signaling `state.json` writes via `StateWrite`, rate-limited for
//!     analog movement but immediate for digital transitions.
//!
//! ## Position discontinuity at gesture boundaries
//!
//! The combined gesture device is virtual — there is no physical sensor with
//! continuous tracking behind it. When a gesture session starts or ends the
//! MT slot positions jump: the gesture device gets assigned slots from scratch
//! at session start, and the survivor pad re-enters its own single-pad channel
//! at a potentially different logical position than when it left. libinput sees
//! this as a sudden large delta and may produce a spurious scroll or pointer
//! jump. This is an inherent limitation of routing across virtual devices and
//! cannot be fully eliminated without libinput-side continuity support. The
//! debounce in `run` mitigates brief session restarts that would otherwise
//! multiply the effect.
use crate::pad_hidraw::PadFrame;
use crate::trackpad::PadState;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

/// Result of comparing two consecutive `PadFrame`s: which pad(s) actually
/// changed, and whether either pad's touch state flipped. Pure and I/O-free
/// on purpose so it can be unit-tested without an async runtime or a real
/// virtual device — see the `tests` module below. This is a router concern,
/// not a raw-production one: the meaning of "changed" here is entirely about
/// what `run` below does with it (which channel(s) to route to, whether a
/// state.json write must happen immediately) — `pad_hidraw.rs`'s own reader
/// loop dedupes independently via a plain frame equality check and never
/// calls this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PadDelta {
    pub l_changed: bool,
    pub r_changed: bool,
    pub touch_transition: bool,
}

pub fn diff_frames(prev: PadFrame, frame: PadFrame) -> PadDelta {
    PadDelta {
        l_changed: (frame.lx, frame.ly, frame.ltouch, frame.lclick)
            != (prev.lx, prev.ly, prev.ltouch, prev.lclick),
        r_changed: (frame.rx, frame.ry, frame.rtouch, frame.rclick)
            != (prev.rx, prev.ry, prev.rtouch, prev.rclick),
        touch_transition: frame.ltouch != prev.ltouch || frame.rtouch != prev.rtouch,
    }
}

/// One pad's frame as delivered to an individual (non-gesture) handler —
/// deliberately smaller than `PadFrame` (only this pad's fields) so a
/// handler like `mt_trackpad::run_single` can't accidentally read the other
/// pad's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SinglePadFrame {
    pub x: i32,
    pub y: i32,
    pub touching: bool,
    pub click: bool,
}

/// Both pads' data as delivered to the combined-gesture handler while a
/// gesture session is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CombinedPadFrame {
    pub lx: i32,
    pub ly: i32,
    pub ltouch: bool,
    pub lclick: bool,
    pub rx: i32,
    pub ry: i32,
    pub rtouch: bool,
    pub rclick: bool,
}

/// One event delivered to the combined-gesture handler's channel, tagged
/// with where it falls in the gesture's lifecycle — start, ongoing
/// movement, or end. `gesture_pad::run` needs this tag (not just the raw
/// frame) to fire lifecycle haptics (`on_gesture_start`/`on_gesture_move`/
/// `on_gesture_end`) without re-deriving session-transition state itself;
/// that state already lives here, in `decide_gesture_transition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureEvent {
    /// The single frame where the session starts (both pads just became
    /// simultaneously touched).
    Start(CombinedPadFrame),
    /// Any frame while the session is active, after the starting frame.
    Move(CombinedPadFrame),
    /// The session just ended (one or both fingers lifted) — carries no
    /// frame since the virtual device should simply clear to untouched.
    End,
}

/// A request to persist current state to `state.json`. The router knows
/// *when* pad state changed and needs writing, but not *how* — it just
/// signals intent and lets the caller's `state_write_loop` do the actual
/// write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateWrite {
    /// Digital state change (touch transition, gesture enter/exit) — write
    /// immediately, bypassing rate limiting.
    Immediate,
    /// Analog movement tick — already rate-limited to ~60 Hz by the caller.
    Analog,
}

/// Identifies one physical trackpad, used by `decide_gesture_transition` to
/// say which pad "survives" when a combined gesture session ends with only
/// one finger still down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pad {
    Left,
    Right,
}

/// Result of feeding one `PadFrame` into the combined-gesture state machine.
/// Pure and I/O-free so the entry/exit logic can be unit-tested directly,
/// independent of which pad physically touched down first or lifted first —
/// see the `tests` module below for the symmetry checks this exists to
/// guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GestureTransition {
    /// Whether a combined gesture session is active *after* this frame.
    pub now_active: bool,
    /// True only on the single frame where the session starts (both pads
    /// just became simultaneously touched) — callers use this to force a
    /// clean lift on any individual per-pad MT devices before combined
    /// events start.
    pub entering: bool,
    /// True only on the single frame where an active session ends (one or
    /// both fingers lifted).
    pub exiting: bool,
    /// If a session just ended (`exiting`) and exactly one finger is still
    /// touching, this is that pad — it should resume on its own individual
    /// MT device with a synthetic touch-down. `None` if both fingers lifted
    /// at the same time (nothing to resume).
    pub resume_survivor: Option<Pad>,
}

/// Decides the combined-gesture state transition for `frame`, given whether
/// a session was already active going into it. Entry requires both pads
/// touching simultaneously; exit fires as soon as either lifts — both
/// checks are pad-order-independent by construction (they only look at
/// `ltouch`/`rtouch` together, never which one changed first), which is
/// what guarantees "start left, add right" and "start right, add left" (and
/// likewise for lifting) behave identically.
pub fn decide_gesture_transition(was_active: bool, frame: PadFrame) -> GestureTransition {
    let mut now_active = was_active;
    if frame.ltouch && frame.rtouch {
        now_active = true;
    }
    if now_active && (!frame.ltouch || !frame.rtouch) {
        let resume_survivor = if frame.ltouch {
            Some(Pad::Left)
        } else if frame.rtouch {
            Some(Pad::Right)
        } else {
            None
        };
        return GestureTransition { now_active: false, entering: false, exiting: was_active, resume_survivor };
    }
    GestureTransition {
        now_active,
        entering: !was_active && now_active,
        exiting: false,
        resume_survivor: None,
    }
}

/// Runs the Core trackpad router until `rx` closes (device unplugged /
/// reader task ended). Always updates `lpad`/`rpad` raw state for state.json
/// export; additionally routes frames to whichever handler channels are
/// `Some` — a `None` channel means no handler is attached for that pad/mode
/// and is skipped entirely (never sent into), so a disabled channel can
/// never block this loop once its buffer fills.
///
/// Not spawned as an independent task — called from `EventReader::run`
/// inside `tokio::join!`, so it can borrow `PadState`/`gesture_session` etc.
/// for its whole lifetime without needing `'static` bounds or extra Arc
/// cloning.
pub async fn run(
    mut rx: mpsc::Receiver<PadFrame>,
    lpad: &PadState,
    rpad: &PadState,
    gesture_session: &Arc<Mutex<bool>>,
    state_tx: mpsc::Sender<StateWrite>,
    left_tx: Option<mpsc::Sender<SinglePadFrame>>,
    right_tx: Option<mpsc::Sender<SinglePadFrame>>,
    combined_tx: Option<mpsc::Sender<GestureEvent>>,
) {
    // Previous frame, to know which pad(s) actually changed and whether a
    // touch transition happened — every recv() is already a real change vs.
    // the *last sent* frame (deduped in pad_hidraw.rs), but that dedup is
    // over the whole frame, not per pad.
    let mut prev = PadFrame::default();
    // Gesture session state: true once both pads were simultaneously
    // touched. Stays true until both fingers are fully lifted — events
    // route to the combined gesture channel while active. Only meaningful
    // when a combined handler is attached at all (`combined_tx.is_some()`).
    let mut gesture_active = false;
    // After GestureEvent::End, suppress single-pad routing for each pad until
    // it has physically lifted (rtouch/ltouch → false). A pad that stays
    // touching after a gesture (held as anchor) would otherwise deliver
    // touching=true frames to its single-pad device, which KWin treats as a
    // new touch and immediately cancels kinetic scroll from the gesture device.
    // A lift event (touching=false) does NOT cancel kinetic, so we pass that
    // through to clear the handler's state.
    let mut suppress_left_until_lift = false;
    let mut suppress_right_until_lift = false;
    const SURVIVOR_MOVE_THRESHOLD: i64 = 4000;
    let mut left_gesture_end_pos: (i32, i32) = (0, 0);
    let mut right_gesture_end_pos: (i32, i32) = (0, 0);
    // Rate-limit state.json writes from trackpad movement to ~60 Hz. Touch
    // transitions (lift/touch-down) bypass this and write immediately.
    let mut last_state_write = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_millis(17))
        .unwrap_or_else(std::time::Instant::now);

    while let Some(frame) = rx.recv().await {
        let delta = diff_frames(prev, frame);
        let (l_changed, r_changed, touch_transition) =
            (delta.l_changed, delta.r_changed, delta.touch_transition);
        prev = frame;

        // Raw state export must reflect hardware truth regardless of which
        // (if any) handler is attached — always write these, unconditionally.
        *lpad.position.lock().await = (frame.lx, frame.ly);
        *lpad.touching_hw.lock().await = frame.ltouch;
        *lpad.pressed.lock().await = frame.lclick;
        *rpad.position.lock().await = (frame.rx, frame.ry);
        *rpad.touching_hw.lock().await = frame.rtouch;
        *rpad.pressed.lock().await = frame.rclick;

        if !l_changed && !r_changed {
            continue;
        }

        // Combined two-finger gesture routing is only meaningful if a
        // combined handler is actually attached — no combined_tx means no
        // gesture-session interpretation happens at all, and l/r data
        // always routes individually.
        if let Some(combined_tx) = &combined_tx {
            let was_gesture_active = gesture_active;
            let transition = decide_gesture_transition(gesture_active, frame);
            gesture_active = transition.now_active;

            // First frame of gesture session: lift any individual channel
            // that currently has a finger down so its handler sees a clean
            // state before combined events start.
            if transition.entering {
                if let Some(tx) = &left_tx {
                    let _ = tx.send(SinglePadFrame::default()).await;
                }
                if let Some(tx) = &right_tx {
                    let _ = tx.send(SinglePadFrame::default()).await;
                }
            }

            if transition.now_active {
                let combined_frame = CombinedPadFrame {
                    lx: frame.lx,
                    ly: frame.ly,
                    ltouch: frame.ltouch,
                    lclick: frame.lclick,
                    rx: frame.rx,
                    ry: frame.ry,
                    rtouch: frame.rtouch,
                    rclick: frame.rclick,
                };
                let event = if transition.entering {
                    GestureEvent::Start(combined_frame)
                } else {
                    GestureEvent::Move(combined_frame)
                };
                let _ = combined_tx.send(event).await;
            } else if transition.exiting {
                gesture_active = false;
                let _ = combined_tx.send(GestureEvent::End).await;
                suppress_left_until_lift = true;
                suppress_right_until_lift = true;
                left_gesture_end_pos = (frame.lx, frame.ly);
                right_gesture_end_pos = (frame.rx, frame.ry);
            }

            if transition.now_active || transition.exiting {
                // Sync to Arc so write_state_inner can read current gesture state.
                *gesture_session.lock().await = gesture_active;
                // Gesture entry/exit is a digital state change — always
                // write state.json immediately.
                if gesture_active != was_gesture_active {
                    let _ = state_tx.send(StateWrite::Immediate).await;
                }
            }
        }

        if !gesture_active {
            if l_changed {
                if !frame.ltouch {
                    suppress_left_until_lift = false;
                } else if suppress_left_until_lift {
                    let dx = (frame.lx - left_gesture_end_pos.0) as i64;
                    let dy = (frame.ly - left_gesture_end_pos.1) as i64;
                    if dx * dx + dy * dy > SURVIVOR_MOVE_THRESHOLD * SURVIVOR_MOVE_THRESHOLD {
                        suppress_left_until_lift = false;
                    }
                }
                if !suppress_left_until_lift {
                    if let Some(tx) = &left_tx {
                        let _ = tx
                            .send(SinglePadFrame {
                                x: frame.lx,
                                y: frame.ly,
                                touching: frame.ltouch,
                                click: frame.lclick,
                            })
                            .await;
                    }
                }
            }
            if r_changed {
                if !frame.rtouch {
                    suppress_right_until_lift = false;
                } else if suppress_right_until_lift {
                    let dx = (frame.rx - right_gesture_end_pos.0) as i64;
                    let dy = (frame.ry - right_gesture_end_pos.1) as i64;
                    if dx * dx + dy * dy > SURVIVOR_MOVE_THRESHOLD * SURVIVOR_MOVE_THRESHOLD {
                        suppress_right_until_lift = false;
                    }
                }
                if !suppress_right_until_lift {
                    if let Some(tx) = &right_tx {
                        let _ = tx
                            .send(SinglePadFrame {
                                x: frame.rx,
                                y: frame.ry,
                                touching: frame.rtouch,
                                click: frame.rclick,
                            })
                            .await;
                    }
                }
            }
        }

        // Signal state.json write, rate-limited to ~60 Hz. Touch transitions
        // bypass the rate limit so touching=false is always written promptly.
        if touch_transition {
            last_state_write = std::time::Instant::now();
            let _ = state_tx.send(StateWrite::Immediate).await;
        } else if last_state_write.elapsed().as_millis() >= 16 {
            last_state_write = std::time::Instant::now();
            let _ = state_tx.send(StateWrite::Analog).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shorthand for building a `PadFrame` straight from touch state, for
    /// the gesture-transition tests below where exact position doesn't
    /// matter.
    fn touch_frame(ltouch: bool, rtouch: bool) -> PadFrame {
        PadFrame {
            lx: 0,
            ly: 0,
            ltouch,
            lclick: false,
            rx: 0,
            ry: 0,
            rtouch,
            rclick: false,
        }
    }

    // --- diff_frames change detection --------------------------------------

    /// Regression guard for the "staircase diagonal" bug: on evdev, X and Y
    /// arrived as two separate SYN_REPORT frames, so software had to coalesce
    /// intermediate half-updated states. `PadFrame` has no such intermediate
    /// state (see `pad_hidraw.rs`'s atomicity tests), so every step of a
    /// diagonal drag must be detected as changed here too.
    #[test]
    fn diagonal_movement_is_always_detected_as_changed() {
        let mut prev = PadFrame::default();
        for step in 1..=20i32 {
            let curr = PadFrame { rx: step * 100, ry: -step * 50, rtouch: true, ..PadFrame::default() };
            let delta = diff_frames(prev, curr);
            assert!(delta.r_changed, "step {step} not detected as changed");
            prev = curr;
        }
    }

    /// A lift followed by a touch-down elsewhere must both be reported as
    /// touch transitions — this is what lets the router bypass the ~60 Hz
    /// state.json rate limit for digital transitions (see `run` above).
    #[test]
    fn lift_and_retouch_elsewhere_are_both_touch_transitions() {
        let touching_at_a = PadFrame { rx: 1000, ry: 2000, rtouch: true, ..PadFrame::default() };
        let lifted = PadFrame { rx: 1000, ry: 2000, rtouch: false, ..PadFrame::default() };
        let touching_at_b = PadFrame { rx: 5000, ry: 6000, rtouch: true, ..PadFrame::default() };

        let lift_delta = diff_frames(touching_at_a, lifted);
        assert!(lift_delta.touch_transition, "lift was not detected as a touch transition");

        let retouch_delta = diff_frames(lifted, touching_at_b);
        assert!(retouch_delta.touch_transition, "retouch was not detected as a touch transition");
    }

    /// A frame identical to the previous one must be reported as unchanged —
    /// otherwise the router would route/emit and write state.json
    /// continuously even while the finger rests still.
    #[test]
    fn identical_frame_is_not_a_change() {
        let a = PadFrame { lx: 1, ly: 2, ltouch: true, rx: 3, ry: 4, rtouch: true, ..PadFrame::default() };
        let b = a;
        let delta = diff_frames(a, b);
        assert!(!delta.l_changed);
        assert!(!delta.r_changed);
        assert!(!delta.touch_transition);
    }

    /// A click-only change (touch and position unchanged) must still count
    /// as a change — otherwise a tap-click with the finger resting still
    /// would never reach a routed channel.
    #[test]
    fn click_only_change_is_detected() {
        let a = PadFrame { lx: 1, ly: 2, ltouch: true, rx: 3, ry: 4, rtouch: true, ..PadFrame::default() };
        let b = PadFrame { lclick: true, ..a };
        let delta = diff_frames(a, b);
        assert!(delta.l_changed, "left click-only change was not detected");
        assert!(!delta.r_changed);
    }

    // --- Combined-gesture transition symmetry -----------------------------
    //
    // These pin down that entering/exiting a combined two-finger gesture
    // session behaves identically no matter which pad touched down or
    // lifted first — the asymmetry a manual test session found (right
    // continues after left lifts, but not vice versa) turned out to be
    // explained by config (left pad mode = disabled, no individual device to
    // resume onto), not by the transition logic itself. These tests assume a
    // symmetric setup (both pads capable of resuming) so a *real* asymmetry
    // in `decide_gesture_transition` would fail here regardless of config.

    #[test]
    fn gesture_enters_when_both_touch_regardless_of_which_touched_first() {
        // Left touched first, right added later.
        let mut active = false;
        let t1 = decide_gesture_transition(active, touch_frame(true, false));
        assert!(!t1.now_active && !t1.entering, "single-pad touch must not start a gesture");
        active = t1.now_active;
        let t2 = decide_gesture_transition(active, touch_frame(true, true));
        assert!(t2.now_active && t2.entering, "adding the second pad must start the gesture");

        // Mirror: right touched first, left added later.
        let mut active = false;
        let t1 = decide_gesture_transition(active, touch_frame(false, true));
        assert!(!t1.now_active && !t1.entering);
        active = t1.now_active;
        let t2 = decide_gesture_transition(active, touch_frame(true, true));
        assert!(t2.now_active && t2.entering);
    }

    #[test]
    fn gesture_exit_survivor_is_right_when_left_lifts_first() {
        // Active session, left finger lifts, right keeps touching.
        let transition = decide_gesture_transition(true, touch_frame(false, true));
        assert!(!transition.now_active);
        assert!(transition.exiting);
        assert_eq!(transition.resume_survivor, Some(Pad::Right));
    }

    #[test]
    fn gesture_exit_survivor_is_left_when_right_lifts_first() {
        // Mirror of the above: active session, right lifts, left keeps
        // touching. Must behave identically (just mirrored), or the manual
        // "one direction has no follow-through" report would be a real bug.
        let transition = decide_gesture_transition(true, touch_frame(true, false));
        assert!(!transition.now_active);
        assert!(transition.exiting);
        assert_eq!(transition.resume_survivor, Some(Pad::Left));
    }

    #[test]
    fn gesture_exit_no_survivor_when_both_lift_simultaneously() {
        let transition = decide_gesture_transition(true, touch_frame(false, false));
        assert!(!transition.now_active);
        assert!(transition.exiting);
        assert_eq!(transition.resume_survivor, None);
    }

    #[test]
    fn gesture_continues_while_both_still_touching() {
        let transition = decide_gesture_transition(true, touch_frame(true, true));
        assert!(transition.now_active);
        assert!(!transition.entering);
        assert!(!transition.exiting);
        assert_eq!(transition.resume_survivor, None);
    }
}
