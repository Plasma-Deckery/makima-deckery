// ── KWin Window-Activation Watcher ───────────────────────────────────────────
//
// Registers a persistent KWin script (via D-Bus Scripting API) that listens
// for workspace.windowActivated and calls back into makima's own D-Bus object.
// This replaces the old kdotool-per-button-press approach with a true
// event-driven mechanism: zero overhead when nothing changes.
//
// Flow:
//   1. We register "org.makima.watcher" on the session bus and expose
//      the object /watcher with method WindowActivated(class_name).
//   2. We write a tiny KWin script to /tmp and load it via
//      org.kde.kwin.Scripting.loadScript / .start().
//   3. Whenever KWin fires workspace.windowActivated, the script calls
//      back into our method → we update the shared Arc<Mutex<Client>>
//      and wake all EventReader tasks via Arc<Notify>.

use crate::udev_monitor::Client;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};
use zbus::{dbus_interface, dbus_proxy, ConnectionBuilder};

// ── KWin script ──────────────────────────────────────────────────────────────

const KWIN_SCRIPT: &str = r#"
workspace.windowActivated.connect(function(w) {
    var cls = (w && w.resourceClass) ? w.resourceClass : "";
    callDBus("org.makima.watcher", "/watcher", "org.makima.watcher", "WindowActivated", cls);
});
"#;

const PLUGIN_NAME: &str = "makima-watcher";
const SCRIPT_PATH: &str = "/tmp/makima-kwin-watcher.js";

// ── D-Bus interface exposed by makima ─────────────────────────────────────────

struct WatcherIface {
    active_client: Arc<Mutex<Client>>,
    notify: Arc<Notify>,
}

#[dbus_interface(name = "org.makima.watcher")]
impl WatcherIface {
    // Called by the KWin script on every window-activation change.
    async fn window_activated(&self, class_name: String) {
        let client = if class_name.is_empty() {
            Client::Default
        } else {
            Client::Class(class_name)
        };
        *self.active_client.lock().await = client;
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
/// `active_client` and `notify` are shared with all EventReader instances.
pub async fn start_kwin_watcher(
    active_client: Arc<Mutex<Client>>,
    notify: Arc<Notify>,
) {
    if let Err(e) = std::fs::write(SCRIPT_PATH, KWIN_SCRIPT) {
        eprintln!("makima: kwin_watcher: failed to write script: {e}");
        return;
    }

    // Register our D-Bus service so KWin can call back into us.
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

    // Wait for KWin to appear on D-Bus (it may not be ready at boot time).
    // Retry every 2 seconds until the script is loaded successfully.
    loop {
        let scripting = match KwinScriptingProxy::new(&conn).await {
            Ok(p) => p,
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        };

        // Clean up any leftover instance from a previous run.
        let _ = scripting.unload_script(PLUGIN_NAME).await;

        match scripting.load_script(SCRIPT_PATH, PLUGIN_NAME).await {
            Ok(id) => {
                println!("makima: kwin_watcher: script loaded (id {id}), window-activation events enabled.");
                let _ = scripting.start().await;
                break;
            }
            Err(_) => {
                // KWin not ready yet — wait and retry.
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }

    // Keep the connection alive — callbacks arrive asynchronously.
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

    #[tokio::test]
    async fn window_activated_updates_client_and_notifies() {
        let active_client = Arc::new(Mutex::new(Client::Default));
        let notify = Arc::new(Notify::new());

        let iface = WatcherIface {
            active_client: active_client.clone(),
            notify: notify.clone(),
        };

        // First, start waiting for the notification
        let notify_clone = notify.clone();
        let wait_task = tokio::spawn(async move {
            notify_clone.notified().await;
        });

        // Yield to ensure the spawned task actually reaches the `.await`
        // before we trigger the notification.
        tokio::task::yield_now().await;

        // Simulate KWin script calling the D-Bus method
        iface.window_activated("org.mozilla.firefox".to_string()).await;

        // Verify the state was correctly updated
        let client = active_client.lock().await;
        assert_eq!(*client, Client::Class("org.mozilla.firefox".to_string()));

        // Verify the notification was actually fired (should resolve immediately)
        assert!(timeout(Duration::from_millis(100), wait_task).await.is_ok());
    }

    #[tokio::test]
    async fn window_activated_empty_string_resets_to_default() {
        let active_client = Arc::new(Mutex::new(Client::Class("some.other.app".to_string())));
        let notify = Arc::new(Notify::new());

        let iface = WatcherIface {
            active_client: active_client.clone(),
            notify: notify.clone(),
        };

        // Simulate empty class name (e.g., desktop focus lost)
        iface.window_activated("".to_string()).await;

        let client = active_client.lock().await;
        assert_eq!(*client, Client::Default);
    }
}
