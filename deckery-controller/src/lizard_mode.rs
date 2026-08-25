/// Lizard Mode suppression for the Steam Deck.
///
/// The `hid-steam` kernel driver keeps a built-in mouse/scroll fallback
/// ("Lizard Mode") active unless a process suppresses it by sending HID
/// feature reports periodically.  Steam handles this while running; this
/// module takes over that role so makima can operate without Steam.
///
/// Safety mechanism: the suppression only holds as long as the heartbeat
/// runs.  When this task is cancelled or the process exits, Lizard Mode
/// re-activates automatically within ~8 s (two missed heartbeats).
///
/// The heartbeat and haptic writes are both handled by the unified
/// `run_hidraw_writer` task in `hidraw.rs`, which selects on the haptic
/// command receiver and a timer simultaneously. This module provides the
/// configuration type and the report-builder helpers called by that task.
///
/// Protocol reference: SDL `SDL_hidapi_steamdeck.c` (`DisableDeckLizardMode` /
/// `FeedDeckLizardWatchdog`) and Linux `drivers/hid/hid-steam.c`
/// (`steam_set_lizard_mode`).
use tokio::time::Duration;

/// How often to re-send the suppression heartbeat.
/// The controller re-enables Lizard Mode after ~8 s without a heartbeat.
/// `pub(super)` so `hidraw.rs` can use this constant for the timer interval.
pub(super) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(4);

// ── Report IDs (from SDL controller_constants.h / hid-steam.c) ───────────────

/// Clears all keyboard/mouse digital mappings → disables Lizard Mode inputs.
const ID_CLEAR_DIGITAL_MAPPINGS: u8 = 0x81;

/// Sets individual controller settings (trackpad modes, pressure thresholds).
const ID_SET_SETTINGS_VALUES: u8 = 0x87;

// ── Setting indices (from SDL ControllerSettings enum) ────────────────────────

const SETTING_LEFT_TRACKPAD_MODE: u8 = 7;
const SETTING_RIGHT_TRACKPAD_MODE: u8 = 8;
const SETTING_SMOOTH_ABSOLUTE_MOUSE: u8 = 24;
const SETTING_LEFT_TRACKPAD_CLICK_PRESSURE: u8 = 52;
const SETTING_RIGHT_TRACKPAD_CLICK_PRESSURE: u8 = 53;

/// `TRACKPAD_NONE` — disables all trackpad emulation for this side.
const TRACKPAD_NONE: u16 = 7;

// ── Report builders ───────────────────────────────────────────────────────────

/// Builds the initial Lizard Mode disable report (`ID_SET_SETTINGS_VALUES`).
///
/// Sets both trackpads to `TRACKPAD_NONE` and disables absolute mouse.
/// Click pressure thresholds are managed separately via `ClickPressureHandle` /
/// `make_click_pressure_report` so they can be updated independently of Lizard
/// Mode (e.g. from trackpad config) without re-sending the full disable packet.
fn make_disable_settings_report() -> [u8; 64] {
    // Settings: (index: u8, value_lo: u8, value_hi: u8)
    let settings: &[(u8, u16)] = &[
        (SETTING_LEFT_TRACKPAD_MODE, TRACKPAD_NONE),
        (SETTING_RIGHT_TRACKPAD_MODE, TRACKPAD_NONE),
        (SETTING_SMOOTH_ABSOLUTE_MOUSE, 0),
    ];
    make_settings_report(settings)
}

/// Builds a 64-byte `ID_SET_SETTINGS_VALUES` report that sets the physical
/// click-pressure thresholds for both trackpads.
///
/// Higher values require more force to register a click; `0xFFFF` effectively
/// disables physical clicks.  Called by `hidraw::run_hidraw_writer` when a new
/// value arrives on the `click_pressure_rx` watch channel.
pub(super) fn make_click_pressure_report(left: u16, right: u16) -> [u8; 64] {
    make_settings_report(&[
        (SETTING_LEFT_TRACKPAD_CLICK_PRESSURE, left),
        (SETTING_RIGHT_TRACKPAD_CLICK_PRESSURE, right),
    ])
}

/// Builds the heartbeat settings report (`ID_SET_SETTINGS_VALUES`).
///
/// Re-asserts `TRACKPAD_NONE` for the right trackpad — matching SDL's
/// `FeedDeckLizardWatchdog`.  Must be sent together with a clear-mappings
/// report on every heartbeat tick.
fn make_heartbeat_settings_report() -> [u8; 64] {
    make_settings_report(&[(SETTING_RIGHT_TRACKPAD_MODE, TRACKPAD_NONE)])
}

/// Builds a 64-byte `ID_SET_SETTINGS_VALUES` report from a slice of
/// `(setting_index, value)` pairs.
///
/// Wire format: `[0x87, payload_len, (index, value_lo, value_hi) × n, zeros…]`
fn make_settings_report(settings: &[(u8, u16)]) -> [u8; 64] {
    let mut buf = [0u8; 64];
    buf[0] = ID_SET_SETTINGS_VALUES;
    buf[1] = (settings.len() * 3) as u8; // payload length in bytes
    for (i, (idx, val)) in settings.iter().enumerate() {
        let off = 2 + i * 3;
        buf[off] = *idx;
        buf[off + 1] = (*val & 0xFF) as u8;
        buf[off + 2] = (*val >> 8) as u8;
    }
    buf
}

/// Builds the 64-byte `ID_CLEAR_DIGITAL_MAPPINGS` report.
fn make_clear_mappings_report() -> [u8; 64] {
    let mut buf = [0u8; 64];
    buf[0] = ID_CLEAR_DIGITAL_MAPPINGS;
    buf
}

// ── Config ───────────────────────────────────────────────────────────────────

/// Which aspects of Lizard Mode to suppress.
///
/// Configured via `SUPPRESS_LIZARD_MODE` in the `[settings]` section:
///
/// ```toml
/// SUPPRESS_LIZARD_MODE = "buttons,mouse"   # suppress both (recommended)
/// SUPPRESS_LIZARD_MODE = "buttons"          # only clear digital mappings
/// SUPPRESS_LIZARD_MODE = "mouse"            # only disable trackpad emulation
/// SUPPRESS_LIZARD_MODE = "false"            # disabled
/// ```
#[derive(Debug, Clone)]
pub struct LizardModeSuppression {
    /// Send `ID_CLEAR_DIGITAL_MAPPINGS` (0x81) — suppresses keyboard/mouse
    /// button mappings (arrow keys, Enter, Esc via d-pad/buttons).
    pub suppress_buttons: bool,
    /// Send `ID_SET_SETTINGS_VALUES` (0x87) with `TRACKPAD_NONE` — suppresses
    /// trackpad mouse and scroll emulation.
    pub suppress_mouse: bool,
}

impl LizardModeSuppression {
    /// Parse from the `SUPPRESS_LIZARD_MODE` setting string.
    /// Returns `None` if the setting is absent or disabled.
    pub fn from_setting(value: &str) -> Option<Self> {
        let v = value.trim().to_lowercase();
        if v == "false" || v == "off" || v == "none" || v == "0" {
            return None;
        }
        let suppress_buttons = v.contains("buttons");
        let suppress_mouse = v.contains("mouse");
        if !suppress_buttons && !suppress_mouse {
            eprintln!(
                "Lizard Mode suppression: unrecognised value {:?} — expected \
                 \"buttons\", \"mouse\", or \"buttons,mouse\". Skipping.",
                value
            );
            return None;
        }
        Some(LizardModeSuppression { suppress_buttons, suppress_mouse })
    }
}

// ── Report builder ────────────────────────────────────────────────────────────

/// Builds the list of reports for either the initial send or a heartbeat tick.
///
/// Called by `hidraw::run_hidraw_writer` — the task that owns the fd and drives
/// the heartbeat timer. `pub(super)` so it is accessible from the sibling
/// `hidraw` module but not from the rest of the crate.
///
/// Initial send: uses `make_disable_settings_report` (sets all fields).
/// Heartbeat: uses `make_heartbeat_settings_report` (SDL's minimal watchdog).
pub(super) fn build_reports(cfg: &LizardModeSuppression, initial: bool) -> Vec<[u8; 64]> {
    let mut v = Vec::new();
    if cfg.suppress_buttons { v.push(make_clear_mappings_report()); }
    if cfg.suppress_mouse {
        v.push(if initial { make_disable_settings_report() } else { make_heartbeat_settings_report() });
    }
    v
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_setting_parses_known_values() {
        let both = LizardModeSuppression::from_setting("buttons,mouse").unwrap();
        assert!(both.suppress_buttons && both.suppress_mouse);

        let buttons = LizardModeSuppression::from_setting("buttons").unwrap();
        assert!(buttons.suppress_buttons && !buttons.suppress_mouse);

        let mouse = LizardModeSuppression::from_setting("mouse").unwrap();
        assert!(!mouse.suppress_buttons && mouse.suppress_mouse);
    }

    #[test]
    fn from_setting_rejects_disabled_values() {
        assert!(LizardModeSuppression::from_setting("false").is_none());
        assert!(LizardModeSuppression::from_setting("off").is_none());
        assert!(LizardModeSuppression::from_setting("none").is_none());
        assert!(LizardModeSuppression::from_setting("0").is_none());
    }

    #[test]
    fn from_setting_rejects_unrecognised() {
        assert!(LizardModeSuppression::from_setting("everything").is_none());
        assert!(LizardModeSuppression::from_setting("").is_none());
    }

    #[test]
    fn make_settings_report_header() {
        let r = make_settings_report(&[(7u8, 0u16)]);
        assert_eq!(r[0], ID_SET_SETTINGS_VALUES);
        assert_eq!(r[1], 3); // 1 setting × 3 bytes
        assert_eq!(r[2], 7); // index
        assert_eq!(r[3], 0); // value lo
        assert_eq!(r[4], 0); // value hi
    }

    #[test]
    fn make_clear_mappings_report_id() {
        let r = make_clear_mappings_report();
        assert_eq!(r[0], ID_CLEAR_DIGITAL_MAPPINGS);
        assert!(r[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn make_disable_settings_report_has_trackpad_none() {
        let r = make_disable_settings_report();
        assert_eq!(r[0], ID_SET_SETTINGS_VALUES);
        // First setting at offset 2: SETTING_LEFT_TRACKPAD_MODE = 7, value = TRACKPAD_NONE = 7
        assert_eq!(r[2], SETTING_LEFT_TRACKPAD_MODE);
        assert_eq!(u16::from_le_bytes([r[3], r[4]]), TRACKPAD_NONE);
        // Click pressure is no longer part of this report — it is managed separately.
        let n = r[1] as usize / 3; // number of settings
        for i in 0..n {
            let idx = r[2 + i * 3];
            assert_ne!(idx, SETTING_LEFT_TRACKPAD_CLICK_PRESSURE,  "click pressure must not appear in disable report");
            assert_ne!(idx, SETTING_RIGHT_TRACKPAD_CLICK_PRESSURE, "click pressure must not appear in disable report");
        }
    }

    #[test]
    fn make_click_pressure_report_round_trips() {
        let r = make_click_pressure_report(1000, 2000);
        assert_eq!(r[0], ID_SET_SETTINGS_VALUES);
        assert_eq!(r[1], 6); // 2 settings × 3 bytes
        assert_eq!(r[2], SETTING_LEFT_TRACKPAD_CLICK_PRESSURE);
        assert_eq!(u16::from_le_bytes([r[3], r[4]]), 1000);
        assert_eq!(r[5], SETTING_RIGHT_TRACKPAD_CLICK_PRESSURE);
        assert_eq!(u16::from_le_bytes([r[6], r[7]]), 2000);
    }
}
