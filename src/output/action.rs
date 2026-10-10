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

impl OutputDevice {
    /// The name used in `DECKERY_RECORD_SINKS` and in the journal.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keys => "keys",
            Self::Axis => "axis",
            Self::Abs => "abs",
            Self::LeftPad => "lpad",
            Self::RightPad => "rpad",
            Self::GesturePad => "gesture_pad",
        }
    }
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

/// One action, numbered and placed, on its way out of a port.
///
/// The sequence number is process-wide and monotonic, so records from different
/// tasks have a total order even though they arrive independently. `site` names
/// the branch that produced it, which is what turns "a frame differs" into "a
/// frame differs here".
#[derive(Debug, Clone)]
pub struct Emission {
    pub seq: u64,
    pub t_us: u128,
    pub site: &'static str,
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

/// Names for the places that produce a frame.
///
/// One table rather than thirty-odd string literals scattered through the event
/// reader, because whether each name sits at the right call site is the one
/// thing in this design no test can check. A reviewer can read this table; they
/// cannot read thirty-one literals spread over a thousand lines.
///
/// A wrong name does not change behaviour — it misattributes a future finding,
/// which is worse in a different way: it costs trust in the journal.
pub mod sites {
    pub const RELEASE_HELD_MODIFIERS: &str = "device_tasks/release_held_modifiers";
    pub const RELEASE_ALL_HELD: &str = "release_all_held";
    pub const KEY_UP_CLEANUP: &str = "convert_event/key_up_cleanup";
    pub const MODIFIER_INTERCEPT: &str = "convert_event/modifier_intercept";
    pub const PEN_FLUSH: &str = "event_loop/pen_flush";

    pub const EMIT_EVENT_MODIFIER_RELEASE: &str = "emit_event/modifier_release";
    pub const EMIT_EVENT_IGNORE_MODIFIERS: &str = "emit_event/ignore_modifiers";
    pub const EMIT_EVENT_MODIFIER_TAP_PRESS: &str = "emit_event/modifier_tap_press";
    pub const EMIT_EVENT_MODIFIER_TAP_RELEASE: &str = "emit_event/modifier_tap_release";
    pub const EMIT_EVENT_KEYS: &str = "emit_event/keys";

    pub const NONMAPPED_RELEASED_KEYS: &str = "emit_nonmapped_event/released_keys";
    pub const NONMAPPED_TAP_PRESS: &str = "emit_nonmapped_event/modifier_tap_press";
    pub const NONMAPPED_TAP_RELEASE: &str = "emit_nonmapped_event/modifier_tap_release";
    pub const NONMAPPED_KEY: &str = "emit_nonmapped_event/key";
    pub const NONMAPPED_RELATIVE: &str = "emit_nonmapped_event/relative";
    pub const NONMAPPED_ABSOLUTE: &str = "emit_nonmapped_event/absolute";
    pub const NONMAPPED_MISC: &str = "emit_nonmapped_event/misc";

    pub const DEFAULT_KEY: &str = "emit_default_event/key";
    pub const DEFAULT_RELATIVE: &str = "emit_default_event/relative";
    pub const DEFAULT_ABSOLUTE: &str = "emit_default_event/absolute";
    pub const DEFAULT_MISC: &str = "emit_default_event/misc";

    pub const CURSOR_LOOP_X: &str = "cursor_loop/x";
    pub const CURSOR_LOOP_Y: &str = "cursor_loop/y";
    pub const SCROLL_LOOP_X: &str = "scroll_loop/x";
    pub const SCROLL_LOOP_Y: &str = "scroll_loop/y";
    pub const KEY_CURSOR_LOOP_X: &str = "key_cursor_loop/x";
    pub const KEY_CURSOR_LOOP_Y: &str = "key_cursor_loop/y";
    pub const KEY_SCROLL_LOOP_X: &str = "key_scroll_loop/x";
    pub const KEY_SCROLL_LOOP_Y: &str = "key_scroll_loop/y";

    pub const PAD_FRAME: &str = "trackpad/pad_frame";
    pub const GESTURE_FRAME: &str = "trackpad/gesture_frame";

    pub const SET_GAMING_MODE: &str = "set_gaming_mode";
    pub const GAMING_MODE_HAPTIC: &str = "set_gaming_mode/haptic";
    pub const RUN_COMMAND: &str = "convert_event/command";
    pub const LAST_ACTION_KEYS: &str = "set_last_emitted";
    pub const LAST_ACTION_COMMAND: &str = "set_last_emitted_cmd";
    pub const PUBLISH_STATE: &str = "write_state";
}

#[cfg(test)]
#[path = "action_tests.rs"]
mod tests;
