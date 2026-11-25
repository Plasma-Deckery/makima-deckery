use super::*;

#[test]
fn the_combo_is_held_once_both_are_down() {
    let mut t = ComboTracker::default();
    assert_eq!(t.update(Key::BTN_MODE, 1), None);
    assert_eq!(t.update(Key::BTN_BASE, 1), Some(true));
    assert!(t.all_held());
    assert_eq!(t.update(Key::BTN_MODE, 0), Some(false));
    assert_eq!(t.update(Key::BTN_BASE, 0), None);
}

#[test]
fn the_order_does_not_matter() {
    let mut t = ComboTracker::default();
    t.update(Key::BTN_BASE, 1);
    assert_eq!(t.update(Key::BTN_MODE, 1), Some(true));
}

#[test]
fn repeats_and_other_keys_change_nothing() {
    let mut t = ComboTracker::default();
    t.update(Key::BTN_MODE, 1);
    assert_eq!(t.update(Key::BTN_BASE, 2), None);
    assert_eq!(t.update(Key::BTN_SOUTH, 1), None);
    assert_eq!(t.update(Key::BTN_BASE, 1), Some(true));
    assert_eq!(t.update(Key::BTN_SOUTH, 0), None);
    assert!(t.all_held());
}
