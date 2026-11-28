//! The user's own choices for the trackpads, kept in `preferences.toml`.
//!
//! ```toml
//! [trackpad.right]
//! mode = "trackball"
//! ```
//!
//! The onboarding tour asks which way the right pad should work. A user file
//! cannot record just that: one with the shipped module's name replaces the
//! whole module, and `[trackpad]` comes whole from one module. So the choice
//! goes where the other choices of the user already go — which modules are on,
//! which member of a group — and is laid over the effective config key by key.
//!
//! The table mirrors the config's own: `[trackpad.right] mode` here is the
//! user's value for `[trackpad.right] mode` there. Only `mode` is read today;
//! the table is kept as written, so a key a later version reads survives a
//! save by this one.
//!
//! Where it sits: above the base config and every module, so it is the user's
//! last word on the shipped trackpad setup; below an app override that brings
//! a trackpad of its own and below an override session's layer, which are
//! more specific than a standing choice.
//!
//! A value makima cannot use is reported and ignored, never fatal: a broken
//! choice must not cost the user their module choices in the same file.

use crate::config::TrackpadConfig;

/// The `[trackpad]` table of `preferences.toml`, as written.
pub type PadPreferences = toml::value::Table;

/// The pad modes the preferences choose, after checking them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PadChoice {
    pub left: Option<String>,
    pub right: Option<String>,
}

impl PadChoice {
    /// Read the modes out of the table. A mode that is not a string, or names
    /// no trackpad mode, is reported once here and left out.
    pub fn from_table(table: &PadPreferences) -> Self {
        let mode = |side: &str| -> Option<String> {
            let value = table.get(side)?.get("mode")?;
            match value.as_str().map(|s| s.trim().to_lowercase()) {
                Some(mode) if crate::device_session::is_known_trackpad_mode(&mode) => Some(mode),
                _ => {
                    eprintln!("deckery: preferences.toml: [trackpad.{side}] mode = {value} \
                               is not a trackpad mode — ignored");
                    None
                }
            }
        };
        Self { left: mode("left"), right: mode("right") }
    }

    /// Lay the chosen modes over `trackpad`. The side's own table is the
    /// handler's input, and it carries the mode too, so both change.
    pub fn apply(&self, trackpad: &mut TrackpadConfig) {
        for (choice, side) in [(&self.left, &mut trackpad.left), (&self.right, &mut trackpad.right)] {
            let Some(mode) = choice else { continue };
            side.mode = mode.clone();
            if let toml::Value::Table(table) = &mut side.handler_config {
                table.insert("mode".to_string(), toml::Value::String(mode.clone()));
            }
        }
    }
}

#[cfg(test)]
#[path = "pad_preferences_tests.rs"]
mod tests;
