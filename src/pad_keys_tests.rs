use super::*;

#[test]
fn every_modes_keys_are_pad_keys() {
    for key in ["mode", "click_pressure", "kde", "haptic", "pixel_interval", "sensitivity", "grid"] {
        assert!(is_pad_key(key), "{key}");
    }
    for key in ["on_press", "on_release", "on_movement", "on_roll"] {
        assert!(is_haptic_key(key), "{key}");
    }
}

#[test]
fn a_typo_is_no_pad_key() {
    assert!(!is_pad_key("friciton"));
    assert!(!is_haptic_key("on_rol"));
}
