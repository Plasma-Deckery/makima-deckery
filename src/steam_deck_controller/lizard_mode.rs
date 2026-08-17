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
/// In Phase 3 the heartbeat no longer opens its own hidraw fd — it sends
/// `HidrawWrite::LizardReport` through the controller's unified writer task,
/// which owns the single shared fd. This eliminates the old race condition
/// where lizard mode and the haptic writer could issue concurrent ioctls.
///
/// Protocol reference: SDL `SDL_hidapi_steamdeck.c` (`DisableDeckLizardMode` /
/// `FeedDeckLizardWatchdog`) and Linux `drivers/hid/hid-steam.c`
/// (`steam_set_lizard_mode`).
use tokio::sync::{mpsc, watch};
use tokio::time::{self, Duration};

use super::hidraw::HidrawWrite;

/// How often to re-send the suppression heartbeat.
/// The controller re-enables Lizard Mode after ~8 s without a heartbeat.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(4);

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
/// Sets both trackpads to `TRACKPAD_NONE`, disables absolute mouse, and
/// maximises click pressure thresholds (matching SDL's `DisableDeckLizardMode`).
fn make_disable_settings_report() -> [u8; 64] {
    // Settings: (index: u8, value_lo: u8, value_hi: u8)
    let settings: &[(u8, u16)] = &[
        (SETTING_LEFT_TRACKPAD_MODE, TRACKPAD_NONE),
        (SETTING_RIGHT_TRACKPAD_MODE, TRACKPAD_NONE),
        (SETTING_SMOOTH_ABSOLUTE_MOUSE, 0),
        (SETTING_LEFT_TRACKPAD_CLICK_PRESSURE, 0xFFFF),
        (SETTING_RIGHT_TRACKPAD_CLICK_PRESSURE, 0xFFFF),
    ];
    make_settings_report(settings)
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

    pub fn is_any(&self) -> bool {
        self.suppress_buttons || self.suppress_mouse
    }
}

// ── Main task ─────────────────────────────────────────────────────────────────

/// Runs the Lizard Mode suppression heartbeat loop.
///
/// Sends `HidrawWrite::LizardReport` through the controller's unified hidraw
/// writer — no fd ownership here. The writer task serializes all hidraw writes
/// onto a single file descriptor, eliminating the old race condition between
/// lizard heartbeats and haptic commands.
///
/// `lizard_rx` is a watch channel carrying the current configuration. The task:
/// - Exits immediately if the initial value is `None`.
/// - Sends initial suppression reports, then heartbeats on `HEARTBEAT_INTERVAL`.
/// - On config change: updates heartbeat reports live (no task restart needed).
/// - On `None` config update or closed channel: exits (Lizard Mode re-activates
///   naturally after ~8 s of missed heartbeats).
pub(super) async fn run_lizard_mode_suppression(
    mut lizard_rx: watch::Receiver<Option<LizardModeSuppression>>,
    hidraw_tx: mpsc::Sender<HidrawWrite>,
) {
    // Wait for a non-None initial config.
    // borrow_and_update() holds a guard — clone before next call to lizard_rx.
    let mut cfg = loop {
        let value = lizard_rx.borrow_and_update().clone();
        match value {
            Some(c) => break c,
            None => {
                if lizard_rx.changed().await.is_err() { return; }
            }
        }
    };

    if !cfg.is_any() { return; }

    /// Sends a slice of raw reports through the unified hidraw writer.
    /// Returns false if the channel is closed (writer gone — exit).
    async fn send_reports(
        tx: &mpsc::Sender<HidrawWrite>,
        reports: &[[u8; 64]],
    ) -> bool {
        for &r in reports {
            if tx.send(HidrawWrite::LizardReport(r)).await.is_err() {
                return false;
            }
        }
        true
    }

    let initial_reports = build_reports(&cfg, true);
    let mut heartbeat_reports = build_reports(&cfg, false);

    println!(
        "Lizard Mode suppression active (buttons={}, mouse={}). +{}ms since startup",
        cfg.suppress_buttons, cfg.suppress_mouse, crate::startup_ms()
    );

    if !send_reports(&hidraw_tx, &initial_reports).await { return; }

    let mut interval = time::interval(HEARTBEAT_INTERVAL);
    interval.tick().await; // skip the immediate first tick — initial already sent

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if !send_reports(&hidraw_tx, &heartbeat_reports).await { return; }
            }
            result = lizard_rx.changed() => {
                if result.is_err() { return; } // sender dropped
                match lizard_rx.borrow_and_update().clone() {
                    None => return, // disabled
                    Some(new_cfg) => {
                        cfg = new_cfg;
                        heartbeat_reports = build_reports(&cfg, false);
                    }
                }
            }
        }
    }
}

/// Builds the list of reports for either the initial send or a heartbeat tick.
///
/// Initial send: uses `make_disable_settings_report` (sets all fields).
/// Heartbeat: uses `make_heartbeat_settings_report` (SDL's minimal watchdog).
fn build_reports(cfg: &LizardModeSuppression, initial: bool) -> Vec<[u8; 64]> {
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
    }
}
