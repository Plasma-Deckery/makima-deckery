//! Steam game-running detection and the async detection task.
//!
//! `steam_detection_task` is the live async loop that bridges kwin_watcher
//! focus events with the `gaming_mode_set_loop` in EventReader.
//!
//! Answers one question: "should Gaming Mode be active right now?"
//!
//! Two independent signals are combined (OR):
//!   1. **Window class** — the currently focused window is Steam Big Picture
//!      Mode (`"steam"` class + BPM caption forwarded by the KWin script).
//!   2. **Process tree** — walk upward from the focused window's PID via
//!      `/proc/{pid}/status` and check whether `reaper → steam` appears in
//!      the ancestor chain.  This is the reliable signal that a Steam game is
//!      actually running.
//!
//! ## Why /proc instead of sysinfo
//!
//! sysinfo's `refresh_processes(All)` reads every field of every process on
//! the system (~160-340 ms).  We only need to answer one question: "is this
//! PID a descendant of steam via reaper?"  Reading a handful of
//! `/proc/{pid}/status` files (one per ancestor level, typically 3-7) takes
//! under 1 ms — the whole sysinfo dependency is gone.
//!
//! ## Module structure
//!
//! All functions are pure and side-effect-free (filesystem reads only, no
//! global state).  `should_be_gaming` is the single public entry point used
//! by `steam_detection_task`.

use crate::udev_monitor::Client;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, Notify};

/// Returns `true` if the focused window is Steam Big Picture Mode.
///
/// BPM is identified by: class == "steam" AND caption contains "Big Picture"
/// or "Big-Picture" (KWin uses either form depending on the Steam version).
pub fn is_steam_bpm(class_name: &str, caption: &str) -> bool {
    class_name == "steam"
        && (caption.contains("Big Picture") || caption.contains("Big-Picture"))
}

/// Walk upward from `focused_pid` through the process tree via
/// `/proc/{pid}/status` and return `true` if `reaper → steam` appears in the
/// ancestor chain, meaning a Steam game is running.
///
/// Process tree structure:
/// ```
/// steam  →  reaper  →  [game process / proton / wine / bwrap / …]
/// ```
///
/// Reads only `Name:` and `PPid:` from each `/proc/{pid}/status` — one file
/// per ancestor level.  Handles disappeared processes gracefully (returns
/// `false`).  Stops at PID 1 (init).
pub fn is_game_running(focused_pid: u32) -> bool {
    let mut current = focused_pid;
    loop {
        let (name, ppid) = match read_proc_status(current) {
            Some(v) => v,
            None => return false, // process disappeared
        };
        if ppid <= 1 {
            return false; // reached init without finding steam/reaper
        }
        if name == "reaper" {
            // Confirm the reaper's parent is steam.
            return read_proc_status(ppid)
                .map(|(parent_name, _)| parent_name == "steam")
                .unwrap_or(false);
        }
        current = ppid;
    }
}

/// Read `Name:` and `PPid:` from `/proc/{pid}/status`.
/// Returns `None` if the file cannot be read (process gone).
fn read_proc_status(pid: u32) -> Option<(String, u32)> {
    let content = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
    let name = parse_field(&content, "Name")?;
    let ppid: u32 = parse_field(&content, "PPid")?.parse().ok()?;
    Some((name, ppid))
}

/// Extract the value of `Field:` from a `/proc/.../status` file.
fn parse_field(content: &str, field: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let rest = line.strip_prefix(field)?.strip_prefix(':')?;
        Some(rest.trim().to_string())
    })
}

/// Async loop that watches for window-focus changes and sends the desired
/// Gaming Mode state (`true` = gaming) over `tx`.
///
/// Spawned once per session from `launch_tasks` in `udev_monitor`.
/// Compositor-agnostic: on non-KDE systems `window_changed` is never fired
/// and the task simply blocks on `notified()` forever — zero overhead.
/// Only sends when the state actually changes to avoid flooding the channel.
///
/// When `auto_detect` is false the task still runs but never sends `true` —
/// Gaming Mode can only be set via the manual double-click trigger or IPC.
pub async fn steam_detection_task(
    window_changed: Arc<Notify>,
    active_client: Arc<Mutex<Client>>,
    tx: mpsc::Sender<bool>,
    auto_detect: bool,
) {
    let mut last_sent: Option<bool> = None;
    loop {
        window_changed.notified().await;
        let new_state = if auto_detect {
            let (class, caption, pid) = match &*active_client.lock().await {
                Client::Class(c, cap, p) => (c.clone(), cap.clone(), *p),
                Client::Default => (String::new(), String::new(), None),
            };
            let bpm  = is_steam_bpm(&class, &caption);
            let game = pid.map(is_game_running).unwrap_or(false);
            bpm || game
        } else {
            false
        };
        if Some(new_state) != last_sent {
            if tx.send(new_state).await.is_err() {
                break; // all receivers gone, stop
            }
            last_sent = Some(new_state);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_bpm_class_and_caption_triggers_gaming() {
        assert!(is_steam_bpm("steam", "Steam Big Picture"));
        assert!(is_steam_bpm("steam", "Steam Big-Picture"));
    }

    #[test]
    fn steam_class_without_bpm_caption_does_not_trigger() {
        assert!(!is_steam_bpm("steam", "Steam"));
        assert!(!is_steam_bpm("steam", ""));
    }

    #[test]
    fn firefox_class_does_not_trigger() {
        assert!(!is_steam_bpm("org.mozilla.firefox", "Mozilla Firefox"));
    }

    #[test]
    fn empty_class_does_not_trigger() {
        assert!(!is_steam_bpm("", ""));
    }

    #[test]
    fn no_pid_means_not_gaming() {
        assert!(!is_steam_bpm("", "") && !is_game_running(1));
        assert!(!is_steam_bpm("org.mozilla.firefox", "Firefox"));
    }

    #[test]
    fn bpm_wins_even_without_pid() {
        assert!(is_steam_bpm("steam", "Steam Big Picture"));
    }

    #[test]
    fn parse_field_extracts_name_and_ppid() {
        let status = "Name:\tfoo\nPPid:\t42\nVmRSS:\t1234 kB\n";
        assert_eq!(parse_field(status, "Name").as_deref(), Some("foo"));
        assert_eq!(parse_field(status, "PPid").as_deref(), Some("42"));
        assert_eq!(parse_field(status, "Missing"), None);
    }

    /// When `auto_detect_steam_games = false` the task must never send `true`,
    /// even when the focused window looks exactly like Steam Big Picture Mode.
    #[tokio::test]
    async fn auto_detect_false_never_sends_true() {
        use crate::udev_monitor::Client;
        let notify = Arc::new(Notify::new());
        // Focused window is Steam BPM — would normally trigger gaming mode.
        let client = Arc::new(Mutex::new(Client::Class(
            "steam".to_string(),
            "Steam Big Picture".to_string(),
            None,
        )));
        let (tx, mut rx) = mpsc::channel::<bool>(8);
        // notify_one() stores a permit so the task fires immediately on first
        // notified().await even if we race ahead of the spawn.
        notify.notify_one();
        tokio::spawn(steam_detection_task(
            notify.clone(),
            client,
            tx,
            false, // auto_detect_steam_games = false
        ));
        // Allow the task one iteration.
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let state = rx.try_recv().expect("task should have sent a value");
        assert!(!state, "auto_detect_steam_games=false must never send true");
    }

    /// When `auto_detect_steam_games = true` the task must send `true` when the
    /// focused window is Steam Big Picture Mode.
    #[tokio::test]
    async fn auto_detect_true_sends_true_for_bpm() {
        use crate::udev_monitor::Client;
        let notify = Arc::new(Notify::new());
        let client = Arc::new(Mutex::new(Client::Class(
            "steam".to_string(),
            "Steam Big Picture".to_string(),
            None,
        )));
        let (tx, mut rx) = mpsc::channel::<bool>(8);
        notify.notify_one();
        tokio::spawn(steam_detection_task(
            notify.clone(),
            client,
            tx,
            true, // auto_detect_steam_games = true
        ));
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let state = rx.try_recv().expect("task should have sent a value");
        assert!(state, "auto_detect_steam_games=true should send true for BPM window");
    }

    // Live process-tree test: only verifies no panic; whether a game is
    // running is non-deterministic in CI.
    #[test]
    fn is_game_running_pid1_returns_false() {
        assert!(!is_game_running(1));
    }

    /// Measures how long a full /proc walk takes on the live system.
    /// Run with: cargo test -- --nocapture proc_walk_timing
    ///
    /// Expected: < 1ms even walking all the way to PID 1.
    /// If consistently > 5ms the spawn_blocking wrapper should be reconsidered.
    #[test]
    fn proc_walk_timing() {
        let own_pid = std::process::id();

        // Warm up (first read may be slower due to page cache cold start).
        let _ = is_game_running(own_pid);

        const RUNS: u32 = 100;
        let start = std::time::Instant::now();
        for _ in 0..RUNS {
            let _ = is_game_running(own_pid);
        }
        let total = start.elapsed();
        let avg_us = total.as_micros() / RUNS as u128;

        println!(
            "proc_walk_timing: {} runs, total={:.2}ms, avg={}µs per call",
            RUNS,
            total.as_secs_f64() * 1000.0,
            avg_us,
        );

        // Soft assertion — if this fires, re-evaluate spawn_blocking.
        assert!(
            avg_us < 5000,
            "proc walk took {}µs on average — consider spawn_blocking if > 5ms",
            avg_us
        );
    }
}
