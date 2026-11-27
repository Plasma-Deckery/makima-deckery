//! The emergency exit: a double-click on QAM (···) ends an override session.
//!
//! The tour can clear every binding, and it can crash or hang while they are
//! cleared. So there is one way out that no layer can take away. It is decided
//! on the raw button, before any binding is looked at, works while paused, and
//! only does anything while a session is open — when none is, the same
//! double-click is the Gaming Mode trigger, which is switched off for the
//! duration of a session, so exactly one of the two ever acts.
//!
//! It used to be Steam + QAM held for three seconds. On the Deck, QAM reaches
//! makima only as a pulse of ~150–180 ms however long it is held (measured on
//! the first hardware test), so a hold with it can never be seen.
//!
//! The double-click window is the Gaming Mode trigger's, from the config, so
//! the button feels the same in both roles. The last press is kept in the
//! session rather than in the reader: the tour rebuilds the device tasks at
//! every stage change, and a reader's state would not survive that.

use evdev::Key;
use std::time::Duration;
use tokio::time::Instant;

/// The button whose double-click ends a session: QAM (···).
pub const EMERGENCY_EXIT_KEY: Key = Key::BTN_BASE;

/// The press edges of `EMERGENCY_EXIT_KEY` while a session is open.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DoubleClick {
    last_press: Option<Instant>,
}

impl DoubleClick {
    /// One press at `now`. True when it is the second of two presses at most
    /// `window` apart — the same test the Gaming Mode trigger makes. A
    /// double-click is used up by the press that completes it, so a third
    /// press starts a new one.
    pub fn press(&mut self, now: Instant, window: Duration) -> bool {
        let double = self.last_press.is_some_and(|prev| now.duration_since(prev) <= window);
        self.last_press = if double { None } else { Some(now) };
        double
    }
}

#[cfg(test)]
#[path = "emergency_tests.rs"]
mod tests;
