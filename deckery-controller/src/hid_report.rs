//! Synthesises a full `ControllerEvent::Input` stream from raw 64-byte
//! hid-steam HID reports.
//!
//! On Linux ≥ 6.12 / kernel 7.1, opening hidraw causes hid-steam to
//! intentionally remove the evdev device (commit cd33a91 by Vicki Pfau /
//! Valve) to prevent concurrent FEATURE-report conflicts on the USB
//! endpoint. This module reproduces exactly what that evdev device would
//! have emitted, so the rest of makima can stay unchanged.
//!
//! Button and axis maps were cross-checked against the upstream hid-steam
//! kernel driver (`steam_deck_button_mappings` / `steam_deck_axis_mappings` /
//! `steam_do_deck_input_event`). Byte offsets were determined empirically
//! (2026-07-08) by recording evdev and hidraw simultaneously with a shared
//! monotonic clock and correlating ABS_HAT0/1X/Y against every int16 offset
//! in the raw report.

use evdev::{AbsoluteAxisType, EventType, InputEvent, Key};

/// Button mapping: (evdev key code, byte index into 64-byte report, bit mask).
///
/// Mirrors `steam_deck_button_mappings[]` in the upstream hid-steam kernel
/// driver. Byte and bit indices are **into the raw 64-byte hidraw read buffer**
/// (i.e. the same buffer that `PadFrame::parse` reads).
static BUTTON_MAP: &[(u16, usize, u8)] = &[
    (Key::BTN_TR2.code(),        8, 1 << 0),  // right bumper 2 (SR)
    (Key::BTN_TL2.code(),        8, 1 << 1),  // left  bumper 2 (SL)
    (Key::BTN_TR.code(),         8, 1 << 2),  // right bumper (R1)
    (Key::BTN_TL.code(),         8, 1 << 3),  // left  bumper (L1)
    // Face buttons: hid-steam uses BTN_Y/B/X/A — evdev crate aliases:
    //   BTN_A = BTN_SOUTH = 0x130, BTN_B = BTN_EAST = 0x131,
    //   BTN_X = BTN_NORTH = 0x133, BTN_Y = BTN_WEST = 0x134
    (Key::BTN_WEST.code(),       8, 1 << 4),  // Y button
    (Key::BTN_EAST.code(),       8, 1 << 5),  // B button
    (Key::BTN_NORTH.code(),      8, 1 << 6),  // X button
    (Key::BTN_SOUTH.code(),      8, 1 << 7),  // A button
    (Key::BTN_DPAD_UP.code(),    9, 1 << 0),
    (Key::BTN_DPAD_RIGHT.code(), 9, 1 << 1),
    (Key::BTN_DPAD_LEFT.code(),  9, 1 << 2),
    (Key::BTN_DPAD_DOWN.code(),  9, 1 << 3),
    (Key::BTN_SELECT.code(),     9, 1 << 4),  // View / Back
    (Key::BTN_MODE.code(),       9, 1 << 5),  // Steam logo
    (Key::BTN_START.code(),      9, 1 << 6),  // Menu / Start
    (Key::BTN_GRIPL2.code(),     9, 1 << 7),  // L5 back paddle (bottom-left)
    (Key::BTN_GRIPR2.code(),    10, 1 << 0),  // R5 back paddle (bottom-right)
    (Key::BTN_THUMB.code(),     10, 1 << 1),  // left  pad click (L2-pad-press)
    (Key::BTN_THUMB2.code(),    10, 1 << 2),  // right pad click (R2-pad-press)
    (Key::BTN_THUMBL.code(),    10, 1 << 6),  // left  stick click (L3)
    (Key::BTN_THUMBR.code(),    11, 1 << 2),  // right stick click (R3)
    (Key::BTN_GRIPL.code(),     13, 1 << 1),  // L4 back paddle (top-left)
    (Key::BTN_GRIPR.code(),     13, 1 << 2),  // R4 back paddle (top-right)
    (Key::BTN_BASE.code(),      14, 1 << 2),  // QAM / … button
];

/// Axis mapping: (evdev absolute axis code, byte offset, negate).
///
/// Mirrors `steam_deck_axis_mappings[]` and the pad-reading in
/// `steam_do_deck_input_event()` from the upstream hid-steam kernel driver.
/// `negate = true` matches hid-steam's `sign = -1` (kernel stores the raw
/// value with the opposite polarity for ABS_Y / ABS_RY so that "up" maps to
/// positive in evdev convention).
static AXIS_MAP: &[(u16, usize, bool)] = &[
    (AbsoluteAxisType::ABS_HAT0X.0,  16, false), // left  pad X
    (AbsoluteAxisType::ABS_HAT0Y.0,  18, false), // left  pad Y
    (AbsoluteAxisType::ABS_HAT1X.0,  20, false), // right pad X
    (AbsoluteAxisType::ABS_HAT1Y.0,  22, false), // right pad Y
    (AbsoluteAxisType::ABS_HAT2Y.0,  44, false), // left  trigger (L2 analog)
    (AbsoluteAxisType::ABS_HAT2X.0,  46, false), // right trigger (R2 analog)
    (AbsoluteAxisType::ABS_X.0,      48, false), // left  stick X
    (AbsoluteAxisType::ABS_Y.0,      50, true),  // left  stick Y (negated)
    (AbsoluteAxisType::ABS_RX.0,     52, false), // right stick X
    (AbsoluteAxisType::ABS_RY.0,     54, true),  // right stick Y (negated)
];

/// Synthesise the `InputEvent`s that the hid-steam evdev device would emit for
/// `buf`, given the previous buffer `prev`. Only changed values are emitted,
/// followed by `SYN_REPORT`. Returns an empty `Vec` when nothing changed.
///
/// This produces a stream that is semantically identical to what
/// `steam_do_deck_input_event` / `steam_map_buttons` / `steam_map_axes` write
/// to the kernel evdev ring buffer, allowing the rest of makima to remain
/// unchanged even on kernels where hid-steam removes the evdev node when
/// hidraw is opened.
pub(super) fn synthesise_input_events(buf: &[u8; 64], prev: &[u8; 64]) -> Vec<InputEvent> {
    let mut events: Vec<InputEvent> = Vec::new();

    // Buttons — emit key event only on edge (0→1 or 1→0).
    for &(code, byte, mask) in BUTTON_MAP {
        let was = (prev[byte] & mask) != 0;
        let now = (buf[byte]  & mask) != 0;
        if was != now {
            events.push(InputEvent::new_now(EventType::KEY, code, now as i32));
        }
    }

    // Axes — emit abs event whenever the i16 value changed.
    for &(code, offset, negate) in AXIS_MAP {
        let prev_val = i16::from_le_bytes([prev[offset], prev[offset + 1]]) as i32;
        let curr_val = i16::from_le_bytes([buf[offset],  buf[offset  + 1]]) as i32;
        if prev_val != curr_val {
            let reported = if negate { -curr_val } else { curr_val };
            events.push(InputEvent::new_now(EventType::ABSOLUTE, code, reported));
        }
    }

    // Emit SYN_REPORT only when there is something to sync.
    if !events.is_empty() {
        events.push(InputEvent::new_now(EventType::SYNCHRONIZATION, 0, 0));
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zero_buf() -> [u8; 64] { [0u8; 64] }

    /// Build a buf with a single button pressed (byte, bit).
    fn btn_buf(byte: usize, mask: u8) -> [u8; 64] {
        let mut buf = zero_buf();
        buf[byte] = mask;
        buf
    }

    /// Build a buf with an i16 axis value at the given byte offset.
    fn axis_buf(offset: usize, val: i16) -> [u8; 64] {
        let mut buf = zero_buf();
        buf[offset..offset + 2].copy_from_slice(&val.to_le_bytes());
        buf
    }

    #[test]
    fn synthesise_emits_nothing_when_buffer_unchanged() {
        let buf = zero_buf();
        let events = synthesise_input_events(&buf, &buf);
        assert!(events.is_empty(), "no events expected when buffer is identical");
    }

    #[test]
    fn synthesise_emits_btn_south_on_a_press() {
        let prev = zero_buf();
        let curr = btn_buf(8, 1 << 7); // BTN_SOUTH / A, byte 8, bit 7
        let events = synthesise_input_events(&curr, &prev);
        // Expect at least one KEY event with value 1, plus SYN_REPORT.
        let key_ev = events.iter().find(|e| {
            e.event_type() == EventType::KEY
                && e.code() == Key::BTN_SOUTH.code()
                && e.value() == 1
        });
        assert!(key_ev.is_some(), "expected BTN_SOUTH press event, got: {:?}", events);
        assert!(
            events.last().map(|e| e.event_type() == EventType::SYNCHRONIZATION).unwrap_or(false),
            "last event must be SYN_REPORT"
        );
    }

    #[test]
    fn synthesise_emits_btn_release_on_transition_1_to_0() {
        let prev = btn_buf(8, 1 << 7); // A pressed
        let curr = zero_buf();          // A released
        let events = synthesise_input_events(&curr, &prev);
        let key_ev = events.iter().find(|e| {
            e.event_type() == EventType::KEY
                && e.code() == Key::BTN_SOUTH.code()
                && e.value() == 0
        });
        assert!(key_ev.is_some(), "expected BTN_SOUTH release event, got: {:?}", events);
    }

    #[test]
    fn synthesise_emits_abs_x_on_left_stick_move() {
        let prev = zero_buf();
        let curr = axis_buf(48, 10_000i16); // ABS_X = left stick X, offset 48
        let events = synthesise_input_events(&curr, &prev);
        let abs_ev = events.iter().find(|e| {
            e.event_type() == EventType::ABSOLUTE
                && e.code() == AbsoluteAxisType::ABS_X.0
                && e.value() == 10_000
        });
        assert!(abs_ev.is_some(), "expected ABS_X = 10000, got: {:?}", events);
    }

    #[test]
    fn synthesise_negates_abs_y_for_left_stick() {
        // ABS_Y (left stick Y, offset 50) has negate=true: raw +5000 → reported -5000.
        let prev = zero_buf();
        let curr = axis_buf(50, 5_000i16);
        let events = synthesise_input_events(&curr, &prev);
        let abs_ev = events.iter().find(|e| {
            e.event_type() == EventType::ABSOLUTE
                && e.code() == AbsoluteAxisType::ABS_Y.0
        });
        assert!(
            abs_ev.is_some() && abs_ev.unwrap().value() == -5_000,
            "expected ABS_Y = -5000 (negated), got: {:?}", events
        );
    }

    #[test]
    fn synthesise_does_not_negate_trigger_axes() {
        // ABS_HAT2Y (left trigger, offset 44) has negate=false.
        let prev = zero_buf();
        let curr = axis_buf(44, 20_000i16);
        let events = synthesise_input_events(&curr, &prev);
        let abs_ev = events.iter().find(|e| {
            e.event_type() == EventType::ABSOLUTE
                && e.code() == AbsoluteAxisType::ABS_HAT2Y.0
        });
        assert!(
            abs_ev.is_some() && abs_ev.unwrap().value() == 20_000,
            "expected ABS_HAT2Y = 20000 (not negated), got: {:?}", events
        );
    }

    #[test]
    fn synthesise_emits_only_changed_fields() {
        // Change only ABS_RX (right stick X, offset 52). Nothing else should appear.
        let prev = zero_buf();
        let mut curr = zero_buf();
        curr[52..54].copy_from_slice(&(-3000i16).to_le_bytes());
        let events = synthesise_input_events(&curr, &prev);
        // Only ABS_RX + SYN_REPORT expected.
        let non_syn: Vec<_> = events.iter()
            .filter(|e| e.event_type() != EventType::SYNCHRONIZATION)
            .collect();
        assert_eq!(non_syn.len(), 1, "expected exactly one non-SYN event, got: {:?}", events);
        assert_eq!(non_syn[0].code(), AbsoluteAxisType::ABS_RX.0);
        assert_eq!(non_syn[0].value(), -3000);
    }

    #[test]
    fn synthesise_multiple_buttons_in_one_report() {
        // Press BTN_SOUTH (byte 8, bit 7) and BTN_DPAD_UP (byte 9, bit 0) simultaneously.
        let prev = zero_buf();
        let mut curr = zero_buf();
        curr[8] = 1 << 7;
        curr[9] = 1 << 0;
        let events = synthesise_input_events(&curr, &prev);
        let south = events.iter().any(|e| e.event_type() == EventType::KEY && e.code() == Key::BTN_SOUTH.code() && e.value() == 1);
        let dpad  = events.iter().any(|e| e.event_type() == EventType::KEY && e.code() == Key::BTN_DPAD_UP.code() && e.value() == 1);
        assert!(south, "BTN_SOUTH missing from: {:?}", events);
        assert!(dpad,  "BTN_DPAD_UP missing from: {:?}", events);
    }
}
