//! `deckery-controller` — Steam Deck controller library.
//!
//! Encapsulates everything specific to the Steam Deck as a physical device:
//! hidraw I/O, Lizard Mode suppression, click-pressure thresholds, and haptic
//! playback.
//!
//! Higher-level concerns (input mapping, virtual devices, config routing)
//! belong in the consuming binary.
//!
//! ## Backend
//!
//! Steam Deck controller I/O is hidraw-only (see issue #47 for why the old
//! evdev fallback backend and its `EVIOCGRAB`/D-Bus grab-yield protocol were
//! removed). `SteamDeckController::start()` fails if no hidraw sibling is
//! found for the evdev device it was constructed from.
//!
//! ## Module layout
//!
//! ```text
//! lib.rs            ← SteamDeckController, ControllerSession, public API
//! hidraw.rs         ← PadFrame reader + unified writer (owns the fd)
//! hid_report.rs     ← raw HID report → ControllerEvent::Input synthesis
//! haptic.rs         ← HapticChain API + player task
//! lizard_mode.rs    ← Lizard Mode suppression heartbeat helpers
//! ```
//!
//! ## Typical usage (makima path — device path already known)
//!
//! ```ignore
//! let controller = SteamDeckController::from_evdev(Path::new(&event_device));
//! let session = controller.start(device_error_notify, lizard_cfg).await?;
//! // session.event_rx             — ControllerEvent channel
//! // session.pad_rx               — PadFrame channel (trackpad position)
//! // session.haptic_tx            — HapticRequest channel (haptic playback)
//! // session.lizard_mode.set(cfg) — live Lizard Mode update
//! // session.click_pressure       — pass to TrackpadSession::setup()
//! ```
//!
//! ## Standalone usage (no makima infrastructure)
//!
//! ```ignore
//! let controller = SteamDeckController::find()?;
//! let session = controller.start(Arc::new(Notify::new()), None).await?;
//! ```

pub(crate) mod haptic;
pub(crate) mod hid_report;
pub(crate) mod hidraw;
pub(crate) mod lizard_mode;

// Re-export the types that consumers need so they import from here,
// not from the internal submodule. This is the stable public API surface.
pub use haptic::{HapticChain, HapticChainStep, HapticPad, HapticPulse, HapticRequest};
pub use hidraw::PadFrame;
pub use lizard_mode::LizardModeSuppression;

// ── Internal startup timing ───────────────────────────────────────────────────

/// Milliseconds elapsed since the first call to this function.
///
/// Used for "+Nms since startup" log markers in the library's internal tasks.
/// The timer starts on first call — in practice this is during
/// `spawn_hidraw_tasks`, so it closely tracks the binary's own startup time
/// when the binary's `main()` calls the controller early in init.
pub(crate) fn startup_ms() -> u128 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis()
}

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
/// Lifetime guard for the Lizard Mode writer channel.
///
/// Obtained from `ControllerSession::lizard_mode`. Store it in `EventReader`
/// (or equivalent session owner) for the full session lifetime — dropping it
/// closes the watch channel and signals the hidraw writer to exit cleanly.
///
/// Call `set(cfg)` to push a live Lizard Mode config update without restarting
/// the session.
pub struct LizardModeHandle(watch::Sender<Option<LizardModeSuppression>>);

impl LizardModeHandle {
    /// Update the Lizard Mode suppression config live. The hidraw writer picks
    /// up the new value immediately — no session restart needed.
    /// Pass `None` to disable suppression entirely.
    pub fn set(&self, cfg: Option<LizardModeSuppression>) {
        // Err = writer already exited (session teardown) — safe to ignore.
        let _ = self.0.send(cfg);
    }
}

use evdev::InputEvent;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Notify};

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
/// Used by the consuming binary to decide whether to use the full Steam Deck
/// controller path (hidraw, Lizard Mode, haptics) or the generic evdev-only
/// path for a matched device.
pub fn is_known_device_name(name: &str) -> bool {
    KNOWN_DEVICE_NAMES.iter().any(|&known| name.contains(known))
}

/// Canonical config-file name for all hid-steam devices (Steam Deck, Steam Controller).
///
/// Config files for these devices should use this name as their filename prefix,
/// independent of the kernel-reported evdev name. This decouples config naming
/// from kernel-version-dependent device names ("Steam Deck" on one kernel,
/// "Valve Software Steam Controller" on another).
pub const CANONICAL_STEAM_DECK_CONFIG_NAME: &str = "Steam Deck";

/// Returns the canonical config-file name for a hid-steam device, or `None`
/// if `evdev_name` is not a known hid-steam device.
///
/// Use this to resolve which config file applies to a device regardless of the
/// name the current kernel version reports. The canonical name is stable:
/// user config files should be named after it, not after the raw evdev name.
///
/// # Examples
/// ```
/// assert_eq!(
///     deckery_controller::canonical_device_name("Valve Software Steam Controller"),
///     Some("Steam Deck"),
/// );
/// assert_eq!(
///     deckery_controller::canonical_device_name("Some Random Gamepad"),
///     None,
/// );
/// ```
pub fn canonical_device_name(evdev_name: &str) -> Option<&'static str> {
    if is_known_device_name(evdev_name) {
        Some(CANONICAL_STEAM_DECK_CONFIG_NAME)
    } else {
        None
    }
}

// ── Public event type ────────────────────────────────────────────────────────

/// An event delivered by a `SteamDeckController` event task to the consumer.
///
/// The task reconnects transparently on suspend/resume; the consumer only
/// needs to handle `Reconnected` by releasing all currently held output keys.
pub enum ControllerEvent {
    /// A normal hardware input event from the evdev device.
    Input(InputEvent),
    /// The device briefly disappeared and has just come back (suspend/resume).
    /// The consumer must release all held output keys to avoid stuck modifiers.
    Reconnected,
    /// Another process is about to grab the device exclusively.
    /// The consumer must release all held output keys immediately so they do
    /// not remain stuck while the grab is active.
    ReleaseAll,
}

// ── ControllerSession ────────────────────────────────────────────────────────

/// All channels for one active controller session.
///
/// Returned by `SteamDeckController::start()`. The controller owns all
/// background tasks; this struct holds the caller-facing ends of their channels.
pub struct ControllerSession {
    /// Button/axis events, synthesised from the hidraw stream.
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
    /// the writer to exit.
    pub lizard_mode: LizardModeHandle,

    /// Setter for click-pressure thresholds. `None` if no hidraw sibling was
    /// found. Pass to your session setup — the session stores it for its
    /// lifetime (allowing future dynamic updates) and calls `set()` once with
    /// the user-configured values.
    pub click_pressure: Option<ClickPressureHandle>,
}

// ── SteamDeckController ──────────────────────────────────────────────────────

/// Represents the physical Steam Deck controller as a lifecycle object.
///
/// Owns the device paths (evdev + hidraw) discovered at construction time.
/// Call `start()` to spawn all internal tasks and receive a `ControllerSession`.
pub struct SteamDeckController {
    pub evdev_path:  PathBuf,
    /// Raw controller hidraw path. `None` on non-Steam Deck hardware or if
    /// sysfs traversal failed.
    pub hidraw_path: Option<PathBuf>,
}

impl SteamDeckController {
    /// Construct from a known evdev device path.
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
                if is_known_device_name(name) {
                    return Some(Self::from_evdev(&path));
                }
            }
        }
        None
    }

    /// Consume the controller and spawn all internal tasks. Returns the
    /// `ControllerSession` with caller-facing channel ends, or an `io::Error`
    /// if no hidraw sibling was found for the evdev device (see issue #47 —
    /// the evdev-only fallback backend was removed as dead code, since
    /// hid-steam always exposes a hidraw sibling on real hardware).
    pub async fn start(
        self,
        device_error_notify: Arc<Notify>,
        initial_lizard_cfg: Option<LizardModeSuppression>,
    ) -> std::io::Result<ControllerSession> {
        let hidraw_path = self.hidraw_path.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no hidraw sibling found for {:?}", self.evdev_path),
            )
        })?;

        let (event_tx, event_rx) = mpsc::channel(64);
        let (lizard_tx,         lizard_rx)         = watch::channel(initial_lizard_cfg);
        let (click_pressure_tx, click_pressure_rx) = watch::channel::<Option<ClickPressureConfig>>(None);

        let (pad_rx, haptic_tx) = hidraw::spawn_hidraw_tasks(
            hidraw_path,
            lizard_rx,
            click_pressure_rx,
            Some(event_tx),
            Some(device_error_notify),
        );

        Ok(ControllerSession {
            event_rx,
            pad_rx:         Some(pad_rx),
            haptic_tx:      Some(haptic_tx),
            lizard_mode:    LizardModeHandle(lizard_tx),
            click_pressure: Some(ClickPressureHandle(click_pressure_tx)),
        })
    }
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
    // Go up four levels: inputN → input/ → HID_A/ → usb_iface/ → usb_device/
    //
    // We compare at the USB-device level (one above the USB interface) so that
    // the raw-controller hidraw (which may be under a *different* USB interface
    // than the evdev node) is still recognised as a sibling.  This matters in
    // QEMU passthrough (usb-host / USB-IP) where each HID interface is exposed
    // as its own USB interface, whereas on real hardware they all live under the
    // same interface — both layouts resolve correctly at the device level.
    let usb_device = evdev_sysfs.parent()?.parent()?.parent()?.parent()?;

    let mut candidates = Vec::new();
    for entry in std::fs::read_dir("/sys/class/hidraw/").ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name().to_string_lossy().to_string();
        if let Ok(hidraw_sysfs) = std::fs::canonicalize(
            format!("/sys/class/hidraw/{}/device", name)
        ) {
            if hidraw_sysfs.parent()?.parent() != Some(usb_device) { continue; }
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
    fn find_does_not_panic() {
        // find() scans live evdev nodes — result is hardware-dependent.
        // This test only verifies it doesn't panic and returns a consistent type.
        let _result: Option<SteamDeckController> = SteamDeckController::find();
    }

    #[tokio::test]
    async fn start_fails_when_no_hidraw_sibling_found() {
        let ctrl = SteamDeckController::from_evdev(Path::new("/dev/input/event99999"));
        let err = ctrl.start(Arc::new(Notify::new()), None).await
            .err().expect("expected an error when no hidraw sibling is found");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
