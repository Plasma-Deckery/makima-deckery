// ── Cooperative grab coordination via D-Bus ───────────────────────────────────
//
// Signals on the system bus coordinate exclusive evdev grabs between
// deckery-controller sessions without requiring a central broker.
//
// Interface: org.Deckery.Controller1
// Object:    /org/Deckery/Controller1
//
//   GrabPending(device_path: str)  — emitted before EVIOCGRAB; yieldable
//                                    sessions flush held keys (and release
//                                    their own grab if they hold one).
//   GrabReleased(device_path: str) — emitted after EVIOCGRAB is released;
//                                    yieldable sessions may re-grab.

use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use zbus::{Connection, MatchRule, MessageStream};
use zbus::message::Type as MsgType;

use crate::ControllerEvent;

const DBUS_PATH:      &str = "/org/Deckery/Controller1";
const DBUS_INTERFACE: &str = "org.Deckery.Controller1";

// In production use the system bus; in tests use the session bus so no root
// access is required and any CI environment with a running dbus-daemon works.
#[cfg(not(test))]
async fn connect() -> zbus::Result<Connection> { Connection::system().await }
#[cfg(test)]
async fn connect() -> zbus::Result<Connection> { Connection::session().await }

// ── Emitter ───────────────────────────────────────────────────────────────────

/// Emit `GrabPending` on the bus for the given device path.
pub async fn emit_grab_pending(device_path: &str) {
    emit_signal("GrabPending", device_path).await;
}

/// Emit `GrabReleased` on the bus for the given device path.
pub async fn emit_grab_released(device_path: String) {
    emit_signal("GrabReleased", &device_path).await;
}

async fn emit_signal(member: &str, device_path: &str) {
    let result: zbus::Result<()> = async {
        let conn = connect().await?;
        conn.emit_signal(
            None::<&str>,
            DBUS_PATH,
            DBUS_INTERFACE,
            member,
            &(device_path,),
        ).await
    }.await;

    if let Err(e) = result {
        eprintln!("deckery-controller: grab_coordinator: {member} failed: {e}");
    }
}

// ── Listener (yieldable sessions) ─────────────────────────────────────────────

/// Spawn a background task that listens for grab coordination signals for
/// `device_path` and forwards them as `ControllerEvent`s.
///
/// - `GrabPending` → `ControllerEvent::ReleaseAll` (flush held output keys)
/// - `GrabReleased` — reserved for future re-grab logic; ignored for now
///
/// The task exits when `event_tx` is closed (session teardown).
pub fn spawn_grab_listener(device_path: String, event_tx: mpsc::Sender<ControllerEvent>) {
    tokio::spawn(async move {
        let conn = match connect().await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("deckery-controller: grab_coordinator: bus unavailable: {e}");
                return;
            }
        };

        let rule = MatchRule::builder()
            .msg_type(MsgType::Signal)
            .interface(DBUS_INTERFACE).unwrap()
            .path(DBUS_PATH).unwrap()
            .build();

        let mut stream = match MessageStream::for_match_rule(rule, &conn, None).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("deckery-controller: grab_coordinator: subscribe failed: {e}");
                return;
            }
        };

        while let Some(Ok(msg)) = stream.next().await {
            let member = msg.header().member().map(|m| m.to_string());
            let body = msg.body();
            let Ok(path) = body.deserialize::<&str>() else { continue };
            if path != device_path { continue; }

            match member.as_deref() {
                Some("GrabPending") => {
                    eprintln!(
                        "deckery-controller: grab_coordinator: GrabPending for {:?} — ReleaseAll",
                        device_path
                    );
                    if event_tx.send(ControllerEvent::ReleaseAll).await.is_err() {
                        break;
                    }
                }
                Some("GrabReleased") => {
                    // Reserved: yieldable+grab sessions would re-grab here.
                }
                _ => {}
            }
        }
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};

    // Each test uses a unique device path so parallel tests on the same session
    // bus cannot receive each other's signals through the path filter.
    const DEV_PENDING:   &str = "/dev/input/event-test-pending";
    const DEV_FILTER:    &str = "/dev/input/event-test-filter";
    const DEV_RELEASED:  &str = "/dev/input/event-test-released";
    const DEV_UNRELATED: &str = "/dev/input/event-test-unrelated";
    const DEV_EXIT:      &str = "/dev/input/event-test-exit";

    async fn subscribe_then_emit(device: &str, signal: &str) -> Option<ControllerEvent> {
        let (tx, mut rx) = mpsc::channel(4);
        spawn_grab_listener(device.to_string(), tx);
        // Give the listener task time to connect and subscribe before emitting.
        tokio::time::sleep(Duration::from_millis(50)).await;
        emit_signal(signal, device).await;
        timeout(Duration::from_secs(2), rx.recv()).await.ok().flatten()
    }

    #[tokio::test]
    async fn grab_pending_delivers_release_all() {
        let event = subscribe_then_emit(DEV_PENDING, "GrabPending").await;
        assert!(matches!(event, Some(ControllerEvent::ReleaseAll)));
    }

    #[tokio::test]
    async fn grab_pending_for_different_device_is_ignored() {
        let (tx, mut rx) = mpsc::channel(4);
        spawn_grab_listener(DEV_FILTER.to_string(), tx);
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Emit for an unrelated device path — the listener must not react.
        emit_signal("GrabPending", DEV_UNRELATED).await;

        let event = timeout(Duration::from_millis(200), rx.recv()).await;
        assert!(event.is_err(), "expected timeout but got an event");
    }

    #[tokio::test]
    async fn grab_released_is_silently_ignored() {
        let event = subscribe_then_emit(DEV_RELEASED, "GrabReleased").await;
        // GrabReleased is currently a no-op — nothing should arrive.
        assert!(event.is_none(), "expected no event but got one");
    }

    #[tokio::test]
    async fn listener_exits_when_channel_closes() {
        let (tx, rx) = mpsc::channel(4);
        spawn_grab_listener(DEV_EXIT.to_string(), tx);
        tokio::time::sleep(Duration::from_millis(50)).await;

        drop(rx);
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Should not panic after the receiver is gone.
        emit_signal("GrabPending", DEV_EXIT).await;
    }
}
