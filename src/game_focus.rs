//! Which game counts as focused — once, for everything that cares.
//!
//! Two things react to a game gaining or losing focus: Gaming Mode
//! auto-detection (`steam_detector`) and muting (`auto_mute`). They used to read
//! raw focus each. Raw focus is too eager: switching desktops with L1+D-pad
//! passes over a game's window on the way, and that instant was enough to turn
//! Gaming Mode on — which switches off makima's bindings, including the L1+D-pad
//! being used to move on. One pass over the game's desktop left the controller
//! stuck.
//!
//! So a game only counts as focused once it has **kept** focus for
//! `[gaming_mode] focus_threshold_ms`. Losing focus counts at once; waiting on
//! the way out would leave the controller raw on the desktop for the length of
//! the threshold.
//!
//! The answer is decided here and published on a `watch` channel, and both
//! consumers subscribe to it. With one source they cannot disagree — Gaming Mode
//! cannot switch on while the game is still muted, or the other way round.
//!
//! Per-app config switching does **not** go through here. Which bindings apply
//! to the focused window should follow focus at once; only game-ness waits.

use crate::session::Client;
use crate::steam_detector::{game_reaper, is_steam_bpm};

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Mutex, Notify};
use tokio::time::Instant;

/// What the focused window is, as far as games are concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GameFocus {
    /// The focused game, by the PID of its `reaper`.
    pub game: Option<u32>,
    /// Steam Big Picture Mode has focus.
    pub big_picture: bool,
}

impl GameFocus {
    /// Whether this focus should turn Gaming Mode on.
    pub fn is_gaming(&self) -> bool {
        self.game.is_some() || self.big_picture
    }
}

/// How long to wait before publishing `focus`, or `None` to publish at once.
///
/// Only a focus that would count as gaming waits — anything else is the user
/// leaving, and leaving must take effect immediately.
pub fn wait_before_publishing(focus: GameFocus, threshold: Duration) -> Option<Duration> {
    (focus.is_gaming() && !threshold.is_zero()).then_some(threshold)
}

/// Read the focused window and classify it.
async fn current(active_client: &Mutex<Client>) -> GameFocus {
    match &*active_client.lock().await {
        Client::Class(class, caption, pid) => GameFocus {
            game: pid.and_then(game_reaper),
            big_picture: is_steam_bpm(class, caption),
        },
        Client::Default => GameFocus::default(),
    }
}

/// Runs for the life of the process and keeps `tx` at the settled focus.
///
/// `threshold` is asked on every focus change rather than captured once, so a
/// config reload changes it without restarting anything.
pub async fn game_focus_task(
    window_changed: Arc<Notify>,
    active_client: Arc<Mutex<Client>>,
    threshold: impl Fn() -> Duration,
    tx: watch::Sender<GameFocus>,
) {
    // Registered before the first wait and re-armed before the work, for the
    // reason given in steam_detection_task: notify_waiters() keeps no permit.
    let mut notified = Box::pin(window_changed.notified());
    notified.as_mut().enable();

    // A gaming focus that is still waiting out the threshold.
    let mut pending: Option<(GameFocus, Instant)> = None;

    loop {
        let focus_changed = match pending {
            Some((_, deadline)) => tokio::select! {
                _ = notified.as_mut() => true,
                _ = tokio::time::sleep_until(deadline) => false,
            },
            None => {
                notified.as_mut().await;
                true
            }
        };

        if !focus_changed {
            // The pending game kept focus for the whole threshold.
            let (focus, _) = pending.take().expect("woke by deadline, so one was pending");
            tx.send_if_modified(|old| std::mem::replace(old, focus) != focus);
            continue;
        }

        notified = Box::pin(window_changed.notified());
        notified.as_mut().enable();

        let focus = current(&active_client).await;
        // Any focus change cancels a pending one — the game did not keep focus.
        pending = None;
        match wait_before_publishing(focus, threshold()) {
            Some(wait) => pending = Some((focus, Instant::now() + wait)),
            None => {
                tx.send_if_modified(|old| std::mem::replace(old, focus) != focus);
            }
        }
    }
}

#[cfg(test)]
#[path = "game_focus_tests.rs"]
mod tests;
