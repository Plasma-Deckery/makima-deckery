//! Placeholder for a future "trackball" trackpad handler.
//!
//! Not wired into `event_reader.rs` yet — a pad with `mode = "trackball"`
//! currently just spawns no handler (no virtual device, no events forwarded).
//! This module only exists so its config schema has a home once real
//! behaviour lands, following the same self-parsing pattern as
//! `mt_trackpad::MtTrackpadConfig`: Core (`config.rs`) only knows `mode` and
//! `click_pressure` for this pad; everything else is owned here.
//!
//! Two open design questions (deliberately not decided yet, see the
//! discussion on the trackpad-router epic):
//!   - Whether trackball is a "movement type" variant living inside the
//!     `mt_trackpad` handler, or a fully separate module with its own output
//!     device (relative mouse instead of absolute MT events).
//!   - What its config fields should actually be (momentum/velocity/deadzone
//!     knobs existed in an earlier draft of this config but were never wired
//!     to firmware — removed here rather than carried forward speculatively).
#![allow(dead_code)]

/// This handler's own config, self-deserialized from the raw `handler_config`
/// TOML sub-table Core hands it. Empty for now — no fields until the design
/// questions above are resolved and real behaviour is implemented.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct TrackballConfig {}

impl TrackballConfig {
    pub fn from_toml_value(value: &toml::Value) -> Self {
        value.clone().try_into::<TrackballConfig>().unwrap_or_else(|e| {
            eprintln!("[makima] Warning: invalid trackball handler config ({}), using defaults.", e);
            TrackballConfig::default()
        })
    }
}
