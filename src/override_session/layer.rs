//! The override document, and laying it over a resolved config.
//!
//! The document is a config in TOML form — `[remap]`, `[commands]`,
//! `[trackpad]` and so on — that starts empty and grows one `override set` at
//! a time. It is parsed by `Config::from_toml_value`, the deserialisation and
//! the `from_raw` a config file goes through, and laid over the resolved
//! config with `merge_base`, the merge an app override gets. So a binding in
//! the document behaves exactly as it would in a file of the user's own, only
//! above every one of them.
//!
//! Two things differ from a file, both because the tour needs them:
//!
//! * `cleared` drops every binding underneath before the document goes on top,
//!   which is how the tour starts from nothing and unlocks buttons one by one.
//! * `[trackpad]` is merged key by key in source form. A file's trackpad table
//!   replaces the one underneath wholesale, which would take the user's
//!   `click_pressure` and KDE settings with it when the tour switches a mode.

use super::{allowlist, path};
use crate::config::{Config, RawTrackpadConfig, TrackpadConfig};
use std::collections::{HashMap, HashSet};

/// The sections `clear` empties.
const BINDING_SECTIONS: &[&str] = &["remap", "commands", "movements", "hints"];

/// The sections a device session reads once, when it starts. A change to
/// either only takes effect when the session is rebuilt; see `planner`.
const DEVICE_SECTIONS: &[&str] = &["trackpad", "settings"];

/// The document and whether the bindings underneath are cleared.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layer {
    cleared: bool,
    doc: toml::value::Table,
}

impl Layer {
    #[cfg(test)]
    pub fn is_cleared(&self) -> bool {
        self.cleared
    }

    /// The document with `value` at `path`. Only the path is checked here;
    /// whether the result is a config is `validate`'s question.
    pub fn with_value(&self, path: &str, value: toml::Value) -> Result<Layer, String> {
        let segments = path::segments(path)?;
        let mut next = self.clone();
        path::insert(&mut next.doc, &segments, value)?;
        Ok(next)
    }

    /// This layer with every binding gone: the document's and those
    /// underneath. Trackpad and settings are not bindings and stay.
    pub fn cleared(&self) -> Layer {
        let mut next = self.clone();
        next.cleared = true;
        for section in BINDING_SECTIONS {
            next.doc.remove(*section);
        }
        next
    }

    /// Whether the two differ in a section a device session only reads at
    /// its start.
    pub fn device_sections_differ(&self, other: &Layer) -> bool {
        DEVICE_SECTIONS.iter().any(|s| self.doc.get(*s) != other.doc.get(*s))
    }

    /// The document as a config of its own, refusing anything a file would
    /// only warn about — an override applies all of it or none.
    pub fn parse(&self, aliases: &HashMap<String, String>) -> Result<Config, String> {
        let (config, problems) = Config::from_toml_value(
            toml::Value::Table(self.doc.clone()), "override".to_string(), aliases)?;
        if problems.is_empty() {
            Ok(config)
        } else {
            Err(problems.join("; "))
        }
    }

    /// `underlay` with this layer on top.
    ///
    /// What the config *is* — its name, device, module, aliases, Gaming Mode —
    /// stays the underlay's, so the state export still names the base and the
    /// focused app, and nothing about which device it drives changes.
    pub fn apply(&self, underlay: &Config) -> Result<Config, String> {
        let own = self.parse(&underlay.aliases)?;
        let base = if self.cleared { underlay.without_bindings() } else { underlay.clone() };
        let mut effective = own.merged_with_base(&base);
        effective.trackpad = match self.doc.get("trackpad") {
            Some(over) => {
                let mut raw = base.trackpad.to_raw_value();
                path::deep_merge(&mut raw, over);
                let raw: RawTrackpadConfig = raw.try_into()
                    .map_err(|e: toml::de::Error| format!("[trackpad]: {e}"))?;
                TrackpadConfig::from_raw(raw)
            }
            None => base.trackpad.clone(),
        };
        effective.name = base.name.clone();
        effective.device = base.device.clone();
        effective.module = base.module.clone();
        effective.aliases = base.aliases.clone();
        effective.gaming_mode_config = base.gaming_mode_config.clone();
        // Which bindings the focused app brought, for the state export's
        // origins. The layer's own count as the base's: it has no file.
        effective.override_bindings = base.override_bindings.clone();
        let _ = effective.resolve_hints();
        Ok(effective)
    }

    /// Whether this layer may go over `underlay`: it parses without a skipped
    /// binding, binds only commands the user's configs already bind, lays
    /// over the underlay, and every trackpad mode and setting it names is one
    /// the device session understands.
    ///
    /// Each of those is the check that already exists for a config file —
    /// the parser, `is_known_trackpad_mode`, `Settings::from_config` — asked
    /// to refuse what it would otherwise only warn about.
    pub fn validate(&self, underlay: &Config, known_commands: &HashSet<Vec<String>>) -> Result<(), String> {
        let own = self.parse(&underlay.aliases)?;
        allowlist::check(&own, known_commands)?;
        let effective = self.apply(underlay)?;

        let trackpad = self.doc.get("trackpad");
        for (side, config) in [("left", &effective.trackpad.left), ("right", &effective.trackpad.right)] {
            let names_mode = trackpad.and_then(|t| t.get(side)).and_then(|s| s.get("mode")).is_some();
            if names_mode && !crate::device_session::is_known_trackpad_mode(&config.mode) {
                return Err(format!("trackpad.{side}.mode: {:?} is not a trackpad mode", config.mode));
            }
        }

        if let Some(toml::Value::Table(settings)) = self.doc.get("settings") {
            let (_, problems) = crate::settings::Settings::from_config(&effective);
            if let Some(problem) = problems.iter().find(|p| settings.contains_key(&p.key)) {
                return Err(format!("[settings] {} = {:?} is not {}", problem.key, problem.value, problem.expected));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "layer_tests.rs"]
mod tests;
