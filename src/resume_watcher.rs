// ── In-process suspend/resume watcher ────────────────────────────────────────
//
// EXPERIMENTAL (branch: experiment/inproc-resume-reconnect-v2).
//
// Replaces the external `makima-resume-watcher` bash script + `systemctl
// --user restart makima.service` with an in-process D-Bus subscription to
// logind's `PrepareForSleep` signal. On resume we reuse the *existing*
// `device_error_notify` reinit path in `udev_monitor.rs` (previously only
// triggered by a read error on the evdev fd — which never happens on this
// hardware after suspend, the fd just freezes silently instead of erroring).
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
// What's NOT solved yet by this alone: `launch_tasks` still rebuilds
// `VirtualDevices` from scratch on every reinit (new uinput devices), so the
// compositor/libinput still has to rediscover them via udev after every
// resume. If that turns out to be the remaining bottleneck, the next step is
// decoupling `VirtualDevices` lifetime from the reinit cycle so only the
// physical-device reader is rebuilt on resume.
use std::sync::Arc;
use tokio::sync::Notify;
use tokio_stream::StreamExt;
use zbus::{dbus_proxy, Connection};

#[dbus_proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait LoginManager {
    #[dbus_proxy(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

/// Starts the resume watcher. Meant to be spawned once as a tokio task; runs
/// indefinitely. `device_error_notify` is the same `Notify` used by
/// `udev_monitor.rs`'s device-error reinit path — triggering it does a full
/// in-process `launch_tasks()` reinit, exactly like a USB replug would.
pub async fn start_resume_watcher(device_error_notify: Arc<Notify>) {
    let conn = match Connection::system().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("makima: resume_watcher: system bus connection failed: {e}");
            return;
        }
    };

    let proxy = match LoginManagerProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("makima: resume_watcher: failed to create logind proxy: {e}");
            return;
        }
    };

    let mut signals = match proxy.receive_prepare_for_sleep().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("makima: resume_watcher: failed to subscribe to PrepareForSleep: {e}");
            return;
        }
    };

    println!(
        "makima: resume_watcher: subscribed to PrepareForSleep. +{}ms since startup",
        crate::startup_ms()
    );

    while let Some(signal) = signals.next().await {
        let Ok(args) = signal.args() else { continue };
        if args.start {
            println!(
                "makima: resume_watcher: suspend starting. +{}ms since startup",
                crate::startup_ms()
            );
        } else {
            println!(
                "makima: resume_watcher: resume detected, triggering in-process reinit. +{}ms since startup",
                crate::startup_ms()
            );
            device_error_notify.notify_waiters();
        }
    }
}
