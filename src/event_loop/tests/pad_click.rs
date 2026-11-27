//! A pad's `click` switch, from the override layer to the handler.
//!
//! The layer sets `trackpad.right.click` or `trackpad.gestures.click`; that is
//! a device section, so the session asks for a rebuild, and the rebuilt
//! device session hands the flag from `registry.base_configs()` to the
//! handlers. The harness has no device to rebuild, so the last step is taken
//! here by hand: the handler is run with the flag the rebuild would read.

use super::*;
use crate::output::action::Emission;
use crate::trackpad_router::{CombinedPadFrame, SinglePadFrame};

/// The `BTN_LEFT` values among `actions`, on whatever device.
fn left_button(actions: &[OutputAction]) -> Vec<i32> {
    actions.iter().flat_map(|a| match a {
        OutputAction::Frame { events, .. } => events.iter()
            .filter(|e| e.ev_type == EventType::KEY.0 && e.code == Key::BTN_LEFT.code())
            .map(|e| e.value).collect::<Vec<_>>(),
        _ => vec![],
    }).collect()
}

fn pointer_moved(actions: &[OutputAction]) -> bool {
    actions.iter().any(|a| match a {
        OutputAction::Frame { events, .. } => events.iter().any(|e|
            (e.ev_type == EventType::ABSOLUTE.0 && e.code == AbsoluteAxisType::ABS_MT_POSITION_X.0)
            || (e.ev_type == EventType::RELATIVE.0 && e.code == RelativeAxisType::REL_X.0)),
        _ => false,
    })
}

fn haptics(actions: &[OutputAction]) -> usize {
    actions.iter().filter(|a| matches!(a, OutputAction::Haptic { .. })).count()
}

fn pad(x: i32, touching: bool, click: bool) -> SinglePadFrame {
    SinglePadFrame { x, y: 0, touching, click, interrupted: false }
}

/// Drag, press, release, lift — on the right pad, through the handler of
/// `mode` with the given click switch. Returns what it sent.
async fn press_the_pad(mode: &str, click: bool) -> Vec<OutputAction> {
    let (tap_tx, mut tap_rx) = mpsc::channel::<Emission>(4096);
    let outputs = crate::output::Outputs { tap: Some(Tap::new(tap_tx)), ..Default::default() };
    let (tx, rx) = mpsc::channel(16);
    let frames = async move {
        for frame in [pad(0, true, false), pad(4000, true, false), pad(8000, true, false),
                      pad(8000, true, true), pad(8000, true, false), pad(8000, false, false)] {
            tx.send(frame).await.unwrap();
        }
    };
    let state = crate::trackpad::PadState::new(false);
    match mode {
        "mt-trackpad" => {
            let press = Some(deckery_controller::HapticChain::default());
            let handler = crate::mt_trackpad::run_single(
                rx, &outputs, &state, HapticPad::Right, press.clone(), press, None, click);
            tokio::join!(handler, frames);
        }
        "trackball" => {
            let config = crate::trackball::TrackballConfig::default();
            tokio::join!(crate::trackball::run_single(rx, &outputs, HapticPad::Right, config, click), frames);
        }
        _ => unreachable!(),
    }
    let mut actions = Vec::new();
    while let Ok(e) = tap_rx.try_recv() {
        actions.push(e.action);
    }
    actions
}

/// With the click off the pad still moves the pointer, and a press sends no
/// `BTN_LEFT` down and plays no click haptic — in either mode. With it on it
/// does both, as before.
#[tokio::test(start_paused = true)]
async fn the_click_switch_gates_only_the_click() {
    for mode in ["mt-trackpad", "trackball"] {
        let off = press_the_pad(mode, false).await;
        assert!(pointer_moved(&off), "{mode}: the pad stopped moving the pointer");
        assert!(!left_button(&off).contains(&1), "{mode}: a press clicked with the click off");
        assert_eq!(haptics(&off), 0, "{mode}: a click haptic played with the click off");

        let on = press_the_pad(mode, true).await;
        assert!(left_button(&on).contains(&1), "{mode}: no click with the click on");
        assert!(haptics(&on) > 0, "{mode}");
    }
}

/// The gesture device keeps both slots — pinch, scroll, pan — and only drops
/// the button.
#[test]
fn the_gesture_click_switch_keeps_the_slots() {
    let frame = CombinedPadFrame {
        lx: -9000, ly: 0, ltouch: true, lclick: true,
        rx: 9000, ry: 0, rtouch: true, rclick: false,
    };
    let off = crate::gesture_pad::frame_actions(frame, false);
    assert!(!left_button(&off).contains(&1));
    let slots = match &off[0] {
        OutputAction::Frame { events, .. } => events.iter()
            .filter(|e| e.code == AbsoluteAxisType::ABS_MT_POSITION_X.0).count(),
        _ => 0,
    };
    assert_eq!(slots, 2, "both fingers are still there to pinch with");
    assert_eq!(left_button(&crate::gesture_pad::frame_actions(frame, true)), vec![1]);
}

/// The right pad's click and the gestures' click, as the rebuilt device
/// session would read them.
fn clicks(deck: &Deck) -> (bool, bool) {
    let base = deck.reader.registry.base_configs().remove(0);
    let gestures = crate::gesture_pad::GesturePadConfig::parse(&base.trackpad.gesture_handler_config).0;
    (base.trackpad.right.click, gestures.click)
}

/// The tour's sequence: on by default, off for the text stages, on again
/// after them — in one session, each step a rebuild, and the pad clicks
/// again at the end.
#[tokio::test(start_paused = true)]
async fn the_tour_switches_the_click_off_and_on_again() {
    let mut deck = Deck::new("override-click");
    deck.override_cmd("override begin").await;
    assert_eq!(clicks(&deck), (true, true), "the default is on");

    for (value, expected) in [("true", true), ("false", false), ("true", true)] {
        assert_eq!(deck.override_cmd(&format!("override set trackpad.right.click {value}")).await.0, "ok");
        assert_eq!(deck.override_cmd(&format!("override set trackpad.gestures.click {value}")).await.0, "ok");
        assert_eq!(clicks(&deck), (expected, expected), "after click = {value}");
        let pressed = press_the_pad(&deck.reader.registry.base_configs()[0].trackpad.right.mode,
            clicks(&deck).0).await;
        assert_eq!(left_button(&pressed).contains(&1), expected, "after click = {value}");
    }
    // Every set changes the trackpad section of the layer, so every one asks
    // for a rebuild — even the first `true`, which writes the default.
    assert_eq!(deck.device_log, vec![DeviceRequest::Relaunch; 6]);

    let (reply, _) = deck.override_cmd("override set trackpad.right.click 0").await;
    assert_eq!(reply, "error trackpad.right.click: must be true or false");
    let (reply, _) = deck.override_cmd(r#"override set trackpad.gestures.click "no""#).await;
    assert!(reply.starts_with("error trackpad.gestures: click"), "{reply}");
}
