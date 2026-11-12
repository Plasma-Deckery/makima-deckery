use super::*;

// ── Trackpad modes ────────────────────────────────────────────────────────────

#[test]
fn the_documented_modes_are_known() {
    for mode in ["disabled", "mt-trackpad", "trackball", "scroll"] {
        assert!(is_known_trackpad_mode(mode), "{mode:?} should be a known mode");
    }
}

#[test]
fn a_misspelled_mode_is_not_known() {
    // Matching is exact on purpose: `MT-Trackpad` or `mt_trackpad` get the
    // warning, so a typo is reported instead of silently switching the pad off.
    for mode in ["", "mt_trackpad", "MT-Trackpad", "trackpad", "scroll "] {
        assert!(!is_known_trackpad_mode(mode), "{mode:?} should not be a known mode");
    }
}

// ── Click pressure ────────────────────────────────────────────────────────────

#[test]
fn no_click_pressure_configured_sends_nothing() {
    // No feature report at all — the firmware keeps whatever it had, which is
    // the point of only sending when the user asked for something.
    assert_eq!(click_pressure_config(None, None), None);
}

#[test]
fn both_sides_configured_are_sent_as_given() {
    assert_eq!(
        click_pressure_config(Some(100), Some(200)),
        Some(ClickPressureConfig { left: 100, right: 200 }),
    );
}

#[test]
fn an_unconfigured_side_is_sent_as_0xffff() {
    // Pinned as it behaves today. `ClickPressureConfig` documents 0xFFFF as
    // "effectively disables physical clicks", so configuring one pad also
    // changes the other one — see the report on issue #42.
    assert_eq!(
        click_pressure_config(Some(100), None),
        Some(ClickPressureConfig { left: 100, right: 0xFFFF }),
    );
    assert_eq!(
        click_pressure_config(None, Some(200)),
        Some(ClickPressureConfig { left: 0xFFFF, right: 200 }),
    );
}
