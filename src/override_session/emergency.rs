//! The emergency exit: a combo that always ends an override session.
//!
//! The tour can clear every binding, and it can crash or hang while they are
//! cleared. So there is one way out that no config can take away: holding
//! Steam and QAM (···) together for three seconds ends the session, however
//! the layer looks. It is decided on the raw buttons, before any binding is
//! looked at, and only does anything while a session is open.
//!
//! The combo and the hold are here and nowhere else.

use evdev::Key;
use std::time::Duration;

/// The buttons that make up the emergency exit: Steam and QAM (···).
pub const EMERGENCY_EXIT_COMBO: [Key; 2] = [Key::BTN_MODE, Key::BTN_BASE];

/// How long the combo has to be held.
pub const EMERGENCY_EXIT_HOLD: Duration = Duration::from_secs(3);

/// Which buttons of the combo are down.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ComboTracker {
    held: [bool; EMERGENCY_EXIT_COMBO.len()],
}

impl ComboTracker {
    /// Record one key edge. Returns whether the whole combo is held now if
    /// that changed with this edge, `None` if it did not.
    ///
    /// A repeat (value 2) is no edge, and a key outside the combo changes
    /// nothing: holding the combo while pressing something else still counts.
    pub fn update(&mut self, key: Key, value: i32) -> Option<bool> {
        let index = EMERGENCY_EXIT_COMBO.iter().position(|k| *k == key)?;
        let down = match value {
            0 => false,
            1 => true,
            _ => return None,
        };
        let before = self.all_held();
        self.held[index] = down;
        let after = self.all_held();
        (before != after).then_some(after)
    }

    pub fn all_held(&self) -> bool {
        self.held.iter().all(|h| *h)
    }

    /// The combo's buttons that are down, for the journal.
    pub fn held_keys(&self) -> Vec<Key> {
        EMERGENCY_EXIT_COMBO.iter().zip(self.held).filter(|(_, h)| *h).map(|(k, _)| *k).collect()
    }
}

#[cfg(test)]
#[path = "emergency_tests.rs"]
mod tests;
