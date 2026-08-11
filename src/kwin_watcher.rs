// ── KWin Window-Activation Watcher ───────────────────────────────────────────
//
// Registers a persistent KWin script (via D-Bus Scripting API) that listens
// for workspace.windowActivated and calls back into makima's own D-Bus object.
// This replaces the old kdotool-per-button-press approach with a true
// event-driven mechanism: zero overhead when nothing changes.
//
// Flow:
//   1. We register "org.makima.watcher" on the session bus and expose
//      the object /watcher with method WindowActivated(class_name, caption).
//   2. We write a tiny KWin script to /tmp and load it via
//      org.kde.kwin.Scripting.loadScript / .start().
//   3. Whenever KWin fires workspace.windowActivated, the script calls
//      back into our method → we update the shared Arc<Mutex<Client>> with
//      Client::Class(class, caption) and fire Arc<Notify>.
//
// This module only reports focus changes. All Gaming Mode policy lives in
// EventReader::steam_detection_loop(), which reads the caption from active_client.

use crate::udev_monitor::Client;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};
use zbus::{dbus_interface, dbus_proxy, ConnectionBuilder};

// ── KWin script ──────────────────────────────────────────────────────────────
//
// Forwards raw class + caption — no classification logic here.
// `steam_detector` decides what they mean in Rust.

const KWIN_SCRIPT: &str = r#"
workspace.windowActivated.connect(function(w) {
    var cls = (w && w.resourceClass) ? w.resourceClass : "";
    var cap = (w && w.caption)       ? w.caption       : "";
    callDBus("org.makima.watcher", "/watcher", "org.makima.watcher", "WindowActivated", cls, cap);
});
"#;

const PLUGIN_NAME: &str = "makima-watcher";
const SCRIPT_PATH: &str = "/tmp/makima-kwin-watcher.js";

// ── D-Bus interface exposed by makima ─────────────────────────────────────────

struct WatcherIface {
    /// Shared client state — holds class + caption after every focus change.
    /// EventReader uses class for config selection; steam_detection_loop
    /// uses caption for Big Picture Mode detection.
    active_client: Arc<Mutex<Client>>,
    /// Wakes all EventReader tasks on every focus change.
    notify: Arc<Notify>,
}

#[dbus_interface(name = "org.makima.watcher")]
impl WatcherIface {
    /// Called by the KWin script on every window-activation change.
    /// Stores raw class + caption in active_client and fires notify.
    /// No detection logic here — that lives in EventReader::steam_detection_loop().
    async fn window_activated(&self, class_name: String, caption: String) {
        *self.active_client.lock().await = if class_name.is_empty() {
            Client::Default
        } else {
            Client::Class(class_name, caption)
        };
        self.notify.notify_waiters();
    }
}

// ── KWin Scripting D-Bus proxy ────────────────────────────────────────────────

#[dbus_proxy(
    interface = "org.kde.kwin.Scripting",
    default_service = "org.kde.KWin",
    default_path = "/Scripting"
)]
trait KwinScripting {
    #[dbus_proxy(name = "loadScript")]
    async fn load_script(&self, path: &str, plugin_name: &str) -> zbus::Result<i32>;
    #[dbus_proxy(name = "start")]
    async fn start(&self) -> zbus::Result<()>;
    #[dbus_proxy(name = "unloadScript")]
    async fn unload_script(&self, plugin_name: &str) -> zbus::Result<bool>;
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Starts the KWin window-activation watcher.
/// Meant to be spawned once as a tokio task; runs indefinitely.
///
/// On every focus change the watcher stores `Client::Class(class, caption)`
/// in `active_client` and fires `notify`, waking all EventReader tasks
/// (config re-evaluation and steam_detection_loop).
pub async fn start_kwin_watcher(
    active_client: Arc<Mutex<Client>>,
    notify: Arc<Notify>,
) {
    if let Err(e) = std::fs::write(SCRIPT_PATH, KWIN_SCRIPT) {
        eprintln!("makima: kwin_watcher: failed to write script: {e}");
        return;
    }

    let conn = match ConnectionBuilder::session()
        .and_then(|b| b.name("org.makima.watcher"))
        .and_then(|b| {
            b.serve_at(
                "/watcher",
                WatcherIface {
                    active_client,
                    notify,
                },
            )
        }) {
        Ok(builder) => match builder.build().await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("makima: kwin_watcher: D-Bus connection failed: {e}");
                return;
            }
        },
        Err(e) => {
            eprintln!("makima: kwin_watcher: D-Bus setup failed: {e}");
            return;
        }
    };

    loop {
        let scripting = match KwinScriptingProxy::new(&conn).await {
            Ok(p) => p,
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        };

        let _ = scripting.unload_script(PLUGIN_NAME).await;

        match scripting.load_script(SCRIPT_PATH, PLUGIN_NAME).await {
            Ok(id) => {
                println!(
                    "makima: kwin_watcher: script loaded (id {id}), window-activation events enabled. +{}ms since startup",
                    crate::startup_ms()
                );
                let _ = scripting.start().await;
                break;
            }
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::udev_monitor::Client;
    use tokio::time::{timeout, Duration};

    fn make_iface() -> WatcherIface {
        WatcherIface {
            active_client: Arc::new(Mutex::new(Client::Default)),
            notify: Arc::new(Notify::new()),
        }
    }

    #[tokio::test]
    async fn window_activated_stores_class_and_caption_in_client() {
        let active_client = Arc::new(Mutex::new(Client::Default));
        let notify = Arc::new(Notify::new());
        let iface = WatcherIface {
            active_client: active_client.clone(),
            notify: notify.clone(),
        };

        let notify_clone = notify.clone();
        let wait_task = tokio::spawn(async move { notify_clone.notified().await; });
        tokio::task::yield_now().await;

        iface.window_activated("org.mozilla.firefox".to_string(), "Firefox".to_string()).await;

        assert_eq!(
            *active_client.lock().await,
            Client::Class("org.mozilla.firefox".to_string(), "Firefox".to_string()),
        );
        assert!(timeout(Duration::from_millis(100), wait_task).await.is_ok());
    }

    #[tokio::test]
    async fn window_activated_empty_string_resets_to_default() {
        let iface = make_iface();
        *iface.active_client.lock().await =
            Client::Class("some.other.app".to_string(), String::new());
        iface.window_activated("".to_string(), "".to_string()).await;
        assert_eq!(*iface.active_client.lock().await, Client::Default);
    }

    #[tokio::test]
    async fn window_activated_stores_bpm_caption() {
        let iface = make_iface();
        iface.window_activated("steam".to_string(), "Steam Big Picture".to_string()).await;
        assert_eq!(
            *iface.active_client.lock().await,
            Client::Class("steam".to_string(), "Steam Big Picture".to_string()),
        );
    }

    #[tokio::test]
    async fn window_activated_fires_notify_for_every_focus_change() {
        let iface = make_iface();
        iface.window_activated("org.mozilla.firefox".to_string(), "Firefox".to_string()).await;
        let notify_clone = iface.notify.clone();
        let wait_task = tokio::spawn(async move { notify_clone.notified().await; });
        tokio::task::yield_now().await;
        iface.window_activated("org.kde.dolphin".to_string(), "Dolphin".to_string()).await;
        assert!(timeout(Duration::from_millis(100), wait_task).await.is_ok());
    }
}
