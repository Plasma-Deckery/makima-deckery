// ── Cooperative grab yield protocol ──────────────────────────────────────────
//
// Protocol for acquiring an exclusive evdev grab without blocking other
// deckery-controller consumers indefinitely:
//
//  1. Emit `GrabPending` signal (system D-Bus) so yieldable sessions flush
//     their held output keys and release their own grab if they hold one.
//  2. Retry EVIOCGRAB in a tight loop until success or timeout.
//  3. Emit `GrabReleased` signal when the grab is relinquished so yieldable
//     sessions can re-grab.
//
// Callers use `open_grabbed` to acquire and get back a `GrabbedStream` RAII
// guard. Dropping the guard releases the evdev grab and emits `GrabReleased`.

use std::io;
use std::path::Path;
use std::time::Duration;
use evdev::EventStream;

use crate::grab_coordinator;

/// How long to keep retrying EVIOCGRAB before giving up.
const GRAB_TIMEOUT: Duration = Duration::from_secs(5);

/// Interval between EVIOCGRAB attempts.
const GRAB_RETRY_INTERVAL: Duration = Duration::from_millis(100);

// ── Public API ────────────────────────────────────────────────────────────────

/// Acquire an exclusive evdev grab using the yield protocol.
///
/// 1. Tries `EVIOCGRAB` immediately.
/// 2. On the first `EBUSY` — and only then — emits `GrabPending` so any
///    session that holds the grab knows to yield it.
/// 3. Keeps retrying every [`GRAB_RETRY_INTERVAL`] until success or
///    [`GRAB_TIMEOUT`] is reached.
///
/// `GrabPending` is intentionally deferred to the first failure: if no other
/// session holds the grab the device is available immediately and D-Bus is
/// never contacted. The 500 ms cap on the notification is a safeguard for
/// slow D-Bus connections; the retry loop continues regardless.
///
/// Returns the grabbed [`EventStream`]. The caller is responsible for emitting
/// `GrabReleased` (via [`signal_grab_released`]) when the grab is relinquished.
pub async fn open_grabbed(path: &Path) -> io::Result<EventStream> {
    let deadline = tokio::time::Instant::now() + GRAB_TIMEOUT;
    let mut pending_emitted = false;

    loop {
        match crate::try_open_event_stream(path, /*grab=*/ true) {
            Ok(stream) => return Ok(stream),
            Err(e) if is_busy(&e) => {
                // First EBUSY: now we know someone holds the grab.
                // Emit GrabPending once so they can yield.
                if !pending_emitted {
                    let _ = tokio::time::timeout(
                        Duration::from_millis(500),
                        grab_coordinator::emit_grab_pending(path.to_str().unwrap_or("")),
                    ).await;
                    pending_emitted = true;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "could not grab {:?} within {:?}: {e}",
                            path, GRAB_TIMEOUT
                        ),
                    ));
                }
                tokio::time::sleep(GRAB_RETRY_INTERVAL).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Emit `GrabReleased` on the system bus.
///
/// Call this when the grab acquired via [`open_grabbed`] is relinquished so
/// yieldable sessions know they can re-grab the device.
pub async fn signal_grab_released(path: String) {
    grab_coordinator::emit_grab_released(path).await;
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn is_busy(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::WouldBlock
        || e.raw_os_error() == Some(libc::EBUSY)
}
