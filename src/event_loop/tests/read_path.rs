//! What the read loop makes of a report before any binding is asked: the
//! SYN_REPORT that closes it, a button nothing is bound to, and the axes a Deck
//! reports that are not buttons.

use super::*;

/// A report is closed by a SYN_REPORT, and that SYN must not reach the virtual
/// keyboard: `emit` appends one of its own, so passing it on would split the
/// frame. Two buttons in one report are two bindings, in report order.
#[tokio::test]
async fn two_buttons_in_one_report_are_two_bindings_and_the_syn_is_consumed() {
    let mut deck = Deck::new("one-report");
    let both = vec![
        InputEvent::new(EventType::KEY, Key::BTN_SOUTH.code(), 1),
        InputEvent::new(EventType::KEY, Key::BTN_EAST.code(), 1),
        syn(),
    ];
    let actions = deck.feed(both).await;
    assert_eq!(keys(&actions), vec![(Key::KEY_ENTER, 1), (Key::KEY_ESC, 1)]);
    assert!(!actions.iter().any(|a| matches!(a, OutputAction::Forward { .. })));
}

/// A button with no binding at all goes through untouched, keeping its own
/// code — QAM pressed once is QAM to the desktop.
#[tokio::test]
async fn an_unbound_button_is_forwarded_raw() {
    let mut deck = Deck::new("unbound");
    let down = deck.press(Key::BTN_BASE).await;
    assert!(down.iter().any(|a| matches!(
        a, OutputAction::Forward { device: crate::output::OutputDevice::Keys, .. }
    )));
    let up = deck.release(Key::BTN_BASE).await;
    assert_eq!(keys(&[down, up].concat()), vec![(Key::BTN_BASE, 1), (Key::BTN_BASE, 0)]);
}

/// The right pad is an `mt-trackpad`, so its click is read from hidraw by the
/// trackpad router; the evdev edge is its duplicate and goes nowhere.
#[tokio::test]
async fn a_trackpad_click_is_left_to_the_trackpad() {
    let mut deck = Deck::new("pad-click");
    let click = [deck.press(Key::BTN_THUMB2).await, deck.release(Key::BTN_THUMB2).await].concat();
    assert!(click.is_empty(), "{click:?}");
}

/// The right pad's position arrives as ABS_HAT1X/Y and is the trackpad
/// router's too: nothing reaches an output.
#[tokio::test]
async fn the_right_pad_position_never_reaches_an_output() {
    let mut deck = Deck::new("right-pad");
    let touch = |x: i32| vec![
        InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_HAT1X.0, x),
        syn(),
    ];
    let actions = deck.feed([touch(1), touch(0)].concat()).await;
    assert!(actions.is_empty(), "{actions:?}");
}

/// A stick bound to directions presses a direction on the way out and releases
/// it on the way back, and pushing further in the same direction sends nothing.
#[tokio::test]
async fn a_bound_stick_presses_once_and_releases_on_return() {
    let mut deck = Deck::new("stick-bind");
    let out = deck.feed(lstick_y(-20000)).await;
    assert_eq!(keys(&out), vec![(Key::KEY_UP, 1)]);
    assert!(keys(&deck.feed(lstick_y(-32767)).await).is_empty());
    assert_eq!(keys(&deck.feed(lstick_y(0)).await), vec![(Key::KEY_UP, 0)]);
}

/// On a Deck, ABS_HAT0X/Y is the left trackpad's position. The base config
/// sets the left pad to `disabled`, and the hat-switch decoder in `read_events`
/// only stands aside for a pad with a handler — so a pad coordinate of exactly
/// ±1, and the 0 after it, is read as a D-pad direction. Nothing can bind that
/// direction, so it falls through to passthrough and the raw ABS event leaks
/// onto the virtual ABS device, which the comment on the HAT1 arm says must not
/// happen.
#[tokio::test]
#[ignore = "bug: left-pad coordinates of exactly ±1 are decoded as a D-pad hat and forwarded when the left pad is disabled"]
async fn the_left_pad_position_never_reaches_an_output() {
    let mut deck = Deck::new("left-pad");
    let touch = |x: i32| vec![
        InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_HAT0X.0, x),
        syn(),
    ];
    let actions = deck.feed([touch(1), touch(0)].concat()).await;
    let emitted: Vec<_> = actions
        .iter()
        .filter(|a| matches!(a, OutputAction::Frame { .. } | OutputAction::Forward { .. }))
        .collect();
    assert!(emitted.is_empty(), "left pad position reached an output: {emitted:?}");
}
