//! Hyprland compositor adapter.
//!
//! Connects to Hyprland's native IPC event socket (`.socket2.sock`) and
//! listens for `activewindow>>class,title` events. On each event, queries
//! `.socket.sock` for the active window's PID (not included in the event itself).
//! Event-driven: focus changes are pushed immediately, no polling needed.
//!
//! Socket locations (`.socket.sock` is request/response, `.socket2.sock` the
//! event stream), tried in this order:
//!   `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/` — Hyprland 0.40 and later
//!   `/tmp/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`             — older releases
//!
//! If `HYPRLAND_INSTANCE_SIGNATURE` was not imported into the user manager,
//! the instance directory under `$XDG_RUNTIME_DIR/hypr/` (or `/tmp/hypr/`)
//! whose event socket was touched last is used, and the signature is exported
//! so `hyprctl` in the polling path finds the same instance.

use super::notify_focus_change;
use crate::session::Client;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, Notify};

const RECONNECT_DELAY_SECS: u64 = 2;

// ── Socket paths ──────────────────────────────────────────────────────────────

const EVENT_SOCKET: &str = ".socket2.sock";
const REQUEST_SOCKET: &str = ".socket.sock";
const LEGACY_BASE: &str = "/tmp/hypr";

/// Directories that may hold Hyprland instances, current location first.
fn instance_bases(runtime_dir: Option<&str>) -> Vec<PathBuf> {
    let mut bases = Vec::new();
    if let Some(dir) = runtime_dir.filter(|d| !d.is_empty()) {
        bases.push(Path::new(dir).join("hypr"));
    }
    bases.push(PathBuf::from(LEGACY_BASE));
    bases
}

/// Where the socket `name` of instance `signature` may be, in the order to try.
fn socket_candidates(runtime_dir: Option<&str>, signature: &str, name: &str) -> Vec<PathBuf> {
    instance_bases(runtime_dir)
        .into_iter()
        .map(|base| base.join(signature).join(name))
        .collect()
}

/// The first candidate for which `exists` holds.
fn resolve_socket(
    runtime_dir: Option<&str>,
    signature: &str,
    name: &str,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    socket_candidates(runtime_dir, signature, name)
        .into_iter()
        .find(|p| exists(p))
}

/// The signature of the instance whose event socket changed last, for sessions
/// that did not import `HYPRLAND_INSTANCE_SIGNATURE`.
fn newest_instance(runtime_dir: Option<&str>) -> Option<String> {
    instance_bases(runtime_dir)
        .into_iter()
        .find_map(|base| {
            std::fs::read_dir(base)
                .ok()?
                .flatten()
                .filter_map(|entry| {
                    let modified = entry.path().join(EVENT_SOCKET).metadata().ok()?.modified().ok()?;
                    Some((modified, entry.file_name().to_string_lossy().into_owned()))
                })
                .max()
                .map(|(_, signature)| signature)
        })
}

/// The instance signature: the environment's, else the newest instance found
/// on disk (exported, so `hyprctl` agrees).
fn instance_signature() -> Option<String> {
    if let Ok(sig) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        if !sig.is_empty() {
            return Some(sig);
        }
    }
    let sig = newest_instance(std::env::var("XDG_RUNTIME_DIR").ok().as_deref())?;
    println!("deckery: hyprland_watcher: HYPRLAND_INSTANCE_SIGNATURE not set, using instance {sig}");
    std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", &sig);
    Some(sig)
}

fn socket_path(signature: &str, name: &str) -> Option<PathBuf> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
    resolve_socket(runtime_dir.as_deref(), signature, name, |p| p.exists())
}

// ── PID query ─────────────────────────────────────────────────────────────────

/// Query the PID of the currently active window via socket1 (`j/activewindow`).
/// The `activewindow` event on socket2 does not include PID, so we fetch it
/// separately. Returns `None` if the query fails or the window has no pid.
async fn query_active_window_pid() -> Option<u32> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let path = socket_path(&sig, REQUEST_SOCKET)?;
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
    let mut logged_missing = false;
    loop {
        // Resolved on every attempt: at login the socket may not exist yet,
        // and a missing signature can only be found once the instance is up.
        let path = instance_signature().and_then(|sig| socket_path(&sig, EVENT_SOCKET));
        let Some(path) = path else {
            if !logged_missing {
                eprintln!("deckery: hyprland_watcher: no Hyprland event socket found yet, retrying");
                logged_missing = true;
            }
            tokio::time::sleep(std::time::Duration::from_secs(RECONNECT_DELAY_SECS)).await;
            continue;
        };
        match UnixStream::connect(&path).await {
            Ok(stream) => {
                logged_missing = false;
                println!(
                    "deckery: hyprland_watcher: connected to event socket {}, window-activation events enabled. +{}ms since startup",
                    path.display(),
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
    use crate::session::Client;

    fn make_state() -> (Arc<Mutex<Client>>, Arc<Notify>) {
        (
            Arc::new(Mutex::new(Client::Default)),
            Arc::new(Notify::new()),
        )
    }

    #[test]
    fn the_runtime_dir_socket_is_tried_before_tmp() {
        assert_eq!(
            socket_candidates(Some("/run/user/1000"), "abc_123", EVENT_SOCKET),
            vec![
                PathBuf::from("/run/user/1000/hypr/abc_123/.socket2.sock"),
                PathBuf::from("/tmp/hypr/abc_123/.socket2.sock"),
            ]
        );
    }

    #[test]
    fn without_a_runtime_dir_only_tmp_is_tried() {
        assert_eq!(
            socket_candidates(None, "abc", REQUEST_SOCKET),
            vec![PathBuf::from("/tmp/hypr/abc/.socket.sock")]
        );
        assert_eq!(socket_candidates(Some(""), "abc", REQUEST_SOCKET).len(), 1);
    }

    #[test]
    fn resolution_takes_the_current_path_when_it_exists() {
        let found = resolve_socket(Some("/run/user/1000"), "abc", EVENT_SOCKET, |_| true);
        assert_eq!(found, Some(PathBuf::from("/run/user/1000/hypr/abc/.socket2.sock")));
    }

    #[test]
    fn resolution_falls_back_to_tmp_for_old_hyprland() {
        let found = resolve_socket(Some("/run/user/1000"), "abc", EVENT_SOCKET, |p| p.starts_with("/tmp"));
        assert_eq!(found, Some(PathBuf::from("/tmp/hypr/abc/.socket2.sock")));
    }

    #[test]
    fn resolution_finds_nothing_when_no_socket_exists() {
        assert_eq!(resolve_socket(Some("/run/user/1000"), "abc", EVENT_SOCKET, |_| false), None);
    }

    #[test]
    fn the_newest_instance_is_found_on_disk() {
        let runtime = std::env::temp_dir().join(format!("makima-hypr-test-{}", std::process::id()));
        for sig in ["old", "new"] {
            let dir = runtime.join("hypr").join(sig);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(EVENT_SOCKET), b"").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // A directory without an event socket is not an instance.
        std::fs::create_dir_all(runtime.join("hypr").join("stale")).unwrap();
        let found = newest_instance(runtime.to_str());
        std::fs::remove_dir_all(&runtime).unwrap();
        assert_eq!(found.as_deref(), Some("new"));
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
