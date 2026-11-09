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

use tokio::sync::{mpsc, watch};

/// Returns `true` if the focused window is Steam Big Picture Mode.
///
/// BPM is identified by: class == "steam" AND caption contains "Big Picture"
/// or "Big-Picture" (KWin uses either form depending on the Steam version).
pub fn is_steam_bpm(class_name: &str, caption: &str) -> bool {
    class_name == "steam"
        && (caption.contains("Big Picture") || caption.contains("Big-Picture"))
}

/// The `reaper` a process runs under, if it is part of a Steam game.
///
/// Walks upward through `/proc/{pid}/status` until it finds `reaper → steam`.
/// It answers *which* game rather than whether there is one: every process a
/// game starts — Proton, wine, the game
/// binary, the thread that opens the audio stream — sits below one `reaper`,
/// so its PID is what identifies the game as a whole. Two processes belong to
/// the same game exactly when this returns the same value for both.
pub fn game_reaper(pid: u32) -> Option<u32> {
    let mut current = pid;
    loop {
        let (name, ppid) = read_proc_status(current)?; // process disappeared
        if ppid <= 1 {
            return None; // reached init without finding steam/reaper
        }
        if name == "reaper" {
            // Confirm the reaper's parent is steam.
            let (parent_name, _) = read_proc_status(ppid)?;
            return (parent_name == "steam").then_some(current);
        }
        current = ppid;
    }
}

/// Read `Name:` and `PPid:` from `/proc/{pid}/status`.
/// Returns `None` if the file cannot be read (process gone).
pub(crate) fn read_proc_status(pid: u32) -> Option<(String, u32)> {
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

/// Async loop that turns the settled game focus into the desired Gaming Mode
/// state (`true` = gaming) on `tx`.
///
/// Spawned once per device generation from `launch_tasks`. It no longer reads
/// raw focus itself: `game_focus` publishes which game counts as focused, after
/// `focus_threshold_ms`, and this only maps that onto Gaming Mode. Only sends
/// when the state actually changes to avoid flooding the channel.
///
/// The value already in the channel when this starts is deliberately *not*
/// acted on. A reinit — every config reload is one — restarts this task, and
/// re-sending the current focus would undo a Gaming Mode the user had just
/// switched off by hand. It waits for the next real change, as it always did.
///
/// When `auto_detect` is false the task still runs but never sends `true` —
/// Gaming Mode can only be set via the manual double-click trigger or IPC.
pub async fn steam_detection_task(
    mut game_focus: watch::Receiver<crate::game_focus::GameFocus>,
    tx: mpsc::Sender<bool>,
    auto_detect: bool,
) {
    let mut last_sent: Option<bool> = None;
    game_focus.borrow_and_update();

    while game_focus.changed().await.is_ok() {
        let new_state = auto_detect && game_focus.borrow_and_update().is_gaming();
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
        assert!(!is_steam_bpm("", "") && game_reaper(1).is_none());
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

    use crate::game_focus::GameFocus;

    const BIG_PICTURE: GameFocus = GameFocus { game: None, big_picture: true };

    /// Start the task, then publish `focus` as a real change after it.
    async fn run(auto_detect: bool, focus: GameFocus) -> Option<bool> {
        let (focus_tx, focus_rx) = watch::channel(GameFocus::default());
        let (tx, mut rx) = mpsc::channel::<bool>(8);
        tokio::spawn(steam_detection_task(focus_rx, tx, auto_detect));
        tokio::task::yield_now().await;
        focus_tx.send(focus).unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        rx.try_recv().ok()
    }

    /// When `auto_detect_steam_games = false` the task must never send `true`,
    /// even when the focused window is exactly Steam Big Picture Mode.
    #[tokio::test]
    async fn auto_detect_false_never_sends_true() {
        assert_eq!(run(false, BIG_PICTURE).await, Some(false));
    }

    /// When `auto_detect_steam_games = true` the task must send `true` when the
    /// settled focus is Steam Big Picture Mode.
    #[tokio::test]
    async fn auto_detect_true_sends_true_for_bpm() {
        assert_eq!(run(true, BIG_PICTURE).await, Some(true));
    }

    /// What is already in the channel when the task starts is not acted on. A
    /// reinit restarts this task, and re-sending the current focus would switch
    /// back on a Gaming Mode the user had just turned off by hand.
    #[tokio::test]
    async fn the_focus_present_at_start_is_not_acted_on() {
        let (_focus_tx, focus_rx) = watch::channel(BIG_PICTURE);
        let (tx, mut rx) = mpsc::channel::<bool>(8);
        tokio::spawn(steam_detection_task(focus_rx, tx, true));
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        assert!(rx.try_recv().is_err(), "acted on the focus it found at start");
    }

    // Live process-tree test: only verifies no panic; whether a game is
    // running is non-deterministic in CI.
    #[test]
    fn pid1_is_not_part_of_a_game() {
        assert_eq!(game_reaper(1), None);
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
        let _ = game_reaper(own_pid);

        const RUNS: u32 = 100;
        let start = std::time::Instant::now();
        for _ in 0..RUNS {
            let _ = game_reaper(own_pid);
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
