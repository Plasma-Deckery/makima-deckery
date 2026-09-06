//! KDE / KWin compositor adapter.
//!
//! Registers a persistent KWin script (via D-Bus Scripting API) that listens
//! for `workspace.windowActivated` and calls back into makima's own D-Bus object.
//! Event-driven: zero overhead when nothing changes.
//!
//! Flow:
//!   1. We register `org.makima.watcher` on the session bus and expose
//!      the object `/watcher` with method `WindowActivated(class, caption, pid)`.
//!   2. We write a tiny KWin script to /tmp and load it via
//!      `org.kde.kwin.Scripting.loadScript` / `.start()`.
//!   3. Whenever KWin fires `workspace.windowActivated`, the script calls
//!      back into our method → we update the shared `Arc<Mutex<Client>>`
//!      via `super::notify_focus_change()` and fire `Arc<Notify>`.

use super::notify_focus_change;
use crate::udev_monitor::Client;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};
use zbus::{interface, proxy, connection::Builder};

// ── KWin script ──────────────────────────────────────────────────────────────
//
// Forwards raw class + caption + pid — no classification logic here.
// steam_detector decides what they mean in Rust.

const KWIN_SCRIPT: &str = r#"
function makimaSend(w) {
    var cls = (w && w.resourceClass) ? w.resourceClass : "";
    var cap = (w && w.caption)       ? w.caption       : "";
    var pid = (w && w.pid)           ? w.pid           : 0;
    callDBus("org.makima.watcher", "/watcher", "org.makima.watcher", "WindowActivated", cls, cap, pid);
}
workspace.windowActivated.connect(makimaSend);
// Push the currently focused window once, right here at load time. Without it
// makima only ever learns about focus by *reacting* to a change, so after a
// restart the focus state stays at its default — and every app-specific config
// remains inactive until the user happens to switch windows.
// `activeWindow` is the Plasma 6 name; on Plasma 5 it is undefined, which
// makimaSend handles by sending empty strings (the default state anyway).
makimaSend(workspace.activeWindow);
"#;

const PLUGIN_NAME: &str = "makima-watcher";
const SCRIPT_PATH: &str = "/tmp/makima-kwin-watcher.js";

// ── D-Bus interface exposed by makima ─────────────────────────────────────────

struct WatcherIface {
    active_client: Arc<Mutex<Client>>,
    notify: Arc<Notify>,
}

#[interface(name = "org.makima.watcher")]
impl WatcherIface {
    /// Called by the KWin script on every window-activation change.
    ///
    /// Note: KWin's callDBus sends JavaScript numbers as D-Bus int32 (i),
    /// so pid must be i32 here — not u32 — to avoid a silent type mismatch
    /// that would cause the entire D-Bus call to be silently dropped.
    async fn window_activated(&self, class_name: String, caption: String, pid: i32) {
        let pid = if pid > 0 { Some(pid as u32) } else { None };
        notify_focus_change(&self.active_client, &self.notify, class_name, caption, pid).await;
    }
}

// ── KWin Scripting D-Bus proxy ────────────────────────────────────────────────

#[proxy(
    interface = "org.kde.kwin.Scripting",
    default_service = "org.kde.KWin",
    default_path = "/Scripting"
)]
trait KwinScripting {
    #[zbus(name = "loadScript")]
    async fn load_script(&self, path: &str, plugin_name: &str) -> zbus::Result<i32>;
    #[zbus(name = "start")]
    async fn start(&self) -> zbus::Result<()>;
    #[zbus(name = "unloadScript")]
    async fn unload_script(&self, plugin_name: &str) -> zbus::Result<bool>;
}

// ── Public entry point ────────────────────────────────────────────────────────

/// KDE focus-watcher: loads a KWin script via D-Bus that calls back on every
/// `workspace.windowActivated` event. Runs indefinitely.
pub async fn run_focus_watcher(
    active_client: Arc<Mutex<Client>>,
    notify: Arc<Notify>,
) {
    if let Err(e) = std::fs::write(SCRIPT_PATH, KWIN_SCRIPT) {
        eprintln!("deckery: kde_watcher: failed to write script: {e}");
        return;
    }

    let conn = match Builder::session()
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
                eprintln!("deckery: kde_watcher: D-Bus connection failed: {e}");
                return;
            }
        },
        Err(e) => {
            eprintln!("deckery: kde_watcher: D-Bus setup failed: {e}");
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
                    "deckery: kde_watcher: script loaded (id {id}), window-activation events enabled. +{}ms since startup",
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

        iface.window_activated("org.mozilla.firefox".to_string(), "Firefox".to_string(), 12345).await;

        assert_eq!(
            *active_client.lock().await,
            Client::Class("org.mozilla.firefox".to_string(), "Firefox".to_string(), Some(12345)),
        );
        assert!(timeout(Duration::from_millis(100), wait_task).await.is_ok());
    }

    #[tokio::test]
    async fn window_activated_empty_string_resets_to_default() {
        let iface = make_iface();
        *iface.active_client.lock().await =
            Client::Class("some.other.app".to_string(), String::new(), None);
        iface.window_activated("".to_string(), "".to_string(), 0).await;
        assert_eq!(*iface.active_client.lock().await, Client::Default);
    }

    #[tokio::test]
    async fn window_activated_stores_bpm_caption() {
        let iface = make_iface();
        iface.window_activated("steam".to_string(), "Steam Big Picture".to_string(), 5678).await;
        assert_eq!(
            *iface.active_client.lock().await,
            Client::Class("steam".to_string(), "Steam Big Picture".to_string(), Some(5678)),
        );
    }

    #[tokio::test]
    async fn window_activated_fires_notify_for_every_focus_change() {
        let iface = make_iface();
        iface.window_activated("org.mozilla.firefox".to_string(), "Firefox".to_string(), 111).await;
        let notify_clone = iface.notify.clone();
        let wait_task = tokio::spawn(async move { notify_clone.notified().await; });
        tokio::task::yield_now().await;
        iface.window_activated("org.kde.dolphin".to_string(), "Dolphin".to_string(), 222).await;
        assert!(timeout(Duration::from_millis(100), wait_task).await.is_ok());
    }
}
