use super::*;

const MS: fn(u64) -> Duration = Duration::from_millis;

#[test]
fn the_wait_doubles_and_is_capped() {
    let schedule: Vec<Duration> = (0..10).map(delay).collect();
    assert_eq!(
        schedule,
        vec![
            MS(0), MS(500), MS(1_000), MS(2_000), MS(4_000),
            MS(8_000), MS(16_000), MAX_DELAY, MAX_DELAY, MAX_DELAY,
        ]
    );
}

/// However long the streak, the arithmetic does not overflow.
#[test]
fn a_very_long_streak_stays_at_the_cap() {
    assert_eq!(delay(40), MAX_DELAY);
    assert_eq!(delay(u32::MAX), MAX_DELAY);
}

#[test]
fn the_first_attempt_does_not_wait() {
    let r = Restart::default();
    assert!(r.due(Instant::now()));
    assert_eq!(r.not_before(), None);
}

/// A `pactl` that exits as soon as it starts backs off further each time.
#[test]
fn immediate_exits_back_off() {
    let t0 = Instant::now();
    let mut r = Restart::default();
    let mut now = t0;
    let mut waits = Vec::new();
    for _ in 0..4 {
        assert!(r.due(now));
        r.started(now);
        let wait = r.failed(now);
        assert!(!r.due(now), "restart allowed right after a failure");
        waits.push(wait);
        now += wait;
    }
    assert_eq!(waits, vec![MS(500), MS(1_000), MS(2_000), MS(4_000)]);
}

/// Spawn errors count the same as early exits.
#[test]
fn spawn_errors_back_off_too() {
    let now = Instant::now();
    let mut r = Restart::default();
    assert_eq!(r.failed(now), MS(500));
    assert_eq!(r.failed(now), MS(1_000));
}

/// After a run that lasted, the next failure starts the schedule over.
#[test]
fn a_healthy_run_resets_the_schedule() {
    let t0 = Instant::now();
    let mut r = Restart::default();
    for _ in 0..5 {
        r.failed(t0);
    }
    r.started(t0);
    assert_eq!(r.failed(t0 + HEALTHY_RUN), FIRST_DELAY);
}

/// A run just short of healthy does not reset it.
#[test]
fn a_short_run_does_not_reset_the_schedule() {
    let t0 = Instant::now();
    let mut r = Restart::default();
    r.failed(t0);
    r.started(t0);
    assert_eq!(r.failed(t0 + HEALTHY_RUN - MS(1)), MS(1_000));
}
