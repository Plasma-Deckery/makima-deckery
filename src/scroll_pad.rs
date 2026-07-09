//! Placeholder for a future "scroll" trackpad handler (a pad dedicated to
//! emitting scroll-wheel events instead of pointer movement).
//!
//! Not wired into `event_reader.rs` yet — a pad with `mode = "scroll"`
//! currently just spawns no handler (no virtual device, no events forwarded).
//! This module only exists so its config schema has a home once real
//! behaviour lands, following the same self-parsing pattern as
//! `mt_trackpad::MtTrackpadConfig`: Core (`config.rs`) only knows `mode` and
//! `click_pressure` for this pad; everything else is owned here.
#![allow(dead_code)]

/// This handler's own config, self-deserialized from the raw `handler_config`
/// TOML sub-table Core hands it. Empty for now — no fields until real
/// behaviour is implemented.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ScrollConfig {}

impl ScrollConfig {
    pub fn from_toml_value(value: &toml::Value) -> Self {
        value.clone().try_into::<ScrollConfig>().unwrap_or_else(|e| {
            eprintln!("[makima] Warning: invalid scroll handler config ({}), using defaults.", e);
            ScrollConfig::default()
        })
    }
}
