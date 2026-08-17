//! Steam Deck controller lifecycle — evdev stream, hidraw I/O, Lizard Mode
//! suppression, and suspend/resume watching.
//!
//! This module is the single point of contact for everything that is specific
//! to the Steam Deck as a physical device. Higher-level concerns (input
//! mapping, virtual devices, config routing) remain in `udev_monitor`,
//! `event_reader`, etc.
//!
//! ## Module layout
//!
//! ```
//! steam_deck_controller/
//!   mod.rs            ← SteamDeckController, ControllerSession, public API
//!   hidraw.rs         ← PadFrame reader + HidrawWrite writer (owns the fd)
//!   lizard_mode.rs    ← Lizard Mode suppression heartbeat
//!   resume_watcher.rs ← logind PrepareForSleep D-Bus watcher
//! ```
//!
//! ## Usage (makima path — device path already known)
//!
//! ```ignore
//! let resume_notify = Arc::new(Notify::new());
//! tokio::spawn(resume_watcher::start_resume_watcher(resume_notify.clone()));
//!
//! let controller = SteamDeckController::from_evdev(Path::new(&event_device));
//! let session = controller.start(grab, resume_notify, device_error_notify, lizard_cfg);
//! // session.event_rx  — ControllerEvent channel (suspend-transparent)
//! // session.pad_rx    — PadFrame channel (trackpad position)
//! // session.hidraw_tx — HidrawWrite channel (haptics, settings)
//! // session.lizard_tx — live Lizard Mode config
//! ```
//!
//! ## Usage (standalone path — no makima infrastructure)
//!
//! ```ignore
//! let controller = SteamDeckController::find()?;
//! let session = controller.start(false, Arc::new(Notify::new()), Arc::new(Notify::new()), None);
//! ```

pub mod hidraw;
pub mod lizard_mode;
pub mod resume_watcher;

// Re-export the types that consumers need so they import from here,
// not from the internal submodule. This is the stable public API surface.
pub use hidraw::{HapticCommand, HapticPad, HidrawWrite, PadFrame};
pub use lizard_mode::LizardModeSuppression;

use evdev::{Device, EventStream, InputEvent};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch, Notify};
use tokio_stream::StreamExt;

/// How long to keep trying to reopen an evdev device after a stream error
/// before concluding the device is genuinely gone.
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Polling interval while waiting for a device to reappear.
const RECONNECT_POLL_INTERVAL: Duration = Duration::from_millis(200);

// ── Known Steam Deck device names ─────────────────────────────────────────────

/// Device names used to identify the Steam Deck controller in evdev enumeration.
/// Used by `SteamDeckController::find()` for callers that don't have a udev
/// path — e.g. `deckery-auth`, which has no config-file-name matching.
const KNOWN_DEVICE_NAMES: &[&str] = &[
    "Steam Deck",
    "Valve Software Steam Controller",
];

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

// ── ControllerSession ────────────────────────────────────────────────────────

/// All channels for one active controller session.
///
/// Returned by `SteamDeckController::start()`. The controller owns all
/// background tasks; this struct holds the caller-facing ends of their channels.
pub struct ControllerSession {
    /// evdev button/axis events. Survives suspend transparently.
    /// `ControllerEvent::Reconnected` signals a resume so callers can release
    /// held keys before resuming — `EventReader` calls `release_all_held()`.
    pub event_rx: mpsc::Receiver<ControllerEvent>,

    /// Raw trackpad position frames from hidraw. `None` if no hidraw sibling
    /// was found (non-Steam Deck hardware or sysfs traversal failed).
    pub pad_rx: Option<mpsc::Receiver<PadFrame>>,

    /// Unified hidraw write channel. Send `HidrawWrite::Haptic(cmd)` for
    /// haptic pulses; future `HidrawWrite::Settings(...)` for firmware config.
    /// `None` if no hidraw sibling was found.
    pub hidraw_tx: Option<mpsc::Sender<HidrawWrite>>,

    /// Live Lizard Mode configuration. Send `None` to disable the heartbeat,
    /// `Some(cfg)` to enable or change timing/reports without restarting tasks.
    pub lizard_tx: watch::Sender<Option<LizardModeSuppression>>,
}

// ── SteamDeckController ──────────────────────────────────────────────────────

/// Represents the physical Steam Deck controller as a lifecycle object.
///
/// Owns the device paths (evdev + hidraw) discovered at construction time.
/// Call `start()` to spawn all internal tasks and receive a `ControllerSession`.
pub struct SteamDeckController {
    pub evdev_path: PathBuf,
    /// Raw controller hidraw path. `None` on non-Steam Deck hardware or if
    /// sysfs traversal failed.
    pub hidraw_path: Option<PathBuf>,
}

impl SteamDeckController {
    /// Construct from a known evdev device path (makima path).
    ///
    /// Immediately discovers the hidraw sibling via sysfs — a synchronous read
    /// that completes in microseconds.
    pub fn from_evdev(evdev_path: &Path) -> Self {
        let hidraw_path = find_controller_hidraw_for_evdev(evdev_path);
        Self { evdev_path: evdev_path.to_path_buf(), hidraw_path }
    }

    /// Find the Steam Deck controller by known device names (standalone path).
    ///
    /// Scans `/dev/input/event*` for a device whose name matches one of the
    /// entries in `KNOWN_DEVICE_NAMES`. Returns `None` if no match is found
    /// (non-Steam Deck hardware or device not yet connected).
    ///
    /// For makima: use `from_evdev` instead — the udev_monitor already has the
    /// path via config-file-name matching. `find()` is for callers without
    /// makima infrastructure (`deckery-auth`, standalone tools).
    pub fn find() -> Option<Self> {
        for (path, device) in evdev::enumerate() {
            if let Some(name) = device.name() {
                if KNOWN_DEVICE_NAMES.iter().any(|&known| name.contains(known)) {
                    return Some(Self::from_evdev(&path));
                }
            }
        }
        None
    }

    /// Open the device and spawn all internal tasks. Returns a `ControllerSession`
    /// with the caller-facing channel ends.
    ///
    /// Spawns:
    /// - reconnecting evdev reader (suspend-transparent `ControllerEvent` stream)
    /// - hidraw reader → `pad_rx`
    /// - hidraw writer ← `hidraw_tx` (serializes haptics, Lizard Mode reports, …)
    /// - Lizard Mode heartbeat task (driven by `lizard_tx` watch channel)
    ///
    /// `resume_notify` is fired by `resume_watcher::start_resume_watcher` (call
    /// that separately, once per process). Passing `Arc::new(Notify::new())` is
    /// safe but disables proactive reconnect — the task still reconnects reactively
    /// on stream errors.
    pub fn start(
        &self,
        grab: bool,
        resume_notify: Arc<Notify>,
        device_error_notify: Arc<Notify>,
        initial_lizard_cfg: Option<LizardModeSuppression>,
    ) -> ControllerSession {
        // ── evdev ──
        let stream = self.open_event_stream_inner(grab);
        let (event_tx, event_rx) = mpsc::channel(64);
        let path = self.evdev_path.clone();
        tokio::spawn(reconnecting_reader_task(
            stream,
            path,
            grab,
            resume_notify,
            event_tx,
            device_error_notify,
        ));

        // ── hidraw ──
        let (pad_rx, hidraw_tx) = match &self.hidraw_path {
            Some(p) => {
                let (rx, tx) = hidraw::spawn_hidraw_tasks(p.clone());
                (Some(rx), Some(tx))
            }
            None => {
                println!(
                    "makima: no hidraw sibling found for {:?} — trackpad position/haptics not available",
                    self.evdev_path
                );
                (None, None)
            }
        };

        // ── Lizard Mode ──
        let (lizard_tx, lizard_rx) = watch::channel(initial_lizard_cfg);
        if let Some(tx) = hidraw_tx.clone() {
            tokio::spawn(lizard_mode::run_lizard_mode_suppression(lizard_rx, tx));
        }

        ControllerSession { event_rx, pad_rx, hidraw_tx, lizard_tx }
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    fn open_event_stream_inner(&self, grab: bool) -> EventStream {
        let mut device = Device::open(&self.evdev_path)
            .expect("Couldn't open device path.");
        if grab {
            device.grab()
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
                            break false;
                        }
                        None => {
                            println!("makima: controller: stream ended on {:?} — reconnecting", path);
                            break false;
                        }
                    }
                }
                _ = resume_notify.notified() => {
                    println!("makima: controller: resume signal — proactive reconnect on {:?}", path);
                    break true;
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
                if tx.send(ControllerEvent::Reconnected).await.is_err() {
                    return;
                }
            }
            None => {
                eprintln!(
                    "makima: controller: {:?} did not return within {:?} — triggering full reinit",
                    path, RECONNECT_TIMEOUT
                );
                device_error_notify.notify_one();
                return;
            }
        }
    }
}

/// Try to open an evdev device and return an `EventStream`. Non-panicking —
/// used in the reconnect poll loop.
fn try_open_event_stream(path: &Path, grab: bool) -> std::io::Result<EventStream> {
    let mut device = Device::open(path)?;
    if grab { device.grab()?; }
    device.into_event_stream()
}

// ── Hidraw discovery ─────────────────────────────────────────────────────────

/// Find the raw controller hidraw sibling for a known evdev device path.
///
/// Combines USB-interface sysfs traversal with the no-`input/`-subdir filter
/// to distinguish the raw controller channel from the kb/mouse emulation nodes.
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
            if hidraw_sysfs.parent() != Some(usb_iface) { continue; }
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

    #[test]
    fn find_does_not_panic() {
        // find() scans live evdev nodes — result is hardware-dependent.
        // This test only verifies it doesn't panic and returns a consistent type.
        let _result: Option<SteamDeckController> = SteamDeckController::find();
    }
}
