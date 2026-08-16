//! Steam Deck controller lifecycle — device discovery, evdev stream, hidraw
//! topology, Lizard Mode suppression, and suspend/resume watching.
//!
//! This is the single module for everything that is specific to the Steam Deck
//! as a physical device. Higher-level concerns (input mapping, virtual devices,
//! config routing) remain in `udev_monitor`, `event_reader`, etc.
//!
//! ## Module layout
//!
//! ```
//! steam_deck_controller/
//!   mod.rs            ← SteamDeckController struct + hidraw discovery
//!   lizard_mode.rs    ← Lizard Mode suppression heartbeat
//!   resume_watcher.rs ← logind PrepareForSleep D-Bus watcher
//! ```
//!
//! ## Usage
//!
//! Call once at process startup (from `udev_monitor::start_monitoring_udev`):
//!
//! ```ignore
//! steam_deck_controller::start_background_tasks(lizard_cfg, resume_notify.clone());
//! ```
//!
//! Call per matched device (from `udev_monitor::launch_tasks`):
//!
//! ```ignore
//! let controller = SteamDeckController::from_evdev(Path::new(&event_device));
//! let stream = controller.open_event_stream(grab);
//! // controller.hidraw_path → passed to pad_hidraw::spawn
//! ```

pub mod lizard_mode;
pub mod resume_watcher;

pub use lizard_mode::LizardModeSuppression;

use evdev::{Device, EventStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Notify;

/// Represents the physical Steam Deck controller as a single lifecycle object.
///
/// Owns the device paths (evdev + hidraw) discovered at construction time.
/// Callers open streams or pass paths to subsystems (pad hidraw reader,
/// haptic writer, lizard mode) — the controller is the single source of
/// truth for device topology.
pub struct SteamDeckController {
    pub evdev_path: PathBuf,
    /// Raw controller hidraw path, if one was found via sysfs. `None` on
    /// non-Steam Deck hardware or if sysfs traversal failed.
    pub hidraw_path: Option<PathBuf>,
}

impl SteamDeckController {
    /// Construct from a known evdev device path.
    ///
    /// Immediately attempts to discover the hidraw sibling via sysfs. This is
    /// a synchronous sysfs read and completes in microseconds.
    pub fn from_evdev(evdev_path: &Path) -> Self {
        let hidraw_path = find_controller_hidraw_for_evdev(evdev_path);
        Self { evdev_path: evdev_path.to_path_buf(), hidraw_path }
    }

    /// Open the evdev device, optionally grab it exclusively, and return an
    /// async event stream. Panics if the device cannot be opened or grabbed —
    /// both are unrecoverable at startup.
    pub fn open_event_stream(&self, grab: bool) -> EventStream {
        let mut device = Device::open(&self.evdev_path)
            .expect("Couldn't open device path.");
        if grab {
            device
                .grab()
                .expect("Unable to grab device. Is another instance of Makima running?");
        }
        device.into_event_stream().unwrap()
    }
}

/// Spawn the process-level background tasks for the Steam Deck controller.
///
/// Call **once** at startup from `udev_monitor::start_monitoring_udev`.
/// Both tasks run for the lifetime of the process.
///
/// - `lizard_cfg`: parsed from `SUPPRESS_LIZARD_MODE` in `[settings]`.
///   Pass `None` to skip Lizard Mode suppression entirely.
/// - `resume_notify`: fired by the resume watcher on logind wake signal.
///   `udev_monitor` listens to this and triggers a `launch_tasks` reinit.
pub fn start_background_tasks(lizard_cfg: Option<LizardModeSuppression>, resume_notify: Arc<Notify>) {
    tokio::spawn(resume_watcher::start_resume_watcher(resume_notify));
    if let Some(cfg) = lizard_cfg {
        // Lizard mode discovers its own hidraw via vendor-ID scan at startup
        // (independent of per-device evdev path, which isn't known yet here).
        tokio::spawn(lizard_mode::run_lizard_mode_suppression(cfg, None));
    }
}

/// Find the raw controller hidraw sibling for a known evdev device path.
///
/// Combines USB-interface sysfs traversal (same approach as the original
/// `pad_hidraw::find_hidraw_for_evdev`) with the no-`input/`-subdir filter
/// from `lizard_mode::find_controller_hidraw_devices` to distinguish the raw
/// controller channel from the kb/mouse emulation hidraw nodes.
///
/// Sysfs layout on Steam Deck:
/// ```text
/// evdev:   /sys/class/input/eventN/device  → …/usb_iface/HID_A/input/inputN
/// hidraw:  /sys/class/hidraw/hidrawN/device → …/usb_iface/HID_B   (raw, no input/)
///          /sys/class/hidraw/hidrawM/device → …/usb_iface/HID_C   (emulated, has input/)
/// ```
pub fn find_controller_hidraw_for_evdev(evdev_path: &Path) -> Option<PathBuf> {
    let dev_name = evdev_path.file_name()?.to_str()?;
    let evdev_sysfs = std::fs::canonicalize(
        format!("/sys/class/input/{}/device", dev_name)
    ).ok()?;
    // evdev_sysfs is …/usb_iface/HID_A/input/inputN
    // Go up three levels: inputN → input/ → HID_A/ → usb_iface/
    let usb_iface = evdev_sysfs.parent()?.parent()?.parent()?;

    let mut candidates = Vec::new();
    for entry in std::fs::read_dir("/sys/class/hidraw/").ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name().to_string_lossy().to_string();
        if let Ok(hidraw_sysfs) = std::fs::canonicalize(
            format!("/sys/class/hidraw/{}/device", name)
        ) {
            // Must share the USB interface parent.
            if hidraw_sysfs.parent() != Some(usb_iface) {
                continue;
            }
            // Must NOT have an input/ subdirectory — that would be a kb/mouse
            // emulation node (backed by hid-steam's Lizard Mode input path)
            // rather than the raw controller channel.
            if Path::new(&format!("/sys/class/hidraw/{}/device/input", name)).exists() {
                continue;
            }
            candidates.push(PathBuf::from(format!("/dev/{}", name)));
        }
    }
    candidates.sort();
    candidates.into_iter().next()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// On non-Steam Deck hardware (or in CI), there is no `/dev/input/event*`
    /// Steam Deck device — the function must return `None` gracefully rather
    /// than panicking.
    #[test]
    fn find_hidraw_returns_none_for_nonexistent_evdev() {
        let result = find_controller_hidraw_for_evdev(Path::new("/dev/input/event99999"));
        assert!(result.is_none());
    }

    #[test]
    fn steam_deck_controller_from_nonexistent_evdev_has_no_hidraw() {
        let ctrl = SteamDeckController::from_evdev(Path::new("/dev/input/event99999"));
        assert_eq!(ctrl.evdev_path, PathBuf::from("/dev/input/event99999"));
        assert!(ctrl.hidraw_path.is_none());
    }
}
