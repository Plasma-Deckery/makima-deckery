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

// ── Emitter ───────────────────────────────────────────────────────────────────

/// Emit `GrabPending` on the system bus for the given device path.
pub async fn emit_grab_pending(device_path: &str) {
    emit_signal("GrabPending", device_path).await;
}

/// Emit `GrabReleased` on the system bus for the given device path.
pub async fn emit_grab_released(device_path: String) {
    emit_signal("GrabReleased", &device_path).await;
}

async fn emit_signal(member: &str, device_path: &str) {
    let result: zbus::Result<()> = async {
        let conn = Connection::system().await?;
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
        let conn = match Connection::system().await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("deckery-controller: grab_coordinator: system bus unavailable: {e}");
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
