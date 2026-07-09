use crate::virtual_devices::VirtualDevices;
use evdev::{EventType, InputEvent, Key};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Per-pad state shared between the pad hidraw reader (`pad_hidraw.rs`,
/// writes position/touching) and the rest of makima (reads them for state
/// export, click forwarding, etc). Position and touch state are always
/// written together as one atomic frame by `EventReader::pad_loop` — see
/// `pad_hidraw.rs` for why that atomicity matters.
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

    pub async fn emit(
        &self,
        virt_dev: &Arc<Mutex<VirtualDevices>>,
        x: i32,
        y: i32,
        touching: bool,
        click: Option<bool>,
    ) {
        let mut vd = virt_dev.lock().await;
        let dev = if self.is_left { vd.lpad.as_mut() } else { vd.rpad.as_mut() };
        let dev = match dev {
            Some(d) => d,
            None => return,
        };
        let y = -y; // Steam Deck Y increases upward; libinput expects downward.

        let mut events = Vec::new();
        events.push(InputEvent::new_now(EventType::KEY, Key::BTN_TOUCH.code(), touching as i32));
        events.push(InputEvent::new_now(EventType::KEY, Key::BTN_TOOL_FINGER.code(), touching as i32));

        if touching {
            events.push(InputEvent::new_now(EventType::ABSOLUTE, 47, 0)); // ABS_MT_SLOT = 0
            events.push(InputEvent::new_now(EventType::ABSOLUTE, 57, 1)); // ABS_MT_TRACKING_ID
            events.push(InputEvent::new_now(EventType::ABSOLUTE, 53, x)); // ABS_MT_POSITION_X
            events.push(InputEvent::new_now(EventType::ABSOLUTE, 54, y)); // ABS_MT_POSITION_Y
            events.push(InputEvent::new_now(EventType::ABSOLUTE, 0, x));  // ABS_X (compat)
            events.push(InputEvent::new_now(EventType::ABSOLUTE, 1, y));  // ABS_Y (compat)
        } else {
            events.push(InputEvent::new_now(EventType::ABSOLUTE, 47, 0));  // ABS_MT_SLOT
            events.push(InputEvent::new_now(EventType::ABSOLUTE, 57, -1)); // ABS_MT_TRACKING_ID = -1 (lift)
        }

        if let Some(pressed) = click {
            events.push(InputEvent::new_now(EventType::KEY, Key::BTN_LEFT.code(), pressed as i32));
        }

        events.push(InputEvent::new_now(EventType::SYNCHRONIZATION, 0, 0));
        if click.is_some() {
            eprintln!(
                "[trackpad-debug] {}pad emit: touch={} click={:?}",
                if self.is_left { "l" } else { "r" },
                touching,
                click
            );
        }
        dev.emit(&events).ok();
    }
}

/// Emit a combined two-finger gesture event to the gesture pad device.
/// `l_touching` / `r_touching` must come from hidraw hardware state.
/// Pass both `false` to force a clean lift on both slots (gesture exit).
/// `click` is the combined click state (`pad_hidraw::combined_click` — a
/// physical click on either half of the pad while a gesture session is
/// active reads as one click on this device) — `None` leaves BTN_LEFT
/// untouched, e.g. for the initial "both pads just started touching" frame
/// where a stale click value doesn't matter yet.
pub async fn emit_gesture_event(
    virt_dev: &Arc<Mutex<VirtualDevices>>,
    lx: i32,
    ly: i32,
    rx: i32,
    ry: i32,
    l_touching: bool,
    r_touching: bool,
    click: Option<bool>,
) {
    let mut vd = virt_dev.lock().await;
    let dev = match vd.gesture_pad.as_mut() {
        Some(d) => d,
        None => return,
    };
    let ly = -ly;
    let ry = -ry;

    // Map each pad to its own half of the combined X space so pinch works correctly.
    //   Left  X ∈ [-32767..+32767] → combined X ∈ [-32767..0]
    //   Right X ∈ [-32767..+32767] → combined X ∈ [0..+32767]
    let combined_lx = (lx - 32767) / 2;
    let combined_rx = (rx + 32767) / 2;

    let mut events = Vec::new();

    events.push(InputEvent::new_now(EventType::ABSOLUTE, 47, 0)); // Slot 0 = left pad
    if l_touching {
        events.push(InputEvent::new_now(EventType::ABSOLUTE, 57, 1));            // ABS_MT_TRACKING_ID
        events.push(InputEvent::new_now(EventType::ABSOLUTE, 53, combined_lx)); // ABS_MT_POSITION_X
        events.push(InputEvent::new_now(EventType::ABSOLUTE, 54, ly));           // ABS_MT_POSITION_Y
    } else {
        events.push(InputEvent::new_now(EventType::ABSOLUTE, 57, -1)); // lift
    }

    events.push(InputEvent::new_now(EventType::ABSOLUTE, 47, 1)); // Slot 1 = right pad
    if r_touching {
        events.push(InputEvent::new_now(EventType::ABSOLUTE, 57, 2));            // ABS_MT_TRACKING_ID
        events.push(InputEvent::new_now(EventType::ABSOLUTE, 53, combined_rx)); // ABS_MT_POSITION_X
        events.push(InputEvent::new_now(EventType::ABSOLUTE, 54, ry));           // ABS_MT_POSITION_Y
    } else {
        events.push(InputEvent::new_now(EventType::ABSOLUTE, 57, -1)); // lift
    }

    let any  = l_touching || r_touching;
    let both = l_touching && r_touching;
    events.push(InputEvent::new_now(EventType::KEY, Key::BTN_TOUCH.code(),          any  as i32));
    events.push(InputEvent::new_now(EventType::KEY, Key::BTN_TOOL_DOUBLETAP.code(), both as i32));
    events.push(InputEvent::new_now(EventType::KEY, Key::BTN_TOOL_FINGER.code(),    (any && !both) as i32));

    if let Some(pressed) = click {
        events.push(InputEvent::new_now(EventType::KEY, Key::BTN_LEFT.code(), pressed as i32));
    }

    events.push(InputEvent::new_now(EventType::SYNCHRONIZATION, 0, 0));
    eprintln!(
        "[trackpad-debug] gesture emit: l_touch={} r_touch={} click={:?}",
        l_touching, r_touching, click
    );
    dev.emit(&events).ok();
}
