// ── Cooperative grab yield protocol ──────────────────────────────────────────
//
// Protocol for acquiring an exclusive evdev grab without blocking other
// deckery-controller consumers indefinitely:
//
//  1. Establish one D-Bus connection for the entire grab session.
//  2. Emit `GrabPending` — all yieldable sessions flush held output keys;
//     sessions that hold EVIOCGRAB release it.
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
/// 2. Emits `GrabPending` immediately on that connection — all yieldable
///    sessions receive it regardless of whether they hold EVIOCGRAB.
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
    let path_str = path.to_str().unwrap_or("");

    // ── 1. Establish D-Bus connection once for this grab session ──────────────
    let conn = grab_coordinator::connect().await.ok();

    // ── 2. Notify yieldable grab=true sessions upfront ───────────────────────
    // Sessions with grab=true and yieldable=true must release their EVIOCGRAB
    // before we can acquire it. grab=false sessions need no notification —
    // their evdev stream pauses automatically while another process holds
    // EVIOCGRAB and resumes on its own after release.
    if let Some(ref c) = conn {
        grab_coordinator::emit_signal_on(c, "GrabPending", path_str).await;
    }

    // ── 3. Retry EVIOCGRAB until success or timeout ───────────────────────────
    let deadline = tokio::time::Instant::now() + GRAB_TIMEOUT;
    loop {
        match crate::try_open_event_stream(path, /*grab=*/ true) {
            Ok(stream) => {
                let handle = GrabbedHandle { conn, path: path_str.to_string() };
                return Ok((stream, handle));
            }
            Err(e) if is_busy(&e) => {
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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn is_busy(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::WouldBlock
        || e.raw_os_error() == Some(libc::EBUSY)
}
