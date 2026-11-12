//! Pause and resume, one decision at a time (issue #62).
//!
//! The sequences these add up to — press, pause, resume, combo, release — run
//! against the real reader in `event_loop/tests/pause.rs`.

use super::*;
use crate::ledger::Ledger;

const L1: Key = Key::BTN_TL;
const STEAM: Key = Key::BTN_MODE;
const X: Key = Key::BTN_NORTH;
const L4: Key = Key::BTN_TRIGGER_HAPPY1;
const L2: Key = Key::BTN_TL2;

/// The shape of the shipped base config, reduced to what these tests need: L1
/// is a layer key (`L1 = []` plus `L1-X`), Steam a modifier with no binding of
/// its own (`STEAM-X`), L4 a remap to Shift, L2 the right mouse button with
/// `no_pause`.
fn layered_config() -> Config {
    let mut config = make_config(
        vec![
            (ev(L1), vec![], vec![]),
            (ev(X), vec![], vec![Key::KEY_X]),
            (ev(X), vec![ev(L1)], vec![Key::KEY_LEFTCTRL, Key::KEY_C]),
            (ev(X), vec![ev(STEAM)], vec![Key::KEY_F5]),
            (ev(L4), vec![], vec![Key::KEY_LEFTSHIFT]),
            (ev(L2), vec![], vec![Key::BTN_RIGHT]),
        ],
        vec![ev(L1), ev(STEAM)],
    );
    config.bindings.no_pause.insert((ev(L2), vec![]));
    config
}

/// A pause with no custom modifier held leaves the flag as it was: it says
/// something about a held modifier, and there is none.
#[test]
fn a_pause_without_a_held_modifier_leaves_the_flag_alone() {
    let config = layered_config();
    let plan = plan_pause(&mut Ledger::new(), &[], &config.mapped_modifiers.custom, false);
    assert!(!plan.modifier_was_activated);
    assert!(plan.frames.is_empty());
}

/// The paused branch for unbound events, now decided in the machine: a
/// modifier is tracked, anything else only shown, and `no_pause` exempts it —
/// for the exact combo or the bare button.
#[test]
fn paused_nonmapped_tracks_modifiers_and_honours_no_pause() {
    let mut config = layered_config();
    assert_eq!(paused_nonmapped(&config, ev(STEAM), &[], false), None, "not paused");
    assert_eq!(paused_nonmapped(&config, ev(STEAM), &[], true), Some(true));
    assert_eq!(paused_nonmapped(&config, ev(Key::BTN_SOUTH), &[], true), Some(false));

    config.bindings.no_pause.insert((ev(Key::BTN_SOUTH), vec![ev(L1)]));
    assert_eq!(paused_nonmapped(&config, ev(Key::BTN_SOUTH), &[ev(L1)], true), None);
    assert_eq!(paused_nonmapped(&config, ev(Key::BTN_SOUTH), &[], true), Some(false));
    config.bindings.no_pause.insert((ev(Key::BTN_SOUTH), vec![]));
    assert_eq!(paused_nonmapped(&config, ev(Key::BTN_SOUTH), &[ev(L4)], true), None);
}

/// Resume with no custom modifier held leaves the flag as it was.
#[test]
fn a_resume_without_a_held_modifier_leaves_the_flag_alone() {
    let config = layered_config();
    let custom = &config.mapped_modifiers.custom;
    assert!(!resume_modifier_was_activated(&[], custom, false));
    assert!(resume_modifier_was_activated(&[ev(STEAM)], custom, false));
}
