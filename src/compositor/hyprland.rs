//! Hyprland compositor adapter.
//!
//! Connects to Hyprland's native IPC event socket (`.socket2.sock`) and
//! listens for `activewindow>>class,title` events. On each event, queries
//! `.socket.sock` for the active window's PID (not included in the event itself).
//! Event-driven: focus changes are pushed immediately, no polling needed.
//!
//! Socket locations:
//!   `/tmp/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock`  — request/response
//!   `/tmp/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock` — event stream

use super::notify_focus_change;
use crate::udev_monitor::Client;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, Notify};

const RECONNECT_DELAY_SECS: u64 = 2;

// ── Socket paths ──────────────────────────────────────────────────────────────

fn socket2_path() -> Option<String> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    Some(format!("/tmp/hypr/{}/.socket2.sock", sig))
}

fn socket1_path() -> Option<String> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    Some(format!("/tmp/hypr/{}/.socket.sock", sig))
}

// ── PID query ─────────────────────────────────────────────────────────────────

/// Query the PID of the currently active window via socket1 (`j/activewindow`).
/// The `activewindow` event on socket2 does not include PID, so we fetch it
/// separately. Returns `None` if the query fails or the window has no pid.
async fn query_active_window_pid() -> Option<u32> {
    let path = socket1_path()?;
    let mut stream = UnixStream::connect(&path).await.ok()?;
    stream.write_all(b"j/activewindow").await.ok()?;
    let mut response = String::new();
    BufReader::new(stream).read_to_string(&mut response).await.ok()?;
    let json: serde_json::Value = serde_json::from_str(&response).ok()?;
    json["pid"].as_u64().map(|p| p as u32)
}

// ── Focus watcher ─────────────────────────────────────────────────────────────

/// Hyprland focus watcher. Connects to `.socket2.sock` and processes
/// `activewindow>>class,title` events indefinitely, reconnecting on disconnect.
pub async fn run_focus_watcher(
    active_client: Arc<Mutex<Client>>,
    notify: Arc<Notify>,
) {
    let path = match socket2_path() {
        Some(p) => p,
        None => {
            eprintln!("deckery: hyprland_watcher: HYPRLAND_INSTANCE_SIGNATURE not set, focus watcher disabled");
            return;
        }
    };

    loop {
        match UnixStream::connect(&path).await {
            Ok(stream) => {
                println!(
                    "deckery: hyprland_watcher: connected to event socket, window-activation events enabled. +{}ms since startup",
                    crate::startup_ms()
                );
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => {
                            eprintln!("deckery: hyprland_watcher: socket closed, reconnecting...");
                            break;
                        }
                        Ok(_) => {
                            handle_event(line.trim_end(), &active_client, &notify).await;
                        }
                        Err(e) => {
                            eprintln!("deckery: hyprland_watcher: read error: {e}, reconnecting...");
                            break;
                        }
                    }
                }
            }
            Err(_) => {
                // Socket not yet available — Hyprland may still be starting up.
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(RECONNECT_DELAY_SECS)).await;
    }
}

/// Process a single line from Hyprland's event socket.
async fn handle_event(line: &str, active_client: &Arc<Mutex<Client>>, notify: &Arc<Notify>) {
    if let Some(rest) = line.strip_prefix("activewindow>>") {
        // Format: "class,title" — title may contain commas, only split on the first.
        let (class, title) = rest
            .split_once(',')
            .map(|(c, t)| (c.to_string(), t.to_string()))
            .unwrap_or_else(|| (rest.to_string(), String::new()));

        let pid = query_active_window_pid().await;
        notify_focus_change(active_client, notify, class, title, pid).await;
    }
    // Other event types (openwindow, closewindow, workspace, etc.) are ignored.
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::udev_monitor::Client;

    fn make_state() -> (Arc<Mutex<Client>>, Arc<Notify>) {
        (
            Arc::new(Mutex::new(Client::Default)),
            Arc::new(Notify::new()),
        )
    }

    #[tokio::test]
    async fn handle_event_ignores_non_activewindow_events() {
        let (ac, n) = make_state();
        handle_event("openwindow>>abc,title,class", &ac, &n).await;
        assert_eq!(*ac.lock().await, Client::Default);
    }

    #[tokio::test]
    async fn handle_event_parses_class_and_title() {
        let (ac, n) = make_state();
        // PID query will return None (no Hyprland running in test), that is fine.
        handle_event("activewindow>>org.mozilla.firefox,Firefox — makima", &ac, &n).await;
        let client = ac.lock().await.clone();
        match client {
            Client::Class(class, title, _pid) => {
                assert_eq!(class, "org.mozilla.firefox");
                assert_eq!(title, "Firefox — makima");
            }
            other => panic!("expected Class, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn handle_event_empty_activewindow_resets_to_default() {
        let (ac, n) = make_state();
        *ac.lock().await = Client::Class("steam".to_string(), "Big Picture".to_string(), None);
        // Hyprland fires "activewindow>>," when all windows are closed.
        handle_event("activewindow>>,", &ac, &n).await;
        assert_eq!(*ac.lock().await, Client::Default);
    }
}
