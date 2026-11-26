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

use super::{allowlist, checks, path};
use crate::config::{Config, RawTrackpadConfig, TrackpadConfig};
use std::collections::{HashMap, HashSet};

/// The sections `clear` empties.
const BINDING_SECTIONS: &[&str] = &["remap", "commands", "movements", "hints"];

/// The sections a device session reads once, when it starts. A change to
/// either only takes effect when the session is rebuilt; see `planner`.
const DEVICE_SECTIONS: &[&str] = &["trackpad", "settings"];

/// Why a layer was refused.
///
/// The reply to a refused `set` names where the problem is exactly once. A
/// check that knows the spot — a binding, a trackpad mode, a setting — says
/// so itself, and its message stands as it is; anything else, a parser error
/// about the document as a whole, is put under the path that was set.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    message: String,
    located: bool,
}

impl Refusal {
    /// A refusal at a known spot in the document, in path form.
    pub fn at(location: impl std::fmt::Display, why: impl std::fmt::Display) -> Self {
        Self { message: format!("{location}: {why}"), located: true }
    }

    /// The text of the reply to a `set` of `path`.
    pub fn for_path(&self, path: &str) -> String {
        if self.located { self.message.clone() } else { format!("{path}: {}", self.message) }
    }
}

/// A refusal that does not say where; the path it came from will.
impl From<String> for Refusal {
    fn from(message: String) -> Self {
        Self { message, located: false }
    }
}

impl From<&str> for Refusal {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

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

    /// The document as a config of its own, refusing a binding the parser
    /// would skip. The rest of what a file only warns about is `validate`'s.
    pub fn parse(&self, aliases: &HashMap<String, String>) -> Result<Config, Refusal> {
        let (config, problems) = Config::from_toml_value(
            toml::Value::Table(self.doc.clone()), "override".to_string(), aliases)?;
        if problems.is_empty() {
            Ok(config)
        } else {
            // Each problem leads with where it is written.
            Err(Refusal { message: problems.join("; "), located: true })
        }
    }

    /// `underlay` with this layer on top.
    ///
    /// What the config *is* — its name, device, module, aliases, Gaming Mode —
    /// stays the underlay's, so the state export still names the base and the
    /// focused app, and nothing about which device it drives changes.
    pub fn apply(&self, underlay: &Config) -> Result<Config, String> {
        let own = self.parse(&underlay.aliases).map_err(|r| r.to_string())?;
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
        // origins.
        effective.override_bindings = base.override_bindings.clone();
        // What the layer itself set, so the state export can name it as the
        // origin rather than the file whose binding it shadows.
        effective.layer_bindings = Some(own.bindings.clone());
        let _ = effective.resolve_hints();
        Ok(effective)
    }

    /// Whether this layer may go over `underlay`, all of it as written.
    ///
    /// It parses without a skipped binding, binds only commands the user's
    /// configs already bind, and lays over the underlay; then `checks` refuses
    /// every fallback the device session, the hint resolver or the settings
    /// would make for what the layer writes. Each of those is the check that
    /// already exists for a config file, asked to refuse what it would
    /// otherwise only warn about.
    pub fn validate(&self, underlay: &Config, known_commands: &HashSet<Vec<String>>) -> Result<(), Refusal> {
        let own = self.parse(&underlay.aliases)?;
        allowlist::check(&own, known_commands)?;
        let effective = self.apply(underlay)?;
        checks::trackpad(self.doc.get("trackpad"), underlay, &effective)?;
        checks::hints(self.doc.get("hints"), &effective)?;
        checks::settings(self.doc.get("settings"), &effective)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "layer_tests.rs"]
mod tests;
