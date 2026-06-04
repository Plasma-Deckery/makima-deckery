/// Lizard Mode suppression for the Steam Deck.
///
/// The `hid-steam` kernel driver keeps a built-in mouse/scroll fallback
/// ("Lizard Mode") active unless a process suppresses it by sending HID
/// feature reports periodically.  Steam handles this while running; this
/// module takes over that role so makima can operate without Steam.
///
/// Safety mechanism: the suppression only holds as long as the heartbeat
/// runs.  When this task is cancelled or the process exits, the file
/// descriptors are closed and Lizard Mode re-activates automatically within
/// ~8 s (two missed heartbeats).
///
/// Protocol reference: SDL `SDL_hidapi_steamdeck.c` (`DisableDeckLizardMode` /
/// `FeedDeckLizardWatchdog`) and Linux `drivers/hid/hid-steam.c`
/// (`steam_set_lizard_mode`).
use std::fs::{self, File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use tokio::time::{self, Duration};

/// Valve USB vendor ID, as it appears in sysfs uevent files.
const VALVE_VENDOR_ID: &str = "000028DE";

/// `HIDIOCSFEATURE(64)` ioctl request code.
///
/// Calculated as `_IOC(_IOC_WRITE|_IOC_READ, 'H', 0x06, 64)`:
///   `(3 << 30) | (0x48 << 8) | 0x06 | (64 << 16)` = `0xC040_4806`
const HIDIOCSFEATURE_64: libc::c_ulong = 0xC040_4806;

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

// ── Device discovery ──────────────────────────────────────────────────────────

/// Finds Valve hidraw devices that are the **raw controller interface** —
/// identified by having no `input/` subdirectory in sysfs (as opposed to the
/// emulated keyboard/mouse hidraw nodes which do have one).
///
/// hidraw0/hidraw1 back the Lizard Mode keyboard and mouse emulation devices
/// and reject our ioctls with `ETIMEDOUT`.  hidraw2 (sysfs device `.0005`,
/// no input node) is the channel hid-steam exposes specifically for userspace
/// controller communication.
fn find_controller_hidraw_devices() -> Vec<PathBuf> {
    let mut result = Vec::new();
    let Ok(dir) = fs::read_dir("/sys/class/hidraw") else {
        return result;
    };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else { continue };

        let base = format!("/sys/class/hidraw/{}/device", name_str);

        // Must be a Valve device.
        let uevent = fs::read_to_string(format!("{}/uevent", base)).unwrap_or_default();
        if !uevent.contains(VALVE_VENDOR_ID) {
            continue;
        }

        // Must NOT have an associated input device (i.e. not an emulated kb/mouse).
        if std::path::Path::new(&format!("{}/input", base)).exists() {
            continue;
        }

        result.push(PathBuf::from(format!("/dev/{}", name_str)));
    }
    result.sort();
    result
}

// ── ioctl helper ─────────────────────────────────────────────────────────────

fn send_feature_report(fd: libc::c_int, buf: &[u8; 64]) -> bool {
    let ret = unsafe { libc::ioctl(fd, HIDIOCSFEATURE_64, buf.as_ptr()) };
    ret >= 0
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
/// Spawned once at startup.  Gracefully skips if no suitable hidraw
/// device is found (i.e. makima is running on non-Steam-Deck hardware).
pub async fn run_lizard_mode_suppression(cfg: LizardModeSuppression) {
    if !cfg.is_any() {
        return;
    }

    let candidates = find_controller_hidraw_devices();
    if candidates.is_empty() {
        println!("Lizard Mode suppression: no suitable hidraw device found, skipping.");
        return;
    }

    // Pre-build reports based on config so the hot loop doesn't branch.
    let initial_reports: Vec<[u8; 64]> = {
        let mut v = Vec::new();
        if cfg.suppress_buttons { v.push(make_clear_mappings_report()); }
        if cfg.suppress_mouse   { v.push(make_disable_settings_report()); }
        v
    };
    // SDL's FeedDeckLizardWatchdog sends 0x81 + 0x87 on every tick.
    let heartbeat_reports: Vec<[u8; 64]> = {
        let mut v = Vec::new();
        if cfg.suppress_buttons { v.push(make_clear_mappings_report()); }
        if cfg.suppress_mouse   { v.push(make_heartbeat_settings_report()); }
        v
    };

    // Open the first candidate that accepts all initial suppression reports.
    let (file, path) = match candidates.iter().find_map(|p| {
        let file = OpenOptions::new().read(true).write(true).open(p).ok()?;
        let fd = file.as_raw_fd();
        let ok = initial_reports.iter().all(|r| send_feature_report(fd, r));
        if ok { Some((file, p.clone())) } else { None }
    }) {
        Some(pair) => pair,
        None => {
            eprintln!(
                "Lizard Mode suppression: found hidraw candidate(s) but none accepted \
                 the suppression reports. Skipping."
            );
            return;
        }
    };

    println!(
        "Lizard Mode suppression active on {:?} (buttons={}, mouse={}).",
        path, cfg.suppress_buttons, cfg.suppress_mouse
    );

    let fd = file.as_raw_fd();
    let _keep_alive = file; // keep fd open — signals to hid-steam that a client is active

    let mut interval = time::interval(HEARTBEAT_INTERVAL);
    interval.tick().await; // consume the immediate first tick (already sent above)

    loop {
        interval.tick().await;
        let ok = heartbeat_reports.iter().all(|r| send_feature_report(fd, r));
        if !ok {
            eprintln!(
                "Lizard Mode suppression: heartbeat failed on {:?}. \
                 Lizard Mode may reactivate.",
                path
            );
        }
    }
}
