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
//! Partly ahead of its callers still, and the gap is now exactly one thing: the
//! device port carries every frame anything decides, but the `State`, `Command`,
//! `Internal` and `Haptic` actions are never constructed, because the branches
//! of `convert_event` still call `set_last_emitted`, `spawn_subprocess` and
//! `set_gaming_mode` rather than returning what they want done. Routing is
//! decided by the machine; the effects of each route are not yet.
//!
//! That is what the allow covers, and removing it is the test for whether the
//! conversion is finished.
#![allow(dead_code)]

pub mod action;
pub mod outputs;

pub use action::OutputDevice;
pub use outputs::Outputs;
