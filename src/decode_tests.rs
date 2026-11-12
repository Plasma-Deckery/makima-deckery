use super::*;

use std::time::Duration;

/// `key_event` for pads in these modes, through the same `has_handler` the
/// reader asks.
fn key_event_with(key: Key, left_mode: &str, right_mode: &str) -> Option<Event> {
    let pad = |mode: &str| crate::config::TrackpadSideConfig {
        mode: mode.to_string(),
        ..Default::default()
    };
    key_event(key, pad(left_mode).has_handler(), pad(right_mode).has_handler())
}

// ── Keys ──────────────────────────────────────────────────────────────────────

/// Issue #44: the D-pad never produced output because the config parsed
/// `BTN_DPAD_*` as axes while the reader sent keys. Pinned from both sides —
/// what the reader decodes a D-pad edge to has to be exactly what a config
/// entry with the same name resolves to, or the binding matches nothing.
#[test]
fn a_dpad_edge_is_the_same_key_event_a_config_names() {
    for (key, name) in [
        (Key::BTN_DPAD_UP, "BTN_DPAD_UP"),
        (Key::BTN_DPAD_DOWN, "BTN_DPAD_DOWN"),
        (Key::BTN_DPAD_LEFT, "BTN_DPAD_LEFT"),
        (Key::BTN_DPAD_RIGHT, "BTN_DPAD_RIGHT"),
    ] {
        let decoded = key_event_with(key, "disabled", "mt-trackpad");
        assert_eq!(decoded, Some(Event::Key(key)), "{name} must decode as a key");
        assert_eq!(
            crate::config::event_from_name(name),
            decoded,
            "{name} in a config must resolve to what the reader sends",
        );
    }
}

/// An ordinary button is passed on unchanged, whatever the pads are doing.
#[test]
fn an_ordinary_button_is_its_own_key() {
    assert_eq!(
        key_event_with(Key::BTN_SOUTH, "mt-trackpad", "mt-trackpad"),
        Some(Event::Key(Key::BTN_SOUTH)),
    );
}

/// A pad in `mt-trackpad` or `trackball` mode has its click read from hidraw,
/// so the key edge is swallowed — otherwise every click would arrive twice.
#[test]
fn a_pad_click_is_swallowed_when_a_handler_owns_its_pad() {
    assert_eq!(key_event_with(Key::BTN_THUMB, "mt-trackpad", "disabled"), None);
    assert_eq!(key_event_with(Key::BTN_THUMB2, "disabled", "mt-trackpad"), None);
    assert_eq!(key_event_with(Key::BTN_THUMB, "trackball", "disabled"), None);
    assert_eq!(key_event_with(Key::BTN_THUMB2, "disabled", "trackball"), None);
}

/// Without a handler nobody else reads the click, so it is a button like any
/// other and can be bound.
#[test]
fn a_pad_click_is_a_button_when_no_handler_owns_its_pad() {
    assert_eq!(
        key_event_with(Key::BTN_THUMB, "disabled", "mt-trackpad"),
        Some(Event::Key(Key::BTN_THUMB)),
    );
    assert_eq!(
        key_event_with(Key::BTN_THUMB2, "trackball", "scroll"),
        Some(Event::Key(Key::BTN_THUMB2)),
    );
}

/// Each pad's mode governs its own click only. This is the shipped layout —
/// right pad a trackpad, left pad disabled — and getting the sides crossed
/// would swallow the wrong click.
#[test]
fn each_pad_mode_governs_only_its_own_click() {
    assert_eq!(
        key_event_with(Key::BTN_THUMB, "disabled", "mt-trackpad"),
        Some(Event::Key(Key::BTN_THUMB)),
        "the left click must not follow the right pad's mode",
    );
    assert_eq!(key_event_with(Key::BTN_THUMB2, "disabled", "mt-trackpad"), None);
}

// ── Hat ───────────────────────────────────────────────────────────────────────

/// Each axis names its own two directions, with negative being left and up as
/// evdev counts them.
#[test]
fn a_hat_press_names_its_direction() {
    assert_eq!(decode_hat(true, -1, 0), (vec![(Axis::BTN_DPAD_LEFT, 1)], -1));
    assert_eq!(decode_hat(true, 1, 0), (vec![(Axis::BTN_DPAD_RIGHT, 1)], 1));
    assert_eq!(decode_hat(false, -1, 0), (vec![(Axis::BTN_DPAD_UP, 1)], -1));
    assert_eq!(decode_hat(false, 1, 0), (vec![(Axis::BTN_DPAD_DOWN, 1)], 1));
}

/// Returning to centre releases the direction that was held, and only that
/// one — 0 carries no direction of its own.
#[test]
fn a_hat_returning_to_centre_releases_what_was_held() {
    assert_eq!(decode_hat(true, 0, -1), (vec![(Axis::BTN_DPAD_LEFT, 0)], 0));
    assert_eq!(decode_hat(true, 0, 1), (vec![(Axis::BTN_DPAD_RIGHT, 0)], 0));
    assert_eq!(decode_hat(false, 0, -1), (vec![(Axis::BTN_DPAD_UP, 0)], 0));
    assert_eq!(decode_hat(false, 0, 1), (vec![(Axis::BTN_DPAD_DOWN, 0)], 0));
}

/// A centre with nothing held releases nothing; a release for a key that was
/// never pressed would reach the output layer as a stray key-up.
#[test]
fn a_hat_at_rest_sends_nothing() {
    assert_eq!(decode_hat(true, 0, 0), (vec![], 0));
}

/// Anything other than -1, 0 or 1 is not a hat value and leaves the held
/// direction alone.
#[test]
fn a_value_that_is_not_a_hat_value_changes_nothing() {
    assert_eq!(decode_hat(true, 12000, 1), (vec![], 1));
    assert_eq!(decode_hat(false, -32767, -1), (vec![], -1));
    assert_eq!(decode_hat(true, 2, 0), (vec![], 0));
}

/// Going straight from one direction to the other without passing centre
/// releases the old one before pressing the new one, so it cannot stay held.
#[test]
fn reversing_a_hat_releases_the_old_direction() {
    let (events, held) = decode_hat(true, 1, -1);
    assert_eq!(held, 1);
    assert_eq!(
        events,
        vec![(Axis::BTN_DPAD_LEFT, 0), (Axis::BTN_DPAD_RIGHT, 1)],
    );
}

// ── Sticks ────────────────────────────────────────────────────────────────────

/// The four stick codes split at ABS_Z: 0 and 1 are the left stick, 3 and 4
/// the right one, and the lower code of each pair is X.
#[test]
fn each_stick_code_names_its_stick_and_axis() {
    use evdev::AbsoluteAxisType as A;
    assert_eq!(stick_for_code(A::ABS_X.0), (Stick::Left, true));
    assert_eq!(stick_for_code(A::ABS_Y.0), (Stick::Left, false));
    assert_eq!(stick_for_code(A::ABS_RX.0), (Stick::Right, true));
    assert_eq!(stick_for_code(A::ABS_RY.0), (Stick::Right, false));
}

/// The shipped base config: `RSTICK = "cursor"`, 16-bit axes, deadzone 15.
/// Outside the deadzone the scaled value lands at the axis' own index for the
/// cursor loop; inside it the stored value is 0, which is what stops the
/// cursor when the stick is let go.
#[test]
fn a_cursor_stick_stores_its_scaled_value_at_its_axis() {
    assert_eq!(
        stick_update("cursor", Stick::Right, true, 20000, 15, true, (0, 0)),
        StickUpdate::Position { index: 0, value: axis_value(20000, 15, true) },
    );
    assert_eq!(
        stick_update("cursor", Stick::Right, false, 20000, 15, true, (0, 0)),
        StickUpdate::Position { index: 1, value: axis_value(20000, 15, true) },
    );
    assert_eq!(
        stick_update("cursor", Stick::Right, true, 2500, 15, true, (0, 0)),
        StickUpdate::Position { index: 0, value: 0 },
        "inside the deadzone the position is cleared, not left as it was",
    );
}

/// Scroll uses the stick the same way as cursor; only the loop reading the
/// position differs.
#[test]
fn a_scroll_stick_is_decoded_like_a_cursor_stick() {
    assert_eq!(
        stick_update("scroll", Stick::Left, false, -20000, 15, true, (0, 0)),
        stick_update("cursor", Stick::Left, false, -20000, 15, true, (0, 0)),
    );
}

/// A bound stick sends a direction once on the way out of the deadzone and
/// releases it on the way back, carrying the held state between the two.
#[test]
fn a_bound_stick_presses_and_releases_a_direction() {
    let StickUpdate::Bind { events, held } =
        stick_update("bind", Stick::Left, false, -20000, 15, true, (0, 0))
    else {
        panic!("a bound stick must decode as a binding");
    };
    assert_eq!(events, vec![(Axis::LSTICK_UP, 1)]);
    assert_eq!(held, (0, -1));

    let back = stick_update("bind", Stick::Left, false, 1000, 15, true, held);
    assert_eq!(
        back,
        StickUpdate::Bind { events: vec![(Axis::LSTICK_UP, 0)], held: (0, 0) },
    );
}

/// The left stick is disabled in the shipped config, and an unknown function
/// is treated the same way: the stick drives nothing.
#[test]
fn a_disabled_stick_drives_nothing() {
    assert_eq!(
        stick_update("disabled", Stick::Left, true, 32767, 15, true, (0, 0)),
        StickUpdate::Ignore,
    );
    assert_eq!(
        stick_update("cusror", Stick::Left, true, 32767, 15, true, (0, 0)),
        StickUpdate::Ignore,
    );
}

/// The same distance from centre gives the same speed in both directions.
/// `axis_value` once added 1999 before dividing, a ceiling only for positive
/// values: integer division truncates towards zero, so full deflection gave 17
/// one way and -15 the other, and just past the deadzone negative values were 0.
#[test]
fn a_stick_is_as_fast_in_one_direction_as_in_the_other() {
    for raw in [3500, 10000, 32767] {
        assert_eq!(
            axis_value(-raw, 15, true),
            -axis_value(raw, 15, true),
            "raw ±{raw}",
        );
    }
}

// ── State write gate ──────────────────────────────────────────────────────────

/// A SYN with no analog change never writes, however long it has been.
#[test]
fn an_unchanged_frame_never_writes_analog_state() {
    assert!(!analog_write_due(false, Duration::from_secs(10)));
}

/// A change writes once the interval has passed, and not before — the edge is
/// inclusive, so a steady 16 ms cadence writes on every frame.
#[test]
fn a_change_writes_at_most_every_sixteen_milliseconds() {
    assert!(!analog_write_due(true, Duration::from_millis(15)));
    assert!(analog_write_due(true, Duration::from_millis(16)));
    assert!(analog_write_due(true, Duration::from_millis(200)));
}
