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
//! Every kind of action is now produced by something: frames, commands, the
//! Gaming Mode change, the haptic, the snapshot and `last_action` all leave the
//! event reader through a port. The trackpads reach the same ports — their
//! haptics and their publish requests are actions too (issue #57), and the pad
//! frames themselves are the last step left. The only thing with no production
//! caller is the tap, because nothing watches yet — the tests do, and a log
//! would. That is marked where it lives rather than across the module.

pub mod action;
pub mod outputs;

pub use action::OutputDevice;
pub use outputs::Outputs;
