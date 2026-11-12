//! What a raw Steam Deck event *is*, decided without touching anything.
//!
//! `read_event` used to decide this inline, between lock acquisitions and
//! `convert_event` calls, which is why none of it had a test. The decisions are
//! here now and the reader only feeds them and applies what comes back — the
//! same split `machine.rs` made for what an event *means*.
//!
//! Only what the Steam Deck can actually produce moved. The Deck is read via
//! hidraw and `deckery-controller` synthesises its evdev stream
//! (`hid_report::synthesise_input_events`): KEY edges, ten ABS axes (both pads,
//! both analog triggers, both sticks) and SYN_REPORT, nothing else. The arms
//! for pens, scroll wheels, ABS_WHEEL dials, digital triggers on ABS_Z/ABS_RZ
//! and the Stadia controller are inherited from upstream makima, cannot be
//! reached from that stream, and were left in the reader exactly as they were.

use crate::config::{Axis, Event};
use crate::machine::{axis_value, decode_stick_bind, Stick};

use evdev::Key;

/// The logical event for a key edge, or `None` if the reader must swallow it.
///
/// Every key is a key, the D-pad included: the Deck reports its D-pad as
/// `BTN_DPAD_*` key edges, and a config names it the same way. Issue #44 was
/// the config side treating those names as axes, so a D-pad binding matched
/// nothing; the reader has to keep producing `Event::Key` for them for the
/// two sides to meet.
///
/// The one exception is the click of a pad that a handler reads from hidraw
/// (`TrackpadSideConfig::has_handler`: `mt-trackpad` or `trackball`). That
/// click lives in the same hidraw byte as the touch, so the handler gets it
/// atomically with position and touch. Letting the key edge through as well
/// would leak it onto the generic key device as a second, unsynchronised
/// click.
pub fn key_event(key: Key, left_has_handler: bool, right_has_handler: bool) -> Option<Event> {
    match key {
        Key::BTN_THUMB if left_has_handler => None,
        Key::BTN_THUMB2 if right_has_handler => None,
        _ => Some(Event::Key(key)),
    }
}

/// What a hat-switch value amounts to on a D-pad, and the direction now held.
///
/// Mirrors `decode_stick_bind` for an axis that only ever reports -1, 0 or 1:
/// -1 and 1 press a direction, 0 releases whichever one was held, and any
/// other value is not a hat value at all and changes nothing. Going straight
/// from one direction to the other releases the old one first. Unlike a stick,
/// a press is sent again if the same direction arrives twice — that is how the
/// reader always behaved, and hats do not repeat a value on their own.
///
/// Only for an evdev-class controller with a real hat switch. A Steam Deck
/// never comes through here: its D-pad arrives as key edges (see `key_event`),
/// and the ABS_HAT0X/Y it reports are the left pad's coordinates, which the
/// reader swallows.
pub fn decode_hat(is_x: bool, value: i32, held: i32) -> (Vec<(Axis, i32)>, i32) {
    let (negative, positive) = if is_x {
        (Axis::BTN_DPAD_LEFT, Axis::BTN_DPAD_RIGHT)
    } else {
        (Axis::BTN_DPAD_UP, Axis::BTN_DPAD_DOWN)
    };
    match value {
        -1 if held == 1 => (vec![(positive, 0), (negative, 1)], -1),
        1 if held == -1 => (vec![(negative, 0), (positive, 1)], 1),
        -1 => (vec![(negative, 1)], -1),
        1 => (vec![(positive, 1)], 1),
        0 => {
            let events = match held {
                -1 => vec![(negative, 0)],
                1 => vec![(positive, 0)],
                _ => vec![],
            };
            (events, 0)
        }
        _ => (vec![], held),
    }
}

/// Which stick an absolute axis code belongs to, and whether it is that
/// stick's X axis.
///
/// Only meaningful for `ABS_X`, `ABS_Y`, `ABS_RX` and `ABS_RY`, the four codes
/// the reader's stick arm matches; everything below `ABS_Z` is the left stick.
pub fn stick_for_code(code: u16) -> (Stick, bool) {
    let stick = if code < 3 { Stick::Left } else { Stick::Right };
    (stick, code == stick.base_code())
}

/// What one stick axis update is for, decided by the stick's configured
/// function.
#[derive(Debug, Clone, PartialEq)]
pub enum StickUpdate {
    /// `cursor` or `scroll`: store the scaled value at this index (0 = X,
    /// 1 = Y) of the stick's position, for the analog loop to read on its
    /// next tick.
    Position { index: usize, value: i32 },
    /// `bind`: send these directional events and keep the new held state.
    Bind { events: Vec<(Axis, i32)>, held: (i32, i32) },
    /// Anything else, `disabled` included: the stick drives nothing.
    Ignore,
}

/// Decode one raw stick axis value against the stick's settings.
///
/// The raw value is recorded for the overlay regardless of this answer; that
/// part stays in the reader because it is a write, not a decision.
pub fn stick_update(
    function: &str,
    stick: Stick,
    is_x: bool,
    raw: i32,
    deadzone: i32,
    axis_16_bit: bool,
    held: (i32, i32),
) -> StickUpdate {
    let value = axis_value(raw, deadzone, axis_16_bit);
    match function {
        "cursor" | "scroll" => StickUpdate::Position { index: if is_x { 0 } else { 1 }, value },
        "bind" => {
            let (events, held) = decode_stick_bind(stick, is_x, value.signum(), held);
            StickUpdate::Bind { events, held }
        }
        _ => StickUpdate::Ignore,
    }
}

/// The shortest interval between two state.json writes caused by stick or
/// analog trigger movement, in milliseconds — roughly 60 Hz.
pub const ANALOG_WRITE_INTERVAL_MS: u128 = 16;

/// Whether a SYN_REPORT should write analog state.
///
/// Only when an analog axis changed since the last SYN, and not more often
/// than every `ANALOG_WRITE_INTERVAL_MS`: every frame of stick movement ends in
/// a SYN, and a write per frame would rewrite the file at the hidraw rate.
pub fn analog_write_due(changed: bool, since_last_write: std::time::Duration) -> bool {
    changed && since_last_write.as_millis() >= ANALOG_WRITE_INTERVAL_MS
}

#[cfg(test)]
#[path = "decode_tests.rs"]
mod tests;
