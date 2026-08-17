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
//!   hidraw.rs         ← PadFrame reader + unified writer (owns the fd)
//!   lizard_mode.rs    ← Lizard Mode suppression heartbeat
//!   resume_watcher.rs ← logind PrepareForSleep D-Bus watcher
//! ```
//!
//! ## Usage (makima path — device path already known)
//!
//! ```ignore
//! let controller = SteamDeckController::from_evdev(Path::new(&event_device));
//! let session = controller.start(grab, device_error_notify, lizard_cfg);
//! // session.event_rx            — ControllerEvent channel (suspend-transparent)
//! // session.pad_rx              — PadFrame channel (trackpad position)
//! // session.haptic_tx           — HapticRequest channel (send chains to play haptics)
//! // session.lizard_mode.set(cfg)  — live Lizard Mode update
//! // session.click_pressure        — pass to TrackpadSession::setup()
//! // Resume watcher spawned internally — no external Notify needed
//! ```
//!
//! ## Usage (standalone path — no makima infrastructure)
//!
//! ```ignore
//! let controller = SteamDeckController::find()?;
//! // device_error_notify: pass a dead Notify if you don't need error signalling.
//! let session = controller.start(false, Arc::new(Notify::new()), None);
//! ```

pub mod haptic;
pub mod hidraw;
pub mod lizard_mode;
pub mod resume_watcher;

// Re-export the types that consumers need so they import from here,
// not from the internal submodule. This is the stable public API surface.
//
// Haptic types live in haptic.rs — HapticCommand and HidrawWrite are internal
// implementation details never exposed outside this module group.
pub use haptic::{HapticChain, HapticChainStep, HapticPad, HapticPulse, HapticRequest};
pub use hidraw::PadFrame;
pub use lizard_mode::LizardModeSuppression;

// ── ClickPressureConfig / ClickPressureHandle ────────────────────────────────

/// Physical click-pressure thresholds for the left and right trackpads.
///
/// Higher values require more force to register a physical click press.
/// `0xFFFF` effectively disables physical clicks. Configured per-side in
/// the `[trackpad.left]` / `[trackpad.right]` TOML sections as
/// `click_pressure = <u16>`.
///
/// Sent to the controller once at session start (via `ClickPressureHandle`)
/// and never re-sent by the Lizard Mode heartbeat — the controller firmware
/// retains the value until a USB reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClickPressureConfig {
    pub left:  u16,
    pub right: u16,
}

/// Setter handle for the click-pressure thresholds on a running session.
///
/// Obtained from `ControllerSession::click_pressure`. Call `set()` once (in
/// `TrackpadSession::setup`) to push the user-configured values; the writer
/// task applies the change immediately.
///
/// Dropping this handle signals the writer that no further click-pressure
/// updates will arrive — the writer simply stops listening on that arm, but
/// does NOT exit or reset the firmware value (safe to drop after the initial
/// `set()`). Store in `TrackpadSession` for the full session lifetime so
/// future dynamic updates (e.g. gaming-mode pressure changes) remain possible.
pub struct ClickPressureHandle(watch::Sender<Option<ClickPressureConfig>>);

impl ClickPressureHandle {
    /// Push a new click-pressure config. `None` is a no-op on the firmware
    /// (the writer ignores it), but can be used to "unset" a pending value
    /// before a real config arrives.
    pub fn set(&self, cfg: Option<ClickPressureConfig>) {
        // Err means the writer already exited (session teardown) — safe to ignore.
        let _ = self.0.send(cfg);
    }
}

// ── LizardModeHandle ─────────────────────────────────────────────────────────

/// Setter handle for the Lizard Mode suppression config on a running session.
///
/// Obtained from `ControllerSession::lizard_mode`. Call `set()` to update the
/// config live — the writer task applies the change on the next heartbeat cycle
/// without restarting anything.
///
/// Dropping this handle signals the writer that the session is ending, which
/// causes it to exit cleanly. The handle must therefore be kept alive for the
/// full session lifetime — store it in `EventReader` or an equivalent owner.
#[allow(dead_code)] // Public API: set() is for live config updates; field kept for future use
pub struct LizardModeHandle(watch::Sender<Option<LizardModeSuppression>>);

impl LizardModeHandle {
    /// Update the Lizard Mode suppression config. The writer task picks up the
    /// new config immediately. Pass `None` to disable suppression.
    #[allow(dead_code)] // Public API for live config updates — not yet called from makima itself
    pub fn set(&self, cfg: Option<LizardModeSuppression>) {
        // Err means the writer already exited (session teardown) — safe to ignore.
        let _ = self.0.send(cfg);
    }
}

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

/// Returns `true` if `name` matches one of the known Steam Deck device names.
///
/// Used by `udev_monitor` to decide whether to use the full Steam Deck
/// controller path (hidraw, Lizard Mode, haptics) or the generic evdev-only
/// path for a matched device.
pub fn is_known_device_name(name: &str) -> bool {
    KNOWN_DEVICE_NAMES.iter().any(|&known| name.contains(known))
}

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
    /// was found (sysfs traversal failed).
    pub pad_rx: Option<mpsc::Receiver<PadFrame>>,

    /// Haptic request channel. Send `HapticRequest { pad, chain }` to play
    /// haptic feedback; the controller evaluates the chain (including inter-step
    /// sleeps) internally. `None` if no hidraw sibling was found.
    pub haptic_tx: Option<mpsc::Sender<HapticRequest>>,

    /// Setter for live Lizard Mode config updates. Call `lizard_mode.set(cfg)`
    /// to change suppression settings without restarting the session.
    /// **Must be kept alive for the session lifetime** — dropping it signals
    /// the writer to exit. Store in `EventReader` or equivalent session owner.
    pub lizard_mode: LizardModeHandle,

    /// Setter for click-pressure thresholds. `None` if no hidraw sibling was
    /// found. Pass to `TrackpadSession::setup()` — the session stores it for
    /// its lifetime (allowing future dynamic updates) and calls `set()` once
    /// with the user-configured values.
    pub click_pressure: Option<ClickPressureHandle>,
}

// ── SteamDeckController ──────────────────────────────────────────────────────

/// Represents the physical Steam Deck controller as a lifecycle object.
///
/// Owns the device paths (evdev + hidraw) discovered at construction time.
/// Call `start()` to spawn all internal tasks and receive a `ControllerSession`.
pub struct SteamDeckController {
    pub(crate) evdev_path: PathBuf,
    /// Raw controller hidraw path. `None` on non-Steam Deck hardware or if
    /// sysfs traversal failed.
    pub(crate) hidraw_path: Option<PathBuf>,
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
    #[allow(dead_code)] // Used by standalone tools (deckery-auth) — not called from makima itself
    pub fn find() -> Option<Self> {
        for (path, device) in evdev::enumerate() {
            if let Some(name) = device.name() {
                if is_known_device_name(name) {
                    return Some(Self::from_evdev(&path));
                }
            }
        }
        None
    }

    /// Open the device and spawn all internal tasks. Returns a `ControllerSession`
    /// with the caller-facing channel ends, or `None` if the device cannot be
    /// opened (permission denied, path not found, device disappeared mid-scan).
    ///
    /// Spawns on success:
    /// - reconnecting evdev reader (suspend-transparent `ControllerEvent` stream)
    /// - hidraw reader → `pad_rx`
    /// - hidraw writer (serialises haptics + Lizard Mode heartbeat onto one fd)
    ///
    /// Spawns the resume watcher internally — no external `Notify` needed.
    /// The watcher fires on logind `PrepareForSleep(false)` and triggers a
    /// proactive evdev reconnect before the first post-suspend event arrives.
    pub fn start(
        &self,
        grab: bool,
        device_error_notify: Arc<Notify>,
        initial_lizard_cfg: Option<LizardModeSuppression>,
    ) -> Option<ControllerSession> {
        // ── evdev + suspend/resume ──
        let stream = match self.open_event_stream_inner(grab) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "makima: cannot open {:?}: {} — skipping device",
                    self.evdev_path, e
                );
                return None;
            }
        };
        // The resume watcher belongs to the controller: it is Steam-Deck-specific
        // and tightly coupled to the reconnecting reader task below.
        let resume_notify = Arc::new(Notify::new());
        tokio::spawn(resume_watcher::start_resume_watcher(resume_notify.clone()));

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

        // ── hidraw + Lizard Mode + Click Pressure ──
        // Two watch channels carry live config updates to the writer task:
        //   - lizard_tx / lizard_rx: Lizard Mode suppression (heartbeat + initial)
        //   - click_pressure_tx / click_pressure_rx: per-pad click thresholds
        // Both senders are returned as typed handles. Dropping them is safe —
        // the writer's arms become no-ops rather than causing teardown (except
        // for lizard_rx, whose Err triggers a clean writer exit).
        let (lizard_tx,         lizard_rx)         = watch::channel(initial_lizard_cfg);
        let (click_pressure_tx, click_pressure_rx) = watch::channel::<Option<ClickPressureConfig>>(None);
        let (pad_rx, haptic_tx, click_pressure) = match &self.hidraw_path {
            Some(p) => {
                let (rx, tx) = hidraw::spawn_hidraw_tasks(p.clone(), lizard_rx, click_pressure_rx);
                (Some(rx), Some(tx), Some(ClickPressureHandle(click_pressure_tx)))
            }
            None => {
                // No hidraw — drop both receivers; handles become no-ops.
                drop(lizard_rx);
                drop(click_pressure_rx);
                drop(click_pressure_tx);
                println!(
                    "makima: no hidraw sibling found for {:?} — trackpad position/haptics not available",
                    self.evdev_path
                );
                (None, None, None)
            }
        };

        Some(ControllerSession {
            event_rx,
            pad_rx,
            haptic_tx,
            lizard_mode: LizardModeHandle(lizard_tx),
            click_pressure,
        })
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    fn open_event_stream_inner(&self, grab: bool) -> std::io::Result<EventStream> {
        let mut device = Device::open(&self.evdev_path)?;
        if grab {
            device.grab()?;
        }
        device.into_event_stream()
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
pub(crate) async fn reconnecting_reader_task(
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
/// used in the reconnect poll loop and by `udev_monitor` for generic devices.
pub(crate) fn try_open_event_stream(path: &Path, grab: bool) -> std::io::Result<EventStream> {
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
