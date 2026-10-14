//! What the input path does, as data.
//!
//! Today every effect of an event is performed where it is decided: a key is
//! written to uinput inside `emit_event`, a command is spawned inside
//! `convert_event`, Gaming Mode is toggled inside the double-click branch. The
//! effect has no name and leaves no trace, which is why the binding engine has
//! no integration tests — there is no seam to look at.
//!
//! `OutputAction` is that name. It is data only: no `Arc`, no `Mutex`, no I/O,
//! and `PartialEq`, so two implementations of the same decision can be compared
//! rather than argued about.

use deckery_controller::{HapticChain, HapticPad};

/// One evdev event, stripped of its timestamp.
///
/// `evdev::InputEvent` is not `PartialEq` and carries the moment it was made,
/// so the same input replayed a millisecond later produces a different value.
/// The kernel derives no behaviour from that timestamp for anything we emit, so
/// `Ev` is the identity of an emitted event — and the whole comparison rests on
/// this being true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub struct Ev {
    pub ev_type: u16,
    pub code: u16,
    pub value: i32,
}

impl From<&evdev::InputEvent> for Ev {
    fn from(e: &evdev::InputEvent) -> Self {
        Self { ev_type: e.event_type().0, code: e.code(), value: e.value() }
    }
}

/// Which virtual device a frame goes to.
///
/// One variant per field of `VirtualDevices`. The virtual devices are a single
/// sink that distinguishes internally — not six sinks — so that everything
/// writing to uinput shares this vocabulary without sharing a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum OutputDevice {
    Keys,
    Axis,
    Abs,
    LeftPad,
    RightPad,
    GesturePad,
}


/// A hardware event on its way through unchanged.
///
/// Wrapped rather than used bare so the enum can keep its derive: `InputEvent`
/// has no `PartialEq`, and the equality that belongs here is not the derived one
/// anyway.
#[derive(Debug, Clone, Copy)]
pub struct Raw(pub evdev::InputEvent);

impl PartialEq for Raw {
    /// Equal when the event is, timestamp aside.
    ///
    /// The timestamp belongs to the hardware, not to any decision — the decision
    /// was "forward this", and the payload was never chosen. So two forwards of
    /// the same event are the same action however far apart they happened.
    fn eq(&self, other: &Self) -> bool {
        Ev::from(&self.0) == Ev::from(&other.0)
    }
}

/// One action, numbered, on its way out of a port.
///
/// The sequence number is process-wide and monotonic, so actions from different
/// tasks have a total order even though they arrive independently.
/// Read only by whatever is watching, and in production nothing is yet — the
/// tests read it, and a log would. Marked here rather than muting the module.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Emission {
    pub seq: u64,
    pub t_us: u128,
    pub action: OutputAction,
}

/// One thing the input path does.
#[derive(Debug, Clone, PartialEq)]
pub enum OutputAction {
    /// Exactly one `VirtualDevice::emit()` call, which is exactly one kernel
    /// frame — `evdev` appends a SYN_REPORT per call.
    ///
    /// The granularity is per emit, **not** per binding, and that is load
    /// bearing: `cursor_loop` emits REL_X and REL_Y as two separate calls, so
    /// the kernel sees two frames, and a pointer that moved diagonally in one
    /// frame is a different thing from one that moved twice. Merging them here
    /// would make a comparison agree about something the kernel never saw.
    ///
    /// `events` is the batch as passed, including any SYN the caller added
    /// itself (the trackpad path does; nothing else does). The recorded form is
    /// what the code handed over, never a tidied-up version.
    Frame { device: OutputDevice, events: Vec<Ev> },

    /// A hardware event passed through untouched.
    ///
    /// Separate from `Frame` because forwarding and synthesising are different
    /// things, and the difference is the timestamp: a synthesised event is
    /// stamped when it is emitted, whereas a forwarded one carries the
    /// hardware's own stamp and has to keep it — libinput reads timestamps for
    /// velocity and smoothing, so restamping a forwarded pen event changes what
    /// the desktop sees.
    ///
    /// The alternative was for `Ev` to carry a timestamp that its equality
    /// ignores. That hides a surprise in a type everything else compares with;
    /// this says out loud that there are two kinds of emission.
    Forward { device: OutputDevice, events: Vec<Raw> },

    /// Publish state, or change what the overlay says happened last.
    State(StateAction),

    /// Run a command binding in the user's session.
    Command { argv: Vec<String>, label: Option<String> },

    /// Change something about makima itself. No evdev, no subprocess.
    Internal(InternalCommand),

    /// Feedback to the controller.
    Haptic { pad: HapticPad, chain: HapticChain },
}

/// A mode change that belongs to makima rather than to the desktop.
///
/// One member today. The generalisation is deliberate: `SetGamingMode` is not an
/// output in the sense a keystroke is, and lumping it in with them would say the
/// wrong thing about what it is.
///
/// `paused` has no member here, and that is not an omission. Gaming Mode can be
/// produced by the event path — a double-click on the configured trigger — while
/// `paused` only ever arrives over IPC and is unknown to the resolver and to the
/// config format. This enum holds what the event path can decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InternalCommand {
    /// `preview` is the `paused` argument of `set_gaming_mode`: it suppresses
    /// the toggle and the haptic but still writes `last_action`, so the overlay
    /// shows what the press *would* do. Two calls that differ only in `preview`
    /// are two different actions.
    SetGamingMode { enabled: bool, preview: bool },
}

/// Whether a state publish may be dropped under load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Urgency {
    /// A discrete event. Sent with backpressure; must never be lost.
    Immediate,
    /// An analog tick, already rate-limited by the caller. Dropped rather than
    /// allowed to block the event loop.
    Analog,
}

/// Something to do to the exported state.
#[derive(Debug, Clone, PartialEq)]
pub enum StateAction {
    /// Publish this snapshot.
    ///
    /// It carries the snapshot rather than asking for one to be built, so the
    /// state port is a port like every other: something goes through it and
    /// whatever is clipped in writes it down. The snapshot is assembled by
    /// whoever has the live state, at the moment the action is produced.
    Publish { snapshot: Box<serde_json::Value>, urgency: Urgency },
    /// Replace `last_action`, or clear it.
    SetLastAction(Option<LastActionSpec>),
}

/// `state_export::LastAction` without its timestamp.
///
/// `ts` is `now_ts()`, so keeping it would make every comparison fail for a
/// reason that has nothing to do with the decision being compared.
#[derive(Debug, Clone, PartialEq)]
pub struct LastActionSpec {
    pub r#type: String,
    pub value: serde_json::Value,
    pub label: Option<String>,
    pub silent: bool,
}

#[cfg(test)]
#[path = "action_tests.rs"]
mod tests;
