//! When to start `pactl subscribe` again after it went away.
//!
//! Without a pause, a `pactl` that exits at once — no PipeWire yet at login,
//! a broken pulse shim — is restarted as fast as it can be spawned, which is a
//! busy loop of process creation. Each failure in a row doubles the wait, up to
//! [`MAX_DELAY`]; a run that lasted [`HEALTHY_RUN`] counts as having worked,
//! so the next failure starts the schedule over.
//!
//! Pure: the clock is passed in, so the schedule is testable without waiting.

use tokio::time::{Duration, Instant};

/// The wait after the first failure.
pub const FIRST_DELAY: Duration = Duration::from_millis(500);
/// The longest wait between two attempts.
pub const MAX_DELAY: Duration = Duration::from_secs(30);
/// A subscription that lived this long was healthy; its end is not part of a
/// streak of failures.
pub const HEALTHY_RUN: Duration = Duration::from_secs(60);

/// The wait before the next attempt after `failures` failures in a row.
/// Zero failures means no wait.
pub fn delay(failures: u32) -> Duration {
    if failures == 0 {
        return Duration::ZERO;
    }
    let factor = 1u32.checked_shl(failures - 1).unwrap_or(u32::MAX);
    FIRST_DELAY.saturating_mul(factor).min(MAX_DELAY)
}

/// The restart state of one subscription.
#[derive(Debug, Default)]
pub struct Restart {
    /// Failures in a row: spawn errors, and runs that ended before
    /// [`HEALTHY_RUN`].
    failures: u32,
    /// When the running subscription started.
    started: Option<Instant>,
    /// No attempt before this.
    not_before: Option<Instant>,
}

impl Restart {
    /// Whether an attempt may be made at `now`.
    pub fn due(&self, now: Instant) -> bool {
        self.not_before.is_none_or(|t| now >= t)
    }

    /// When the next attempt is allowed, if it has to wait.
    pub fn not_before(&self) -> Option<Instant> {
        self.not_before
    }

    /// The subscription started.
    pub fn started(&mut self, now: Instant) {
        self.started = Some(now);
        self.not_before = None;
    }

    /// The subscription could not be started, or it ended. Returns the wait
    /// before the next attempt.
    pub fn failed(&mut self, now: Instant) -> Duration {
        let healthy = self
            .started
            .take()
            .is_some_and(|s| now.saturating_duration_since(s) >= HEALTHY_RUN);
        if healthy {
            self.failures = 0;
        }
        self.failures = self.failures.saturating_add(1);
        let wait = delay(self.failures);
        self.not_before = Some(now + wait);
        wait
    }
}

#[cfg(test)]
#[path = "restart_tests.rs"]
mod tests;
