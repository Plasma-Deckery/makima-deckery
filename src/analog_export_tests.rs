use super::*;

#[test]
fn it_starts_off_and_is_shared_by_every_clone() {
    let a = AnalogExport::default();
    let b = a.clone();
    assert!(!b.is_on());
    a.set(true, "test");
    assert!(b.is_on());
    b.set(false, "test");
    assert!(!a.is_on());
}
