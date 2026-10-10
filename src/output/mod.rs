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
//! **This module is one increment ahead of its callers.** The vocabulary and the
//! ports are written; nothing produces actions yet, because the event reader
//! still performs its effects at the point where it decides them. Converting it
//! is the next step, and until then these names have no users — hence the allow
//! below. It goes away with the first producer, and if it is still here
//! afterwards then the conversion is unfinished.
#![allow(dead_code)]

pub mod action;
pub mod outputs;

// Only what callers outside this module name today. The rest is reached through
// `output::action::…` / `output::outputs::…` until there is a caller for it.
pub use action::OutputDevice;
