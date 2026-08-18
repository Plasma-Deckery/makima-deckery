// ── In-process suspend/resume watcher ────────────────────────────────────────
//
// Replaces the external `makima-resume-watcher` bash script + `systemctl
// --user restart makima.service` with an in-process D-Bus subscription to
// logind's `PrepareForSleep` signal. On resume we trigger a dedicated
// `resume_notify` reinit path in `udev_monitor.rs`, separate from the
// `device_error_notify` path used for a real read error on the evdev fd —
// resume is (near-certainly) the same physical device coming back, so that
// path reuses the existing `VirtualDevices` instead of rebuilding it from
// scratch (see issue #39: rebuilding costs several seconds of libinput
// rediscovery). A genuine read error carries no such guarantee, so it still
// does a full recreate.
//
// Why this should be faster than the external script:
//   - No `sleep 2` blind guess before acting.
//   - No `systemctl restart` (no process teardown/fork/exec, no re-reading
//     config from disk via a fresh process, no re-registering with systemd).
//   - Reinit happens inside the already-running process via the same
//     `launch_tasks()` path used for USB replug/device-error recovery, which
//     profiling showed takes ~160ms end to end from a cold process start —
//     the in-process reinit should be at least that fast, likely faster
//     since config parsing is already warm (still re-reads config files, see
//     `launch_tasks` call site in udev_monitor.rs — future optimization: skip
//     that when we know config hasn't changed).
//
// `launch_tasks` reuses the existing `VirtualDevices` on this path instead of
// rebuilding it from scratch, so the compositor/libinput doesn't have to
// rediscover new uinput devices via udev after every resume (previously
// measured at ~6.36s — see issue #39). Only the physical-device reader is
// rebuilt.
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

/// Starts the resume watcher. Meant to be spawned once as a tokio task; runs
/// indefinitely. `resume_notify` is `udev_monitor.rs`'s dedicated resume
/// reinit signal (separate from `device_error_notify`) — triggering it does
/// an in-process `launch_tasks()` reinit that reuses the existing
/// `VirtualDevices` rather than rebuilding it.
pub async fn start_resume_watcher(resume_notify: Arc<Notify>) {
    let conn = match Connection::system().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("deckery-controller: resume_watcher: system bus connection failed: {e}");
            return;
        }
    };

    let proxy = match LoginManagerProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("deckery-controller: resume_watcher: failed to create logind proxy: {e}");
            return;
        }
    };

    let mut signals = match proxy.receive_prepare_for_sleep().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("deckery-controller: resume_watcher: failed to subscribe to PrepareForSleep: {e}");
            return;
        }
    };

    println!(
        "deckery-controller: resume_watcher: subscribed to PrepareForSleep. +{}ms since startup",
        crate::startup_ms()
    );

    while let Some(signal) = signals.next().await {
        let Ok(args) = signal.args() else { continue };
        if args.start {
            println!(
                "deckery-controller: resume_watcher: suspend starting. +{}ms since startup",
                crate::startup_ms()
            );
        } else {
            println!(
                "deckery-controller: resume_watcher: resume detected, triggering in-process reinit. +{}ms since startup",
                crate::startup_ms()
            );
            resume_notify.notify_waiters();
        }
    }
}
