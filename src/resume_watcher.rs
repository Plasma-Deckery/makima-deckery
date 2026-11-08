// ── In-process suspend/resume watcher ────────────────────────────────────────
//
// Replaces the external `makima-resume-watcher` bash script + `systemctl
// --user restart makima.service` with an in-process D-Bus subscription to
// logind's `PrepareForSleep` signal. On resume we reuse the existing
// `device_error_notify` reinit path in `udev_monitor.rs` (previously only
// triggered by a read error on the evdev fd — which never happens on Steam
// Deck hardware after suspend; the fd freezes silently instead of erroring).
//
// Why this is faster than the external script:
//   - No blind `sleep 2` guess before acting.
//   - No `systemctl restart` (no process teardown/fork/exec, no re-reading
//     config from disk, no re-registering with systemd).
//   - Reinit happens inside the already-running process via the same
//     `launch_tasks()` path used for USB replug / device-error recovery.

use std::sync::Arc;
use tokio::sync::Notify;
use tokio_stream::StreamExt;
use zbus::{proxy, Connection};

#[proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait LoginManager {
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

/// Starts the resume watcher. Spawn once as a tokio task; runs indefinitely.
///
/// `device_error_notify` is the same `Notify` used by `udev_monitor.rs`'s
/// device-error reinit path — triggering it does a full in-process
/// `launch_tasks()` reinit, exactly like a USB replug would.
pub async fn start_resume_watcher(device_error_notify: Arc<Notify>) {
    let conn = match Connection::system().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("deckery: resume_watcher: system bus connection failed: {e}");
            return;
        }
    };

    let proxy = match LoginManagerProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("deckery: resume_watcher: failed to create logind proxy: {e}");
            return;
        }
    };

    let mut signals = match proxy.receive_prepare_for_sleep().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("deckery: resume_watcher: failed to subscribe to PrepareForSleep: {e}");
            return;
        }
    };

    println!(
        "deckery: resume_watcher: subscribed to PrepareForSleep. +{}ms since startup",
        crate::startup_ms()
    );

    while let Some(signal) = signals.next().await {
        let Ok(args) = signal.args() else { continue };
        if args.start {
            println!(
                "deckery: resume_watcher: suspend starting. +{}ms since startup",
                crate::startup_ms()
            );
        } else {
            println!(
                "deckery: resume_watcher: resume detected, triggering device reinit. +{}ms since startup",
                crate::startup_ms()
            );
            // notify_one(), not notify_waiters(): the consumer is a select! arm
            // in udev_monitor's main loop, which is only re-armed at the top of
            // each iteration. While that loop is inside launch_tasks() — a full
            // device reinit — no waiter is registered, and notify_waiters()
            // keeps no permit, so a resume landing there would be dropped and
            // the devices would never come back. notify_one() stores a permit,
            // so the reinit happens on the next iteration instead. This also
            // matches the other producer on this same Notify (udev_monitor.rs).
            device_error_notify.notify_one();
        }
    }
}
