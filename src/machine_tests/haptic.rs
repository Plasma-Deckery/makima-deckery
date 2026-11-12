//! `binding_haptic`: what a press plays on the pad.

use super::*;

fn click() -> crate::config::BindingHaptic {
    crate::config::BindingHaptic {
        pad: deckery_controller::HapticPad::Right,
        chain: deckery_controller::HapticChain::Single(Default::default()),
    }
}

/// A press with a haptic plays it, on the side the binding names.
#[test]
fn a_press_plays_the_binding_haptic() {
    let h = click();
    assert_eq!(
        binding_haptic(Some(&h), 1, false, false),
        Some(OutputAction::Haptic { pad: h.pad, chain: h.chain.clone() }),
    );
}

/// Only the press: a repeat or a release is not a click.
#[test]
fn repeat_and_release_play_nothing() {
    let h = click();
    assert_eq!(binding_haptic(Some(&h), 2, false, false), None, "repeat buzzed");
    assert_eq!(binding_haptic(Some(&h), 0, false, false), None, "release buzzed");
}

/// A press swallowed by pause sends nothing, so it must not feel like it did —
/// unless the binding is `no_pause` and really fires.
#[test]
fn pause_silences_it_unless_the_binding_fires_anyway() {
    let h = click();
    assert_eq!(binding_haptic(Some(&h), 1, true, false), None);
    assert!(binding_haptic(Some(&h), 1, true, true).is_some());
}

/// No haptic configured, nothing played.
#[test]
fn no_haptic_plays_nothing() {
    assert_eq!(binding_haptic(None, 1, false, false), None);
}
