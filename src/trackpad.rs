use crate::virtual_devices::VirtualDevices;
use evdev::{EventType, InputEvent, Key};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct PadState {
    is_left: bool,
    pub position: Arc<Mutex<(i32, i32)>>,
    pub touching_hw: Arc<Mutex<bool>>,
    pub pressed: Arc<Mutex<bool>>,
    pub x_fresh: AtomicBool,
    pub y_fresh: AtomicBool,
    pub emit_pending: AtomicBool,
    pub dirty: AtomicBool,
}

impl PadState {
    pub fn new(is_left: bool) -> Self {
        Self {
            is_left,
            position: Arc::new(Mutex::new((0, 0))),
            touching_hw: Arc::new(Mutex::new(false)),
            pressed: Arc::new(Mutex::new(false)),
            x_fresh: AtomicBool::new(false),
            y_fresh: AtomicBool::new(false),
            emit_pending: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
        }
    }

    pub fn reset_coalescing(&self) {
        self.dirty.store(false, Ordering::Relaxed);
        self.x_fresh.store(false, Ordering::Relaxed);
        self.y_fresh.store(false, Ordering::Relaxed);
        self.emit_pending.store(false, Ordering::Relaxed);
    }

    pub async fn process_syn(&self, virt_dev: &Arc<Mutex<VirtualDevices>>) {
        if self.dirty.load(Ordering::Relaxed) {
            self.dirty.store(false, Ordering::Relaxed);
            // If we were already pending, it means we were waiting for the other axis.
            // Don't wait any longer - emit now with whatever we have. This fixes the
            // single-axis jitter where pure vertical/horizontal movement would never
            // complete a frame (the other axis never arrives).
            let was_pending = self.emit_pending.swap(false, Ordering::Relaxed);
            if was_pending || (self.x_fresh.load(Ordering::Relaxed) && self.y_fresh.load(Ordering::Relaxed)) {
                self.x_fresh.store(false, Ordering::Relaxed);
                self.y_fresh.store(false, Ordering::Relaxed);
                let (x, y) = *self.position.lock().await;
                let touching = *self.touching_hw.lock().await;
                self.emit(virt_dev, x, y, touching, None).await;
            } else {
                // First frame with only one axis fresh - wait one SYN for the other
                self.emit_pending.store(true, Ordering::Relaxed);
            }
        } else if self.emit_pending.load(Ordering::Relaxed) {
            // Empty SYN fallback: the other axis never came - emit what we have
            self.emit_pending.store(false, Ordering::Relaxed);
            self.x_fresh.store(false, Ordering::Relaxed);
            self.y_fresh.store(false, Ordering::Relaxed);
            let (x, y) = *self.position.lock().await;
            let touching = *self.touching_hw.lock().await;
            self.emit(virt_dev, x, y, touching, None).await;
        }
    }

    /// Returns true if this pad is ready to emit a combined frame:
    /// - it has no new data (!dirty), or
    /// - it has new data with both axes fresh (x_fresh && y_fresh).
    /// Does NOT reset any flags - the caller must reset them after emitting.
    pub fn is_ready(&self) -> bool {
        if self.dirty.load(Ordering::Relaxed) {
            self.x_fresh.load(Ordering::Relaxed) && self.y_fresh.load(Ordering::Relaxed)
        } else {
            true
        }
    }

    /// Reset all coalescing flags. Use after a combined frame has been emitted
    /// (e.g. by the gesture pad emitter) to mark this pad as "consumed".
    pub fn clear_pending(&self) {
        self.dirty.store(false, Ordering::Relaxed);
        self.x_fresh.store(false, Ordering::Relaxed);
        self.y_fresh.store(false, Ordering::Relaxed);
        self.emit_pending.store(false, Ordering::Relaxed);
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
        dev.emit(&events).ok();
    }
}

/// Emit a combined two-finger gesture event to the gesture pad device.
/// `l_touching` / `r_touching` must come from hidraw hardware state.
/// Pass both `false` to force a clean lift on both slots (gesture exit).
pub async fn emit_gesture_event(
    virt_dev: &Arc<Mutex<VirtualDevices>>,
    lx: i32,
    ly: i32,
    rx: i32,
    ry: i32,
    l_touching: bool,
    r_touching: bool,
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

    events.push(InputEvent::new_now(EventType::SYNCHRONIZATION, 0, 0));
    dev.emit(&events).ok();
}
