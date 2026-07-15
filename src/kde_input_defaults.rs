//! Writes KDE/KWin libinput settings for Deckery virtual trackpad devices into
//! `~/.config/kcminputrc` on every makima startup.
//!
//! KWin reads kcminputrc when a new input device appears. If a matching
//! `[Libinput][vendor][product][name]` section already exists it applies those
//! settings; otherwise it uses KDE defaults — which include tap-to-click enabled
//! and flat acceleration, neither of which is right for a Steam Deck pad.
//!
//! `ensure_kde_input_defaults()` always overwrites the Deckery sections (Option
//! B): the makima TOML config is the single source of truth for these settings.
//! KDE settings-UI changes survive until the next makima restart.
//!
//! Visible config fields (shown in example config with their defaults):
//!   [trackpad.left.kde] / [trackpad.right.kde]:
//!     pointer_acceleration = 0.2
//!     pointer_acceleration_profile = "flat"   # or "adaptive"
//!   [trackpad.gestures.kde]:
//!     natural_scroll = true
//!     scroll_factor = 0.5
//!
//! Hidden-but-overridable fields (not shown, sensible defaults):
//!   tap_to_click = false, disable_while_typing = false

use crate::virtual_devices::{DECKERY_PRODUCT, DECKERY_VENDOR};
use std::fs;

// ── Acceleration profile ──────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AccelerationProfile {
    #[default]
    Flat,
    Adaptive,
}

impl AccelerationProfile {
    fn kde_value(&self) -> u8 {
        match self {
            Self::Flat => 2,
            Self::Adaptive => 1,
        }
    }
}

// ── Per-pad KDE config ────────────────────────────────────────────────────────

fn default_pointer_acceleration() -> f32 { 0.2 }

/// KDE/libinput settings for a single trackpad (left or right).
/// Deserialised from `[trackpad.left.kde]` / `[trackpad.right.kde]`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PadKdeConfig {
    /// Hidden: default false — tap-to-click causes accidental clicks when
    /// switching between gesture mode and cursor mode.
    #[serde(default)]
    pub tap_to_click: bool,
    /// Hidden: default false — irrelevant in gaming context.
    #[serde(default)]
    pub disable_while_typing: bool,
    /// Visible: cursor speed within the chosen profile. Range -1.0..+1.0.
    #[serde(default = "default_pointer_acceleration")]
    pub pointer_acceleration: f32,
    /// Visible: `"flat"` (1:1, recommended) or `"adaptive"` (KDE default).
    #[serde(default)]
    pub pointer_acceleration_profile: AccelerationProfile,
    /// Hidden: default false for single pads (cursor, not scroll device).
    #[serde(default)]
    pub natural_scroll: bool,
}

impl Default for PadKdeConfig {
    fn default() -> Self {
        Self {
            tap_to_click: false,
            disable_while_typing: false,
            pointer_acceleration: default_pointer_acceleration(),
            pointer_acceleration_profile: AccelerationProfile::Flat,
            natural_scroll: false,
        }
    }
}

impl PadKdeConfig {
    pub fn from_toml_value(value: &toml::Value) -> Self {
        value.clone().try_into::<PadKdeConfig>().unwrap_or_else(|e| {
            eprintln!("[makima] Warning: invalid [trackpad.*.kde] config ({}), using defaults.", e);
            PadKdeConfig::default()
        })
    }
}

// ── Gesture pad KDE config ────────────────────────────────────────────────────

fn default_natural_scroll_true() -> bool { true }
fn default_scroll_factor() -> f32 { 0.5 }

/// KDE/libinput settings for the combined gesture device.
/// Deserialised from `[trackpad.gestures.kde]`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct GestureKdeConfig {
    /// Hidden: default false.
    #[serde(default)]
    pub tap_to_click: bool,
    /// Hidden: default false.
    #[serde(default)]
    pub disable_while_typing: bool,
    /// Visible: default true — content follows finger direction.
    #[serde(default = "default_natural_scroll_true")]
    pub natural_scroll: bool,
    /// Visible: default 0.5 — raw pad range (±32767) makes KDE default too fast.
    #[serde(default = "default_scroll_factor")]
    pub scroll_factor: f32,
    /// Hidden: default flat.
    #[serde(default)]
    pub pointer_acceleration_profile: AccelerationProfile,
}

impl Default for GestureKdeConfig {
    fn default() -> Self {
        Self {
            tap_to_click: false,
            disable_while_typing: false,
            natural_scroll: true,
            scroll_factor: 0.5,
            pointer_acceleration_profile: AccelerationProfile::Flat,
        }
    }
}

impl GestureKdeConfig {
    pub fn from_toml_value(value: &toml::Value) -> Self {
        value.clone().try_into::<GestureKdeConfig>().unwrap_or_else(|e| {
            eprintln!("[makima] Warning: invalid [trackpad.gestures.kde] config ({}), using defaults.", e);
            GestureKdeConfig::default()
        })
    }
}

// ── kcminputrc writer ─────────────────────────────────────────────────────────

fn kcminputrc_path() -> Option<std::path::PathBuf> {
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{}/.config", h)))?;
    Some(std::path::PathBuf::from(config_dir).join("kcminputrc"))
}

/// Removes all `[Libinput][DECKERY_VENDOR][DECKERY_PRODUCT][*]` sections from
/// `content`, returning the cleaned file text.
fn remove_deckery_sections(content: &str) -> String {
    let prefix = format!("[Libinput][{}][{}][", DECKERY_VENDOR, DECKERY_PRODUCT);
    let mut result = String::with_capacity(content.len());
    let mut in_deckery = false;

    for line in content.lines() {
        if line.starts_with('[') {
            in_deckery = line.starts_with(&prefix);
        }
        if !in_deckery {
            result.push_str(line);
            result.push('\n');
        }
    }
    // Trim trailing blank lines left by removed sections.
    result.trim_end_matches('\n').to_string()
}

fn write_pad_section(out: &mut String, name: &str, cfg: &PadKdeConfig) {
    out.push_str(&format!(
        "\n[Libinput][{}][{}][{}]\n",
        DECKERY_VENDOR, DECKERY_PRODUCT, name
    ));
    out.push_str(&format!("DisableWhileTyping={}\n", cfg.disable_while_typing));
    out.push_str(&format!("NaturalScroll={}\n", cfg.natural_scroll));
    out.push_str(&format!("PointerAcceleration={:.3}\n", cfg.pointer_acceleration));
    out.push_str(&format!("PointerAccelerationProfile={}\n", cfg.pointer_acceleration_profile.kde_value()));
    out.push_str(&format!("TapToClick={}\n", cfg.tap_to_click));
}

fn write_gesture_section(out: &mut String, cfg: &GestureKdeConfig) {
    out.push_str(&format!(
        "\n[Libinput][{}][{}][Deckery Combined Trackpad]\n",
        DECKERY_VENDOR, DECKERY_PRODUCT
    ));
    out.push_str(&format!("DisableWhileTyping={}\n", cfg.disable_while_typing));
    out.push_str(&format!("NaturalScroll={}\n", cfg.natural_scroll));
    out.push_str(&format!("PointerAccelerationProfile={}\n", cfg.pointer_acceleration_profile.kde_value()));
    out.push_str(&format!("ScrollFactor={}\n", cfg.scroll_factor));
    out.push_str(&format!("TapToClick={}\n", cfg.tap_to_click));
}

/// Writes KDE libinput defaults for all three Deckery virtual trackpad devices
/// to kcminputrc, always overwriting any existing Deckery sections. Call this
/// after loading config but before creating uinput devices.
pub fn ensure_kde_input_defaults(
    left: &PadKdeConfig,
    right: &PadKdeConfig,
    gesture: &GestureKdeConfig,
) {
    let path = match kcminputrc_path() {
        Some(p) => p,
        None => {
            eprintln!("[makima] Warning: XDG_CONFIG_HOME and HOME not set; skipping kcminputrc defaults.");
            return;
        }
    };

    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut new_content = remove_deckery_sections(&existing);

    write_pad_section(&mut new_content, "Deckery Left Trackpad", left);
    write_pad_section(&mut new_content, "Deckery Right Trackpad", right);
    write_gesture_section(&mut new_content, gesture);

    new_content.push('\n');

    match fs::write(&path, &new_content) {
        Ok(_) => {}
        Err(e) => eprintln!("[makima] Warning: could not write kcminputrc ({}); libinput defaults may not apply.", e),
    }
}
