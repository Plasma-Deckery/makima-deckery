//! `deckery-controller` — Steam Deck controller library.
//!
//! Encapsulates everything specific to the Steam Deck as a physical device:
//! evdev event streaming (with suspend/resume transparency), hidraw I/O,
//! Lizard Mode suppression, click-pressure thresholds, and haptic playback.
//!
//! Higher-level concerns (input mapping, virtual devices, config routing)
//! belong in the consuming binary.
//!
//! ## Module layout
//!
//! ```text
//! lib.rs            ← SteamDeckController, ControllerSession, public API
//! hidraw.rs         ← PadFrame reader + unified writer (owns the fd)
//! haptic.rs         ← HapticChain API + player task
//! lizard_mode.rs    ← Lizard Mode suppression heartbeat helpers
//! resume_watcher.rs ← logind PrepareForSleep D-Bus watcher
//! ```
//!
//! ## Typical usage (makima path — device path already known)
//!
//! ```ignore
//! let controller = SteamDeckController::from_evdev(Path::new(&event_device));
//! let session = controller.start(grab, device_error_notify, lizard_cfg);
//! // session.event_rx             — ControllerEvent channel (suspend-transparent)
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
//! let session = controller.start(false, Arc::new(Notify::new()), None);
//! ```

pub(crate) mod grab_coordinator;
pub(crate) mod haptic;
pub(crate) mod hidraw;
pub(crate) mod lizard_mode;
pub(crate) mod resume_watcher;
pub(crate) mod yield_protocol;

// Re-export the types that consumers need so they import from here,
// not from the internal submodule. This is the stable public API surface.
pub use haptic::{HapticChain, HapticChainStep, HapticPad, HapticPulse, HapticRequest};
pub use hidraw::PadFrame;
pub use lizard_mode::LizardModeSuppression;
pub use yield_protocol::GrabbedHandle;

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

use evdev::{Device, EventStream, InputEvent};
use futures_core::Stream;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch, Notify};
use tokio_stream::StreamExt;
use libc;

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
/// Used by the consuming binary to decide whether to use the full Steam Deck
/// controller path (hidraw, Lizard Mode, haptics) or the generic evdev-only
/// path for a matched device.
pub fn is_known_device_name(name: &str) -> bool {
    KNOWN_DEVICE_NAMES.iter().any(|&known| name.contains(known))
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
    /// evdev button/axis events. Survives suspend transparently.
    /// `ControllerEvent::Reconnected` signals a resume so callers can release
    /// held keys before resuming.
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

    /// RAII guard that emits `GrabReleased` on drop via the pre-established
    /// D-Bus connection. `None` when the session was opened without grab.
    /// Keep alive for the full grab session lifetime — dropping it releases
    /// the cooperative grab lock on the D-Bus.
    pub grab_handle: Option<GrabbedHandle>,
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
    /// If `true`, the library spawns a D-Bus listener for `GrabPending` and
    /// emits `ControllerEvent::ReleaseAll` so the consumer can flush held
    /// virtual output keys before another process acquires `EVIOCGRAB`.
    ///
    /// For `grab=true` yieldable sessions: additionally releases `EVIOCGRAB`
    /// on `GrabPending` (so the requester can acquire it) and re-acquires on
    /// `GrabReleased` with the same retry/timeout logic as suspend/resume.
    ///
    /// For `grab=false` yieldable sessions (e.g. makima): only the key-flush
    /// (`ReleaseAll`) matters — the evdev stream pauses/resumes automatically,
    /// no EVIOCGRAB coordination needed.
    pub yieldable:   bool,
}

impl SteamDeckController {
    /// Construct from a known evdev device path.
    ///
    /// `yieldable`: set `true` if this session should participate in the
    /// cooperative grab protocol. See field doc for exact behaviour per
    /// `grab` value.
    ///
    /// Immediately discovers the hidraw sibling via sysfs — a synchronous read
    /// that completes in microseconds.
    pub fn from_evdev(evdev_path: &Path, yieldable: bool) -> Self {
        let hidraw_path = find_controller_hidraw_for_evdev(evdev_path);
        Self { evdev_path: evdev_path.to_path_buf(), hidraw_path, yieldable }
    }

    /// Find the Steam Deck controller by known device names (standalone path).
    ///
    /// `yieldable`: see `from_evdev`.
    ///
    /// Scans `/dev/input/event*` for a device whose name matches one of the
    /// entries in `KNOWN_DEVICE_NAMES`. Returns `None` if no match is found
    /// (non-Steam Deck hardware or device not yet connected).
    ///
    /// For makima: use `from_evdev` instead — the udev_monitor already has the
    /// path via config-file-name matching. `find()` is for callers without
    /// makima infrastructure (`deckery-auth`, standalone tools).
    pub fn find(yieldable: bool) -> Option<Self> {
        for (path, device) in evdev::enumerate() {
            if let Some(name) = device.name() {
                if is_known_device_name(name) {
                    return Some(Self::from_evdev(&path, yieldable));
                }
            }
        }
        None
    }

    /// Consume the controller and spawn all internal tasks. Returns the
    /// `ControllerSession` with caller-facing channel ends, or an `io::Error`
    /// if the device cannot be opened.
    ///
    /// When `grab=true`, the yield protocol runs first: establishes one D-Bus
    /// connection, emits `GrabPending` so any `grab=true && yieldable=true`
    /// session releases its `EVIOCGRAB`, then retries until success or timeout.
    /// Returns a `GrabbedHandle` in `session.grab_handle` — dropping it emits
    /// `GrabReleased` on the same connection.
    /// When `grab=false`, the device is opened without exclusive access; the
    /// evdev stream pauses automatically if another process holds `EVIOCGRAB`.
    ///
    /// Spawns on success:
    /// - reconnecting evdev reader (suspend-transparent `ControllerEvent` stream)
    /// - if `yieldable`: D-Bus listener for `GrabPending` → `ControllerEvent::ReleaseAll`
    /// - hidraw reader → `pad_rx`
    /// - hidraw writer (serialises haptics + Lizard Mode heartbeat onto one fd)
    pub async fn start(
        self,
        grab: bool,
        device_error_notify: Arc<Notify>,
        initial_lizard_cfg: Option<LizardModeSuppression>,
    ) -> std::io::Result<ControllerSession> {
        let (stream, grab_handle) = if grab {
            let (s, h) = yield_protocol::open_grabbed(&self.evdev_path).await?;
            println!("deckery-controller: grabbed {:?} (exclusive evdev access)", self.evdev_path);
            (s, Some(h))
        } else {
            let s = Self::open_event_stream_inner(&self.evdev_path)?;
            println!("deckery-controller: opened {:?} (no grab)", self.evdev_path);
            (s, None)
        };
        let mut session = self.spawn_tasks(stream, grab, device_error_notify, initial_lizard_cfg);
        session.grab_handle = grab_handle;
        Ok(session)
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    /// Spawn all background tasks for a session whose evdev stream is already open.
    ///
    /// Called by `start()` after the device is opened (grabbed or not).
    fn spawn_tasks(
        self,
        stream: EventStream,
        grab: bool,
        device_error_notify: Arc<Notify>,
        initial_lizard_cfg: Option<LizardModeSuppression>,
    ) -> ControllerSession {
        let resume_notify = Arc::new(Notify::new());
        tokio::spawn(resume_watcher::start_resume_watcher(resume_notify.clone()));

        let (event_tx, event_rx) = mpsc::channel(64);
        let path = self.evdev_path.clone();

        // All yieldable sessions listen for GrabPending so they can flush held
        // output keys via ControllerEvent::ReleaseAll before another process
        // acquires EVIOCGRAB — avoiding stuck virtual keys during the grab.
        //
        // grab=true yieldable sessions additionally release EVIOCGRAB on
        // GrabPending and re-grab on GrabReleased, coordinated via a dedicated
        // channel between spawn_grab_listener and reconnecting_reader_task.
        let yield_rx = if self.yieldable {
            let (yield_tx, yield_rx) = if grab {
                let (tx, rx) = mpsc::channel(4);
                (Some(tx), Some(rx))
            } else {
                (None, None)
            };
            grab_coordinator::spawn_grab_listener(
                path.to_string_lossy().into_owned(),
                event_tx.clone(),
                yield_tx,
            );
            yield_rx
        } else {
            None
        };

        tokio::spawn(reconnecting_reader_task(
            stream,
            path,
            grab,
            resume_notify,
            event_tx,
            device_error_notify,
            yield_rx,
        ));

        let (lizard_tx,         lizard_rx)         = watch::channel(initial_lizard_cfg);
        let (click_pressure_tx, click_pressure_rx) = watch::channel::<Option<ClickPressureConfig>>(None);
        let (pad_rx, haptic_tx, click_pressure) = match &self.hidraw_path {
            Some(p) => {
                let (rx, tx) = hidraw::spawn_hidraw_tasks(p.clone(), lizard_rx, click_pressure_rx);
                (Some(rx), Some(tx), Some(ClickPressureHandle(click_pressure_tx)))
            }
            None => {
                drop(lizard_rx);
                drop(click_pressure_rx);
                drop(click_pressure_tx);
                println!(
                    "deckery-controller: no hidraw sibling found for {:?} — trackpad position/haptics not available",
                    self.evdev_path
                );
                (None, None, None)
            }
        };

        ControllerSession {
            event_rx,
            pad_rx,
            haptic_tx,
            lizard_mode: LizardModeHandle(lizard_tx),
            click_pressure,
            grab_handle: None, // filled in by start() after open_grabbed
        }
    }

    fn open_event_stream_inner(evdev_path: &Path) -> std::io::Result<EventStream> {
        Device::open(evdev_path)?.into_event_stream()
    }
}

// ── Reconnecting reader task ─────────────────────────────────────────────────

/// Resolves to the next `YieldEvent` if `yield_rx` is `Some`, otherwise
/// returns a future that never resolves — effectively disabling the yield arm
/// in `tokio::select!` when the session is not `grab=true && yieldable=true`.
async fn yield_rx_recv(
    yield_rx: &mut Option<mpsc::Receiver<grab_coordinator::YieldEvent>>,
) -> Option<grab_coordinator::YieldEvent> {
    match yield_rx {
        Some(rx) => rx.recv().await,
        None     => std::future::pending().await,
    }
}

/// Returns `true` if `e` represents a busy/locked device (EBUSY or WouldBlock).
pub(crate) fn is_grab_busy(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::WouldBlock
        || e.raw_os_error() == Some(libc::EBUSY)
}

/// Public shim — delegates to the generic inner implementation with the
/// real `try_open_event_stream` as the reopen function.
///
/// See [`reconnecting_reader_task_with`] for the full documentation.
pub(crate) async fn reconnecting_reader_task(
    stream: EventStream,
    path: PathBuf,
    grab: bool,
    resume_notify: Arc<Notify>,
    tx: mpsc::Sender<ControllerEvent>,
    device_error_notify: Arc<Notify>,
    yield_rx: Option<mpsc::Receiver<grab_coordinator::YieldEvent>>,
) {
    reconnecting_reader_task_with(
        stream, path, grab, resume_notify, tx, device_error_notify, yield_rx,
        |p, g| try_open_event_stream(p, g),
    ).await
}

/// Generic core of the reconnecting reader task — the stream-reopen operation
/// is injected so it can be replaced with a mock in tests without needing real
/// evdev hardware or a uinput device.
///
/// Reads events from `stream` and forwards them to `tx`, transparently
/// reconnecting on suspend/resume.
///
/// On `resume_notify` or stream error, calls `try_reopen(path, grab)` to get a
/// new stream (polling every `RECONNECT_POLL_INTERVAL`). Once back, sends
/// `ControllerEvent::Reconnected` so the consumer can release held keys.
///
/// If `yield_rx` is `Some` (session is `grab=true && yieldable=true`):
/// - On `YieldEvent::Release` (triggered by `GrabPending`): drops the stream,
///   releasing `EVIOCGRAB`, then waits for `YieldEvent::Regrab`.
/// - On `YieldEvent::Regrab` (triggered by `GrabReleased`): re-acquires via
///   `try_reopen(path, true)` with the same retry/timeout as normal reconnect.
///
/// If `try_reopen` does not succeed within `RECONNECT_TIMEOUT`, fires
/// `device_error_notify` and exits.
pub(crate) async fn reconnecting_reader_task_with<S, F>(
    mut stream: S,
    path: PathBuf,
    grab: bool,
    resume_notify: Arc<Notify>,
    tx: mpsc::Sender<ControllerEvent>,
    device_error_notify: Arc<Notify>,
    mut yield_rx: Option<mpsc::Receiver<grab_coordinator::YieldEvent>>,
    try_reopen: F,
)
where
    S: Stream<Item = io::Result<InputEvent>> + Unpin,
    F: Fn(&Path, bool) -> io::Result<S>,
{
    use grab_coordinator::YieldEvent;

    // Reason the inner read loop broke out.
    enum BreakReason { StreamError, Resume, Yield }

    loop {
        // ── Read phase: forward events until stream dies, resume fires, or yield ──
        let break_reason = loop {
            tokio::select! {
                event = stream.next() => {
                    match event {
                        Some(Ok(e)) => {
                            if tx.send(ControllerEvent::Input(e)).await.is_err() {
                                return; // consumer dropped — process exiting
                            }
                        }
                        Some(Err(e)) => {
                            println!("deckery-controller: stream error on {:?}: {} — reconnecting", path, e);
                            break BreakReason::StreamError;
                        }
                        None => {
                            println!("deckery-controller: stream ended on {:?} — reconnecting", path);
                            break BreakReason::StreamError;
                        }
                    }
                }
                _ = resume_notify.notified() => {
                    println!("deckery-controller: resume signal — proactive reconnect on {:?}", path);
                    break BreakReason::Resume;
                }
                yield_cmd = yield_rx_recv(&mut yield_rx) => {
                    match yield_cmd {
                        Some(YieldEvent::Release) => {
                            println!(
                                "deckery-controller: GrabPending — yielding EVIOCGRAB on {:?}",
                                path
                            );
                            break BreakReason::Yield;
                        }
                        // Channel closed — disable this arm from now on.
                        None => { yield_rx = None; }
                        // Stale Regrab (shouldn't arrive here) — ignore.
                        Some(YieldEvent::Regrab) => {}
                    }
                }
            }
        };

        // ── Drop old stream BEFORE reconnecting ────────────────────────────────
        // CRITICAL: the old EventStream holds an open evdev fd. If grab=true,
        // that fd holds EVIOCGRAB. Keeping it alive while try_reopen attempts a
        // new grab would fail with EBUSY, causing every reconnect attempt to
        // silently fail for the full RECONNECT_TIMEOUT before triggering a
        // spurious full reinit.
        drop(stream);

        // ── Yield path: wait for GrabReleased, then re-acquire EVIOCGRAB ─────
        if matches!(break_reason, BreakReason::Yield) {
            // Block until the requester signals that it has released the grab.
            println!("deckery-controller: waiting for GrabReleased on {:?}", path);
            loop {
                match yield_rx_recv(&mut yield_rx).await {
                    Some(YieldEvent::Regrab) => {
                        println!("deckery-controller: GrabReleased — re-grabbing {:?}", path);
                        break;
                    }
                    Some(YieldEvent::Release) => {} // stale duplicate — ignore
                    None => return,                 // session ending
                }
            }

            // Re-acquire with retry — same pattern as the normal reconnect loop.
            let deadline = tokio::time::Instant::now() + RECONNECT_TIMEOUT;
            stream = loop {
                match try_reopen(&path, true) {
                    Ok(s) => {
                        println!("deckery-controller: re-grabbed {:?} after yield", path);
                        if tx.send(ControllerEvent::Reconnected).await.is_err() {
                            return;
                        }
                        break s;
                    }
                    Err(e) if is_grab_busy(&e) => {
                        if tokio::time::Instant::now() >= deadline {
                            eprintln!(
                                "deckery-controller: could not re-grab {:?} within {:?} \
                                 (last error: {}) — triggering reinit",
                                path, RECONNECT_TIMEOUT, e
                            );
                            device_error_notify.notify_one();
                            return;
                        }
                        tokio::time::sleep(RECONNECT_POLL_INTERVAL).await;
                    }
                    Err(e) => {
                        eprintln!(
                            "deckery-controller: re-grab of {:?} failed: {} — triggering reinit",
                            path, e
                        );
                        device_error_notify.notify_one();
                        return;
                    }
                }
            };
            continue; // back to the read loop
        }

        // ── Normal reconnect path (suspend/resume or stream error) ────────────

        // Reactive reconnect: give the kernel a moment to complete the USB
        // reset and re-enumerate the device. Proactive: the device should
        // already be back — try immediately, polling handles any brief gap.
        if matches!(break_reason, BreakReason::StreamError) {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }

        let deadline = tokio::time::Instant::now() + RECONNECT_TIMEOUT;
        stream = loop {
            match try_reopen(&path, grab) {
                Ok(s) => {
                    if grab {
                        println!("deckery-controller: reconnected to {:?} (grab re-acquired)", path);
                    } else {
                        println!("deckery-controller: reconnected to {:?}", path);
                    }
                    if tx.send(ControllerEvent::Reconnected).await.is_err() {
                        return; // consumer dropped — session ending
                    }
                    break s;
                }
                Err(e) => {
                    if tokio::time::Instant::now() >= deadline {
                        eprintln!(
                            "deckery-controller: {:?} did not return within {:?} \
                             (last error: {}) — triggering full reinit",
                            path, RECONNECT_TIMEOUT, e
                        );
                        device_error_notify.notify_one();
                        return;
                    }
                    tokio::time::sleep(RECONNECT_POLL_INTERVAL).await;
                }
            }
        };
    }
}

/// Try to open an evdev device and return an `EventStream`. Non-panicking —
/// used in the reconnect poll loop and by consumers for generic devices.
pub fn try_open_event_stream(path: &Path, grab: bool) -> std::io::Result<EventStream> {
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
        let ctrl = SteamDeckController::from_evdev(Path::new("/dev/input/event99999"), false);
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
        let _result: Option<SteamDeckController> = SteamDeckController::find(false);
    }

    // ── reconnecting_reader_task_with: yield path ────────────────────────────
    //
    // Uses a channel-backed mock stream (ReceiverStream) and a mock try_reopen
    // closure instead of real evdev hardware. Verifies that YieldEvent::Release
    // followed by YieldEvent::Regrab causes the task to call try_reopen and
    // send ControllerEvent::Reconnected — without needing a uinput device.

    use tokio_stream::wrappers::ReceiverStream;
    use grab_coordinator::YieldEvent;
    use tokio::time::{timeout, Duration};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// A mock stream backed by an mpsc channel.
    /// Dropping the sender closes the stream (stream.next() returns None).
    fn mock_stream() -> (
        tokio::sync::mpsc::Sender<io::Result<InputEvent>>,
        ReceiverStream<io::Result<InputEvent>>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        (tx, ReceiverStream::new(rx))
    }

    #[tokio::test]
    async fn yield_release_then_regrab_calls_try_reopen_and_sends_reconnected() {
        let (_stream_tx, initial_stream) = mock_stream();

        let resume_notify      = Arc::new(Notify::new());
        let device_error_notify = Arc::new(Notify::new());
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
        let (yield_tx, yield_rx)     = tokio::sync::mpsc::channel(4);

        // Flag set when try_reopen is called — confirms the re-grab attempt.
        let reopen_called = Arc::new(AtomicBool::new(false));
        let reopen_called_clone = reopen_called.clone();

        tokio::spawn(reconnecting_reader_task_with(
            initial_stream,
            PathBuf::from("/dev/input/event-mock-regrab"),
            /*grab=*/ true,
            resume_notify,
            event_tx,
            device_error_notify,
            Some(yield_rx),
            move |_path, _grab| {
                reopen_called_clone.store(true, Ordering::SeqCst);
                // Return a new mock stream that stays open.
                let (_tx, rx) = tokio::sync::mpsc::channel::<io::Result<InputEvent>>(1);
                Ok(ReceiverStream::new(rx))
            },
        ));

        // Trigger the yield: tell the task to release EVIOCGRAB.
        yield_tx.send(YieldEvent::Release).await.unwrap();

        // After releasing, send Regrab: tell the task GrabReleased, time to re-grab.
        // Brief sleep ensures the task has entered the wait-for-Regrab phase.
        tokio::time::sleep(Duration::from_millis(20)).await;
        yield_tx.send(YieldEvent::Regrab).await.unwrap();

        // try_reopen must have been called.
        timeout(Duration::from_secs(2), async {
            loop {
                if reopen_called.load(Ordering::SeqCst) { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("try_reopen not called after YieldEvent::Regrab");

        // Reconnected must have been sent to the consumer.
        let event = timeout(Duration::from_secs(2), event_rx.recv()).await
            .expect("timeout waiting for Reconnected")
            .expect("event_rx closed");
        assert!(
            matches!(event, ControllerEvent::Reconnected),
            "expected Reconnected after re-grab, got something else",
        );
    }

    #[tokio::test]
    async fn yield_regrab_busy_then_succeeds_on_retry() {
        let (_stream_tx, initial_stream) = mock_stream();

        let resume_notify       = Arc::new(Notify::new());
        let device_error_notify = Arc::new(Notify::new());
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
        let (yield_tx, yield_rx)     = tokio::sync::mpsc::channel(4);

        // First call returns EBUSY, second succeeds.
        let attempt = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let attempt_clone = attempt.clone();

        tokio::spawn(reconnecting_reader_task_with(
            initial_stream,
            PathBuf::from("/dev/input/event-mock-busy"),
            /*grab=*/ true,
            resume_notify,
            event_tx,
            device_error_notify,
            Some(yield_rx),
            move |_path, _grab| {
                let n = attempt_clone.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    Err(io::Error::from_raw_os_error(libc::EBUSY))
                } else {
                    let (_tx, rx) = tokio::sync::mpsc::channel::<io::Result<InputEvent>>(1);
                    Ok(ReceiverStream::new(rx))
                }
            },
        ));

        yield_tx.send(YieldEvent::Release).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        yield_tx.send(YieldEvent::Regrab).await.unwrap();

        // Reconnected must arrive despite the initial EBUSY.
        let event = timeout(Duration::from_secs(3), event_rx.recv()).await
            .expect("timeout — EBUSY retry did not succeed")
            .expect("event_rx closed");
        assert!(matches!(event, ControllerEvent::Reconnected));
        assert!(
            attempt.load(Ordering::SeqCst) >= 2,
            "expected at least two try_reopen calls (EBUSY + success)",
        );
    }

    #[tokio::test]
    async fn yield_regrab_non_busy_error_triggers_device_error_notify() {
        let (_stream_tx, initial_stream) = mock_stream();

        let resume_notify       = Arc::new(Notify::new());
        let device_error_notify = Arc::new(Notify::new());
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
        let (yield_tx, yield_rx)  = tokio::sync::mpsc::channel(4);

        tokio::spawn(reconnecting_reader_task_with(
            initial_stream,
            PathBuf::from("/dev/input/event-mock-error"),
            /*grab=*/ true,
            resume_notify,
            event_tx,
            device_error_notify.clone(),
            Some(yield_rx),
            |_path, _grab| -> io::Result<ReceiverStream<io::Result<InputEvent>>> {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "no permission"))
            },
        ));

        yield_tx.send(YieldEvent::Release).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        yield_tx.send(YieldEvent::Regrab).await.unwrap();

        // Non-EBUSY error must fire device_error_notify immediately.
        timeout(Duration::from_secs(2), device_error_notify.notified()).await
            .expect("device_error_notify not fired on non-EBUSY re-grab error");
    }
}
