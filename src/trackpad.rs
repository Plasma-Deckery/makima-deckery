//! Turning a pad's position and touch state into the frame it means.
//!
//! Both planners here are pure: they build a frame and return it as an action,
//! and `Outputs::dispatch` is what reaches a device. One `Frame` action is one
//! `emit()` call is one kernel frame — `evdev` appends a SYN_REPORT per call —
//! so each planner returns exactly one action and never merges two.

use crate::output::action::{Ev, OutputAction};
use crate::output::OutputDevice;
use evdev::{EventType, Key};
use std::sync::Arc;
use tokio::sync::Mutex;

/// ABS codes the kernel names but `evdev`'s Rust bindings do not expose as
/// constants. Spelled out once here rather than as bare numbers with a trailing
/// comment at each of the eleven places they were used.
mod abs {
    pub const X: u16 = 0;
    pub const Y: u16 = 1;
    pub const MT_SLOT: u16 = 47;
    pub const MT_POSITION_X: u16 = 53;
    pub const MT_POSITION_Y: u16 = 54;
    pub const MT_TRACKING_ID: u16 = 57;
}

/// A tracking id of -1 is how a slot reports that the finger has left it.
const SLOT_LIFTED: i32 = -1;

/// The widest value a pad axis reports, in either direction.
const AXIS_MAX: i32 = 32767;

fn ev(ev_type: EventType, code: u16, value: i32) -> Ev {
    Ev { ev_type: ev_type.0, code, value }
}

/// Per-pad state shared between the trackpad router, which writes position,
/// touch and click as one atomic frame (`trackpad_router::run`), and the state
/// export, which reads them. Atomicity matters so a reader never sees a stale
/// position paired with an updated touch flag.
pub struct PadState {
    is_left: bool,
    pub position: Arc<Mutex<(i32, i32)>>,
    pub touching_hw: Arc<Mutex<bool>>,
    pub pressed: Arc<Mutex<bool>>,
}

impl PadState {
    pub fn new(is_left: bool) -> Self {
        Self {
            is_left,
            position: Arc::new(Mutex::new((0, 0))),
            touching_hw: Arc::new(Mutex::new(false)),
            pressed: Arc::new(Mutex::new(false)),
        }
    }

    /// Which virtual device this pad's frames go to.
    pub fn device(&self) -> OutputDevice {
        if self.is_left { OutputDevice::LeftPad } else { OutputDevice::RightPad }
    }
}

/// One pad's state as the frame it means.
///
/// `click` of `None` leaves BTN_LEFT untouched rather than sending a zero, so a
/// frame that says nothing about the button does not release it.
pub fn plan_pad_frame(
    device: OutputDevice,
    x: i32,
    y: i32,
    touching: bool,
    click: Option<bool>,
) -> Vec<OutputAction> {
    let y = -y; // Steam Deck Y increases upward; libinput expects downward.

    let mut events = vec![
        ev(EventType::KEY, Key::BTN_TOUCH.code(), touching as i32),
        ev(EventType::KEY, Key::BTN_TOOL_FINGER.code(), touching as i32),
    ];

    if touching {
        events.extend([
            ev(EventType::ABSOLUTE, abs::MT_SLOT, 0),
            ev(EventType::ABSOLUTE, abs::MT_TRACKING_ID, 1),
            ev(EventType::ABSOLUTE, abs::MT_POSITION_X, x),
            ev(EventType::ABSOLUTE, abs::MT_POSITION_Y, y),
            // The single-touch compat pair, for readers that do not do MT.
            ev(EventType::ABSOLUTE, abs::X, x),
            ev(EventType::ABSOLUTE, abs::Y, y),
        ]);
    } else {
        events.extend([
            ev(EventType::ABSOLUTE, abs::MT_SLOT, 0),
            ev(EventType::ABSOLUTE, abs::MT_TRACKING_ID, SLOT_LIFTED),
        ]);
    }

    if let Some(pressed) = click {
        events.push(ev(EventType::KEY, Key::BTN_LEFT.code(), pressed as i32));
    }

    // The explicit SYN the old path pushed. `emit` appends one of its own per
    // call, so dropping this would change what the kernel sees.
    events.push(ev(EventType::SYNCHRONIZATION, 0, 0));

    vec![OutputAction::Frame { device, events }]
}

/// Both pads as one combined two-finger frame on the gesture device.
///
/// libinput derives everything it needs — two-finger scroll, pan, pinch-zoom —
/// from the two MT slots alone; there is no separate gesture-type input.
/// `l_touching`/`r_touching` must come from hidraw hardware state. Both `false`
/// forces a clean lift on both slots, which is how a gesture session ends.
/// `click` is the combined state: a physical click on either half while a
/// session is active reads as one click on this device.
pub fn plan_gesture_frame(
    lx: i32,
    ly: i32,
    rx: i32,
    ry: i32,
    l_touching: bool,
    r_touching: bool,
    click: Option<bool>,
) -> Vec<OutputAction> {
    let ly = -ly;
    let ry = -ry;

    // Each pad gets its own half of the combined X space, so the distance
    // between the two slots grows and shrinks the way a pinch needs.
    //   Left  X ∈ [-32767..+32767] → combined X ∈ [-32767..0]
    //   Right X ∈ [-32767..+32767] → combined X ∈ [0..+32767]
    let combined_lx = (lx - AXIS_MAX) / 2;
    let combined_rx = (rx + AXIS_MAX) / 2;

    let mut events = Vec::new();

    for (slot, tracking_id, touching, x, y) in [
        (0, 1, l_touching, combined_lx, ly),
        (1, 2, r_touching, combined_rx, ry),
    ] {
        events.push(ev(EventType::ABSOLUTE, abs::MT_SLOT, slot));
        if touching {
            events.extend([
                ev(EventType::ABSOLUTE, abs::MT_TRACKING_ID, tracking_id),
                ev(EventType::ABSOLUTE, abs::MT_POSITION_X, x),
                ev(EventType::ABSOLUTE, abs::MT_POSITION_Y, y),
            ]);
        } else {
            events.push(ev(EventType::ABSOLUTE, abs::MT_TRACKING_ID, SLOT_LIFTED));
        }
    }

    let any = l_touching || r_touching;
    let both = l_touching && r_touching;
    events.extend([
        ev(EventType::KEY, Key::BTN_TOUCH.code(), any as i32),
        ev(EventType::KEY, Key::BTN_TOOL_DOUBLETAP.code(), both as i32),
        ev(EventType::KEY, Key::BTN_TOOL_FINGER.code(), (any && !both) as i32),
    ]);

    if let Some(pressed) = click {
        events.push(ev(EventType::KEY, Key::BTN_LEFT.code(), pressed as i32));
    }

    events.push(ev(EventType::SYNCHRONIZATION, 0, 0));

    vec![OutputAction::Frame { device: OutputDevice::GesturePad, events }]
}

#[cfg(test)]
#[path = "trackpad_tests.rs"]
mod tests;
