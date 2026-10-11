//! What the input path does, as data — and the ports it goes out through.
//!
//! The part that decides returns a `Vec<OutputAction>` and touches no device, no
//! socket and no process. `Outputs` holds one port per kind, each either clipped
//! in or empty, and pushes each action through its own. An empty port means the
//! action has nowhere to go and nothing happens — which is what will let the
//! binding engine be tested with no `/dev/uinput` and no session bus.
//!
//! ```text
//!   Vec<OutputAction> ──▶ Outputs ──┬─ devices  ──▶ uinput
//!                                   ├─ state    ──▶ state.json
//!                                   ├─ commands ──▶ user session
//!                                   ├─ internal ──▶ back in as the next input
//!                                   ├─ haptics  ──▶ hidraw
//!                                   └─ tap      ──▶ the log, or a test
//! ```
//!
//! `internal` is the one port that loops back: Gaming Mode leaves as an action
//! and returns as the next input. That makes this a state machine rather than a
//! function, and it is an advantage for testing — a mode change is observable as
//! an action before it is believed as state.
//!
//! Partly ahead of its callers still. The device port carries every frame the
//! event reader decides; the state, command and internal ports are built and
//! tested but only reached for some kinds, because `convert_event` and
//! `emit_nonmapped_event` still perform their own effects. The allow covers that
//! gap and should shrink to nothing as they are converted.
#![allow(dead_code)]

pub mod action;
pub mod outputs;

pub use action::OutputDevice;
pub use outputs::Outputs;
