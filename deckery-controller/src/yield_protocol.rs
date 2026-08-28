// ── Cooperative grab yield protocol ──────────────────────────────────────────
//
// Protocol for acquiring an exclusive evdev grab without blocking other
// deckery-controller consumers indefinitely:
//
//  1. Establish one D-Bus connection for the entire grab session.
//  2. Emit `GrabPending` — yieldable grab=true sessions release EVIOCGRAB;
//     all yieldable sessions flush held output keys via ReleaseAll.
//  3. Retry EVIOCGRAB until success or timeout.
//  4. Return a `GrabbedHandle` that holds the connection.
//     Dropping it emits `GrabReleased` on the same connection — no new
//     handshake, no new auth round-trip.
//
// The connection is established once so signal emission is a single round-trip
// on an already-authenticated socket, taking microseconds rather than the
// full connect+handshake overhead of a fresh connection.

use std::io;
use std::path::Path;
use std::time::Duration;
use evdev::EventStream;
use zbus::Connection;

use crate::grab_coordinator;

/// How long to keep retrying EVIOCGRAB before giving up.
const GRAB_TIMEOUT: Duration = Duration::from_secs(5);

/// Interval between EVIOCGRAB attempts.
const GRAB_RETRY_INTERVAL: Duration = Duration::from_millis(100);

// ── GrabbedHandle ─────────────────────────────────────────────────────────────

/// RAII guard for an active evdev grab acquired via the yield protocol.
///
/// Holds the D-Bus connection that was used to emit `GrabPending` so
/// `GrabReleased` can be sent on the **same** connection when the guard is
/// dropped — no new D-Bus handshake required.
///
/// Obtained from [`open_grabbed`]. The evdev [`EventStream`] is returned
/// separately (it lives in the reconnecting reader task); this guard only
/// owns the D-Bus side of the session.
#[derive(Debug)]
pub struct GrabbedHandle {
    /// Pre-established connection; `None` if D-Bus was unavailable at grab time.
    conn: Option<Connection>,
    path: String,
}

impl Drop for GrabbedHandle {
    /// Emit `GrabReleased` on the pre-established connection.
    ///
    /// Uses `tokio::runtime::Handle::try_current()` so a missing runtime
    /// (e.g. during process shutdown or in sync tests) is handled gracefully
    /// rather than panicking.
    fn drop(&mut self) {
        let Some(conn) = self.conn.take() else { return };
        let path = self.path.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    grab_coordinator::emit_signal_on(&conn, "GrabReleased", &path).await;
                });
            }
            Err(_) => {
                eprintln!(
                    "deckery-controller: GrabbedHandle dropped outside Tokio runtime \
                     — GrabReleased not emitted for {:?}",
                    path
                );
            }
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Acquire an exclusive evdev grab using the yield protocol.
///
/// 1. Establishes one D-Bus connection for the session.
/// 2. Emits `GrabPending` immediately on that connection — yieldable grab=true
///    sessions release their EVIOCGRAB; all yieldable sessions flush held keys.
/// 3. Retries `EVIOCGRAB` every [`GRAB_RETRY_INTERVAL`] until success or
///    [`GRAB_TIMEOUT`] is reached.
///
/// Returns `(stream, handle)` on success. `handle` is a [`GrabbedHandle`]
/// that emits `GrabReleased` (on the same connection) when dropped.
/// Store it for the lifetime of the grab; dropping it signals release.
///
/// If D-Bus is unavailable, the grab proceeds without notification and
/// `handle.conn` is `None` — `Drop` is a no-op in that case.
pub async fn open_grabbed(path: &Path) -> io::Result<(EventStream, GrabbedHandle)> {
    open_grabbed_with(path, |p| crate::try_open_event_stream(p, true)).await
}

/// Generic core of the yield protocol — the grab operation is injected so
/// it can be replaced with a mock in tests without needing real evdev devices.
///
/// `try_grab(path)` must return `Err` with `EBUSY` / `WouldBlock` when the
/// device is held by another process, and `Ok(stream)` on success. Any other
/// error aborts the retry loop immediately.
pub(crate) async fn open_grabbed_with<S>(
    path: &Path,
    try_grab: impl Fn(&Path) -> io::Result<S>,
) -> io::Result<(S, GrabbedHandle)> {
    let path_str = path.to_str().unwrap_or("");

    // ── 1. Establish D-Bus connection once for this grab session ──────────────
    let conn = grab_coordinator::connect().await.ok();

    // ── 2. Notify yieldable sessions upfront ─────────────────────────────────
    // grab=true yieldable sessions must release their EVIOCGRAB before we can
    // acquire it. All yieldable sessions flush held virtual output keys.
    if let Some(ref c) = conn {
        grab_coordinator::emit_signal_on(c, "GrabPending", path_str).await;
    }

    // ── 3. Retry until success or timeout ─────────────────────────────────────
    let deadline = tokio::time::Instant::now() + GRAB_TIMEOUT;
    loop {
        match try_grab(path) {
            Ok(stream) => {
                let handle = GrabbedHandle { conn, path: path_str.to_string() };
                return Ok((stream, handle));
            }
            Err(e) if crate::is_grab_busy(&e) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("could not grab {:?} within {:?}: {e}", path, GRAB_TIMEOUT),
                    ));
                }
                tokio::time::sleep(GRAB_RETRY_INTERVAL).await;
            }
            Err(e) => return Err(e),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio::time::{timeout, Duration};

    use crate::grab_coordinator::{connect, spawn_grab_listener, YieldEvent};
    use crate::ControllerEvent;

    // Unique device paths so parallel tests on the same session bus don't
    // interfere with each other through the path filter.
    const DEV_HANDOFF:        &str = "/dev/input/event-yield-handoff";
    const DEV_RELEASED:       &str = "/dev/input/event-yield-released";
    const DEV_TIMEOUT:        &str = "/dev/input/event-yield-timeout";
    const DEV_NO_DBUS:        &str = "/dev/input/event-yield-nodbus";
    const DEV_REGRAB_NOTIFY:  &str = "/dev/input/event-yield-regrab-notify";
    const DEV_REGRAB_NO_LOOP: &str = "/dev/input/event-yield-regrab-no-loop";

    /// Simulate a yieldable session: subscribe to GrabPending, release the
    /// mock grab (flip the AtomicBool) when ReleaseAll arrives.
    async fn spawn_yieldable(device: &str, grabbed: Arc<AtomicBool>) -> mpsc::Receiver<ControllerEvent> {
        let (tx, rx) = mpsc::channel(4);
        spawn_grab_listener(device.to_string(), tx, None);

        // Simulate the EVIOCGRAB release when GrabPending arrives.
        // In production this is done by reconnecting_reader_task dropping
        // the EventStream; here we just flip the bool.
        let grabbed_clone = grabbed.clone();
        let device = device.to_string();
        tokio::spawn(async move {
            // We need a second channel consumer — use a separate listener for
            // the AtomicBool side, driven by the same D-Bus signal.
            let conn = connect().await.expect("session bus unavailable");
            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface("org.Deckery.Controller1").unwrap()
                .path("/org/Deckery/Controller1").unwrap()
                .build();
            let mut stream = zbus::MessageStream::for_match_rule(rule, &conn, None)
                .await
                .expect("subscribe failed");
            use tokio_stream::StreamExt as _;
            while let Some(Ok(msg)) = stream.next().await {
                let member = msg.header().member().map(|m| m.to_string());
                let body = msg.body();
                let Ok(path) = body.deserialize::<&str>() else { continue };
                if path != device { continue }
                if member.as_deref() == Some("GrabPending") {
                    grabbed_clone.store(false, Ordering::SeqCst);
                    break;
                }
            }
        });

        // Give listener time to subscribe before the test emits signals.
        tokio::time::sleep(Duration::from_millis(50)).await;
        rx
    }

    /// Mock grab closure: returns EBUSY while `grabbed` is true, Ok(()) after.
    fn mock_grab(grabbed: Arc<AtomicBool>) -> impl Fn(&Path) -> io::Result<()> {
        move |_path| {
            if grabbed.load(Ordering::SeqCst) {
                Err(io::Error::from_raw_os_error(libc::EBUSY))
            } else {
                Ok(())
            }
        }
    }

    // ── Test 1: Full handoff ──────────────────────────────────────────────────

    /// Requester grabs after yieldable session releases on GrabPending.
    #[tokio::test]
    async fn requester_gets_grab_after_yieldable_releases() {
        let grabbed = Arc::new(AtomicBool::new(true));
        let mut rx = spawn_yieldable(DEV_HANDOFF, grabbed.clone()).await;

        let (_unit, _handle) = open_grabbed_with(
            Path::new(DEV_HANDOFF),
            mock_grab(grabbed),
        ).await.expect("open_grabbed_with should succeed");

        // Yieldable session must have received ReleaseAll.
        let event = timeout(Duration::from_secs(2), rx.recv()).await
            .expect("timeout waiting for ReleaseAll")
            .expect("channel closed");
        assert!(matches!(event, ControllerEvent::ReleaseAll));
    }

    // ── Test 2: GrabbedHandle drop emits GrabReleased ────────────────────────

    /// Dropping the handle emits GrabReleased on the pre-established connection.
    #[tokio::test]
    async fn grabbed_handle_drop_emits_grab_released() {
        // Subscribe a raw D-Bus listener for GrabReleased.
        let conn = connect().await.expect("session bus unavailable");
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.Deckery.Controller1").unwrap()
            .path("/org/Deckery/Controller1").unwrap()
            .build();
        let mut stream = zbus::MessageStream::for_match_rule(rule, &conn, None)
            .await
            .expect("subscribe failed");
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Acquire a handle via open_grabbed_with (device never busy).
        let grabbed = Arc::new(AtomicBool::new(false));
        let (_unit, handle) = open_grabbed_with(
            Path::new(DEV_RELEASED),
            mock_grab(grabbed),
        ).await.expect("open_grabbed_with should succeed");

        // Drop the handle — should emit GrabReleased.
        drop(handle);

        // Wait for GrabReleased on the bus.
        use tokio_stream::StreamExt as _;
        let received = timeout(Duration::from_secs(2), async {
            while let Some(Ok(msg)) = stream.next().await {
                let member = msg.header().member().map(|m| m.to_string());
                let body = msg.body();
                let Ok(path) = body.deserialize::<&str>() else { continue };
                if path == DEV_RELEASED && member.as_deref() == Some("GrabReleased") {
                    return true;
                }
            }
            false
        }).await;

        assert!(matches!(received, Ok(true)), "GrabReleased not received after handle drop");
    }

    // ── Test 3: Non-EBUSY errors propagate immediately ───────────────────────

    /// Errors other than EBUSY/WouldBlock abort the retry loop immediately —
    /// there is no point waiting for a yield if the device is outright
    /// inaccessible (permission denied, not found, etc.).
    #[tokio::test]
    async fn non_busy_error_propagates_without_retry() {
        let result = open_grabbed_with(
            Path::new(DEV_TIMEOUT),
            |_| Err::<(), _>(io::Error::new(io::ErrorKind::PermissionDenied, "no permission")),
        ).await;

        assert_eq!(
            result.unwrap_err().kind(),
            io::ErrorKind::PermissionDenied,
        );
    }

    // ── Test 5: Re-grab notifies other yieldable sessions ────────────────────

    /// Multi-participant: a grab=false+yieldable observer receives ReleaseAll
    /// twice — once on the requester's initial GrabPending, and again when
    /// the grab=true+yieldable session emits GrabPending before re-grabbing.
    ///
    /// Participants:
    ///   - grab=false+yieldable (obs_rx): collects ReleaseAll events
    ///   - grab=true+yieldable (yield_rx): releases mock grab on Release,
    ///     acknowledges Regrab
    ///   - requester: open_grabbed_with, then drops GrabbedHandle
    #[tokio::test]
    async fn regrab_notifies_other_yieldable_sessions() {
        // grab=false+yieldable observer: only ReleaseAll events, no yield coordination.
        let (obs_tx, mut obs_rx) = mpsc::channel(8);
        spawn_grab_listener(DEV_REGRAB_NOTIFY.to_string(), obs_tx, None);

        // grab=true+yieldable session: has yield coordination.
        let (dummy_tx, _dummy_rx) = mpsc::channel(4);
        let (yield_tx, mut yield_rx) = mpsc::channel(4);
        spawn_grab_listener(DEV_REGRAB_NOTIFY.to_string(), dummy_tx, Some(yield_tx));

        // Simulated EVIOCGRAB state: starts held (true = grabbed).
        let grabbed = Arc::new(AtomicBool::new(true));
        let grabbed_for_task = grabbed.clone();

        // Drive the grab=true+yieldable session: Release → drop mock grab.
        tokio::spawn(async move {
            while let Some(cmd) = yield_rx.recv().await {
                if matches!(cmd, YieldEvent::Release) {
                    grabbed_for_task.store(false, Ordering::SeqCst);
                }
                // YieldEvent::Regrab: reconnecting_reader_task would re-grab here;
                // in this test there is no reader task, so we just consume it.
            }
        });

        tokio::time::sleep(Duration::from_millis(100)).await; // listeners subscribe

        // Requester acquires the grab (after yieldable releases).
        let (_, handle) = open_grabbed_with(
            Path::new(DEV_REGRAB_NOTIFY),
            mock_grab(grabbed),
        ).await.expect("requester should grab after yieldable releases");

        // First ReleaseAll — from the requester's initial GrabPending.
        let ev1 = timeout(Duration::from_secs(2), obs_rx.recv()).await
            .expect("timeout waiting for first ReleaseAll")
            .expect("channel closed");
        assert!(matches!(ev1, ControllerEvent::ReleaseAll), "expected first ReleaseAll");

        // Requester releases: GrabReleased → grab=true+yieldable listener
        // emits GrabPending (re-grab notification) → observer gets ReleaseAll again.
        drop(handle);

        let ev2 = timeout(Duration::from_secs(2), obs_rx.recv()).await
            .expect("timeout waiting for second ReleaseAll (re-grab notification)")
            .expect("channel closed");
        assert!(matches!(ev2, ControllerEvent::ReleaseAll), "expected second ReleaseAll on re-grab");
    }

    // ── Test 6: Re-grab GrabPending does not loop back as spurious Release ───

    /// The grab=true+yieldable listener emits GrabPending before signalling
    /// Regrab. That signal echoes back to the same listener on the D-Bus.
    /// The listener must skip it — it must NOT forward a spurious
    /// YieldEvent::Release to the reader task after the Regrab.
    #[tokio::test]
    async fn regrab_grab_pending_does_not_trigger_spurious_release() {
        let (dummy_tx, _dummy_rx) = mpsc::channel(4);
        let (yield_tx, mut yield_rx) = mpsc::channel(8);
        spawn_grab_listener(DEV_REGRAB_NO_LOOP.to_string(), dummy_tx, Some(yield_tx));

        let grabbed = Arc::new(AtomicBool::new(true));

        // Collect yield events and drive the mock grab.
        let grabbed_for_task = grabbed.clone();
        let (collected_tx, mut collected_rx) = mpsc::channel(8);
        tokio::spawn(async move {
            while let Some(cmd) = yield_rx.recv().await {
                if matches!(cmd, YieldEvent::Release) {
                    grabbed_for_task.store(false, Ordering::SeqCst);
                }
                let label: &'static str = match cmd {
                    YieldEvent::Release => "Release",
                    YieldEvent::Regrab  => "Regrab",
                };
                let _ = collected_tx.send(label).await;
            }
        });

        tokio::time::sleep(Duration::from_millis(100)).await;

        let (_, handle) = open_grabbed_with(
            Path::new(DEV_REGRAB_NO_LOOP),
            mock_grab(grabbed),
        ).await.expect("should grab");

        let e1 = timeout(Duration::from_secs(2), collected_rx.recv()).await
            .expect("timeout waiting for Release").expect("closed");
        assert_eq!(e1, "Release");

        // Drop handle → GrabReleased → listener emits GrabPending + sends Regrab.
        drop(handle);

        let e2 = timeout(Duration::from_secs(2), collected_rx.recv()).await
            .expect("timeout waiting for Regrab").expect("closed");
        assert_eq!(e2, "Regrab");

        // Allow the echoed GrabPending to propagate and be processed.
        tokio::time::sleep(Duration::from_millis(300)).await;

        // No spurious Release must arrive.
        let spurious = timeout(Duration::from_millis(100), collected_rx.recv()).await;
        assert!(spurious.is_err(), "spurious event received after Regrab — echo-loop not suppressed");
    }

    // ── Test 4: GrabPending emitted even when device is immediately available ─

    /// GrabPending is always sent upfront, even if nobody holds the grab.
    #[tokio::test]
    async fn grab_pending_sent_even_when_immediately_available() {
        let (tx, mut rx) = mpsc::channel(4);
        spawn_grab_listener(DEV_NO_DBUS.to_string(), tx, None);
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Device is free from the start — no EBUSY.
        let grabbed = Arc::new(AtomicBool::new(false));
        let (_unit, _handle) = open_grabbed_with(
            Path::new(DEV_NO_DBUS),
            mock_grab(grabbed),
        ).await.expect("should succeed immediately");

        // Yieldable session still gets ReleaseAll because GrabPending is
        // always emitted before the first grab attempt.
        let event = timeout(Duration::from_secs(2), rx.recv()).await
            .expect("timeout waiting for ReleaseAll")
            .expect("channel closed");
        assert!(matches!(event, ControllerEvent::ReleaseAll));
    }
}
