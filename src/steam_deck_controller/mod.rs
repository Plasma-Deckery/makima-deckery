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
//!   mod.rs            ← SteamDeckController struct + hidraw discovery + reconnecting reader
//!   lizard_mode.rs    ← Lizard Mode suppression heartbeat
//!   resume_watcher.rs ← logind PrepareForSleep D-Bus watcher
//! ```
//!
//! ## Usage
//!
//! Call once at process startup (from `udev_monitor::start_monitoring_udev`):
//!
//! ```ignore
//! // resume_notify is created here and shared with launch_tasks:
//! let resume_notify = Arc::new(Notify::new());
//! steam_deck_controller::start_background_tasks(lizard_cfg, resume_notify.clone());
//! ```
//!
//! Call per matched device (from `udev_monitor::launch_tasks`):
//!
//! ```ignore
//! let controller = SteamDeckController::from_evdev(Path::new(&event_device));
//! let (is_tablet, max_abs_wheel, event_rx) =
//!     controller.start_event_task(grab, resume_notify, device_error_notify);
//! // controller.hidraw_path → passed to pad_hidraw::spawn
//! ```

pub mod lizard_mode;
pub mod resume_watcher;

pub use lizard_mode::LizardModeSuppression;

use evdev::{Device, EventStream, InputEvent};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Notify};
use tokio_stream::StreamExt;

/// How long to keep trying to reopen an evdev device after a stream error
/// before concluding the device is genuinely gone (USB unplug, hardware failure).
/// On suspend/resume the device typically returns within 1–2 s; 10 s is a
/// generous upper bound before escalating to a full `device_error_notify` reinit.
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Polling interval while waiting for a device to reappear after a stream error.
const RECONNECT_POLL_INTERVAL: Duration = Duration::from_millis(200);

// ── Public event type ────────────────────────────────────────────────────────

/// An event delivered by a `SteamDeckController` event task to `EventReader`.
///
/// The task reconnects transparently on suspend/resume; `EventReader` only
/// needs to handle `Reconnected` by releasing all currently held output keys.
pub enum ControllerEvent {
    /// A normal hardware input event from the evdev device.
    Input(InputEvent),
    /// The device briefly disappeared and has just come back (suspend/resume).
    /// `EventReader` must release all held output keys to avoid stuck modifiers.
    Reconnected,
}

// ── SteamDeckController ──────────────────────────────────────────────────────

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

    /// Open the device, query capabilities, and start a reconnecting event task.
    ///
    /// Returns:
    /// - `is_tablet` — true if the device supports BTN_TOOL_PEN (graphics tablet)
    /// - `max_abs_wheel` — maximum value across all absolute axes (for scroll scaling)
    /// - `event_rx` — receiver for `ControllerEvent`s; never drops unless the
    ///   device is genuinely gone (after `RECONNECT_TIMEOUT`), at which point
    ///   `device_error_notify` is fired and the channel closes.
    ///
    /// `resume_notify` is the process-level suspend/resume signal shared between
    /// `start_background_tasks` (resume_watcher fires it) and this task (listens
    /// to it for proactive reconnect, avoiding the brief stream-error window).
    pub fn start_event_task(
        &self,
        grab: bool,
        resume_notify: Arc<Notify>,
        device_error_notify: Arc<Notify>,
    ) -> (bool, i32, mpsc::Receiver<ControllerEvent>) {
        let stream = self.open_event_stream_inner(grab);

        // Query device capabilities from the initial stream before the task
        // takes ownership of it — these don't change across reconnects.
        let is_tablet = stream
            .device()
            .supported_keys()
            .unwrap_or(&evdev::AttributeSet::new())
            .contains(evdev::Key::BTN_TOOL_PEN);
        let max_abs_wheel = stream
            .device()
            .get_abs_state()
            .ok()
            .map(|s| s.iter().map(|a| a.maximum).max().unwrap_or(0))
            .unwrap_or(0);

        let (tx, rx) = mpsc::channel(64);
        let path = self.evdev_path.clone();
        tokio::spawn(reconnecting_reader_task(
            stream,
            path,
            grab,
            resume_notify,
            tx,
            device_error_notify,
        ));

        (is_tablet, max_abs_wheel, rx)
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    fn open_event_stream_inner(&self, grab: bool) -> EventStream {
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

// ── Reconnecting reader task ─────────────────────────────────────────────────

/// Reads events from `stream` and forwards them to `tx`, transparently
/// reconnecting on suspend/resume.
///
/// On `resume_notify` or stream error, waits for the device to reappear at
/// `path` (polling every `RECONNECT_POLL_INTERVAL`). Once back, sends
/// `ControllerEvent::Reconnected` so `EventReader` can release held keys.
///
/// If the device does not return within `RECONNECT_TIMEOUT`, fires
/// `device_error_notify` (triggering a full reinit in `udev_monitor`) and
/// exits — dropping `tx` closes the channel, causing `EventReader` to exit.
async fn reconnecting_reader_task(
    mut stream: EventStream,
    path: PathBuf,
    grab: bool,
    resume_notify: Arc<Notify>,
    tx: mpsc::Sender<ControllerEvent>,
    device_error_notify: Arc<Notify>,
) {
    loop {
        // ── Read phase: forward events until stream dies or resume fires ──────
        let was_proactive = loop {
            tokio::select! {
                event = stream.next() => {
                    match event {
                        Some(Ok(e)) => {
                            if tx.send(ControllerEvent::Input(e)).await.is_err() {
                                return; // EventReader dropped — process exiting
                            }
                        }
                        Some(Err(e)) => {
                            println!("makima: controller: stream error on {:?}: {} — reconnecting", path, e);
                            break false; // reactive reconnect
                        }
                        None => {
                            println!("makima: controller: stream ended on {:?} — reconnecting", path);
                            break false; // reactive reconnect
                        }
                    }
                }
                _ = resume_notify.notified() => {
                    println!("makima: controller: resume signal — proactive reconnect on {:?}", path);
                    break true; // proactive reconnect
                }
            }
        };

        // Reactive reconnect: give the kernel a moment to reset the device.
        // Proactive: try immediately — the device is likely already back.
        if !was_proactive {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }

        // ── Reconnect phase: poll until device is back or timeout ─────────────
        let deadline = tokio::time::Instant::now() + RECONNECT_TIMEOUT;
        let new_stream = loop {
            match try_open_event_stream(&path, grab) {
                Ok(s) => break Some(s),
                Err(_) => {
                    if tokio::time::Instant::now() >= deadline {
                        break None;
                    }
                    tokio::time::sleep(RECONNECT_POLL_INTERVAL).await;
                }
            }
        };

        match new_stream {
            Some(s) => {
                stream = s;
                println!("makima: controller: reconnected to {:?}", path);
                // Signal EventReader to release stuck keys before resuming.
                if tx.send(ControllerEvent::Reconnected).await.is_err() {
                    return;
                }
            }
            None => {
                // Device genuinely gone (USB unplug / hardware failure).
                eprintln!(
                    "makima: controller: {:?} did not return within {:?} — triggering full reinit",
                    path, RECONNECT_TIMEOUT
                );
                device_error_notify.notify_one();
                return; // dropping tx closes the channel → EventReader exits
            }
        }
    }
}

/// Try to open an evdev device and return an `EventStream`, or an error if
/// the device is not yet available. Non-panicking version used in the reconnect loop.
fn try_open_event_stream(path: &Path, grab: bool) -> std::io::Result<EventStream> {
    let mut device = Device::open(path)?;
    if grab {
        device.grab()?;
    }
    device.into_event_stream()
}

// ── Process-level background tasks ──────────────────────────────────────────

/// Spawn the process-level background tasks for the Steam Deck controller.
///
/// Call **once** at startup from `udev_monitor::start_monitoring_udev`.
/// Both tasks run for the lifetime of the process.
///
/// - `lizard_cfg`: parsed from `SUPPRESS_LIZARD_MODE` in `[settings]`.
///   Pass `None` to skip Lizard Mode suppression entirely.
/// - `resume_notify`: created by `udev_monitor` and shared with `launch_tasks`
///   so the reconnecting reader can proactively reconnect on resume.
///   The resume watcher fires it; the reconnecting task listens to it.
pub fn start_background_tasks(lizard_cfg: Option<LizardModeSuppression>, resume_notify: Arc<Notify>) {
    tokio::spawn(resume_watcher::start_resume_watcher(resume_notify));
    if let Some(cfg) = lizard_cfg {
        tokio::spawn(lizard_mode::run_lizard_mode_suppression(cfg, None));
    }
}

// ── Hidraw discovery ─────────────────────────────────────────────────────────

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
            if hidraw_sysfs.parent() != Some(usb_iface) {
                continue;
            }
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

    #[test]
    fn try_open_event_stream_returns_error_for_nonexistent_path() {
        let result = try_open_event_stream(Path::new("/dev/input/event99999"), false);
        assert!(result.is_err());
    }
}
