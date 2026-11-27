use super::*;

const WINDOW: Duration = Duration::from_millis(400);

fn at(start: Instant, ms: u64) -> Instant {
    start + Duration::from_millis(ms)
}

#[tokio::test(start_paused = true)]
async fn two_presses_within_the_window_are_a_double_click() {
    let t0 = Instant::now();
    let mut d = DoubleClick::default();
    assert!(!d.press(t0, WINDOW));
    assert!(d.press(at(t0, 400), WINDOW), "the window is inclusive, like the Gaming Mode trigger");
}

#[tokio::test(start_paused = true)]
async fn two_presses_too_far_apart_are_not() {
    let t0 = Instant::now();
    let mut d = DoubleClick::default();
    assert!(!d.press(t0, WINDOW));
    assert!(!d.press(at(t0, 401), WINDOW));
    // The late press starts the next try.
    assert!(d.press(at(t0, 700), WINDOW));
}

#[tokio::test(start_paused = true)]
async fn a_completed_double_click_is_used_up() {
    let t0 = Instant::now();
    let mut d = DoubleClick::default();
    d.press(t0, WINDOW);
    assert!(d.press(at(t0, 100), WINDOW));
    assert!(!d.press(at(t0, 200), WINDOW), "a third press is a new first click");
}
