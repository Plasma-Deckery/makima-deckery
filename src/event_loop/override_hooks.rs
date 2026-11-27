//! The reader's part in an override session.
//!
//! The session itself is `crate::override_session`; the layer it sets reaches
//! the reader through the registry, like any config. What is left for the
//! reader is what only the reader can do:
//!
//! * re-resolve and publish when the layer changes, so the HUD shows the new
//!   bindings before the next button is pressed;
//! * end Gaming Mode when a session opens, and keep its double-click trigger
//!   switched off while one is open — the tour runs on the desktop bindings;
//! * pass QAM presses to the session for the emergency exit, which works
//!   whatever the layer has cleared;
//! * say in the state export that a session is open, and that the Gaming
//!   Mode trigger is not there while it is.

use super::*;
use crate::config::DoubleclickTrigger;
use crate::override_session::emergency::EMERGENCY_EXIT_KEY;

impl EventReader {
    /// Apply every change to the live layer as it comes.
    ///
    /// A session may have opened while no reader was running — during a
    /// reinit, or with no controller connected — and the Gaming Mode flag is
    /// shared across reader generations. So a reader starting under an open
    /// session ends Gaming Mode first, before any change arrives.
    pub(super) async fn override_loop(&self) {
        let mut changes = self.override_session.subscribe();
        self.end_gaming_mode_for_session().await;
        while changes.changed().await.is_ok() {
            self.apply_override_change().await;
        }
    }

    /// One change to the live layer: end Gaming Mode if a session is open,
    /// then re-resolve, which publishes.
    pub(super) async fn apply_override_change(&self) {
        self.end_gaming_mode_for_session().await;
        self.update_config().await;
    }

    /// Ask for Gaming Mode off if a session is open and it is on.
    ///
    /// Through its channel, so the one setter switches it with its haptic and
    /// its announcement, however many readers ask.
    pub(super) async fn end_gaming_mode_for_session(&self) {
        if self.override_session.is_active() && *self.gaming_mode.lock().await {
            println!("deckery: override session open — ending Gaming Mode");
            let _ = self.gaming_mode_tx.try_send(false);
        }
    }

    /// The Gaming Mode double-click trigger, unless a session is open. With
    /// it gone the trigger button is an ordinary button again, and whatever
    /// the layer binds it to applies.
    pub(super) fn gaming_mode_trigger(&self) -> Option<&DoubleclickTrigger> {
        if self.override_session.is_active() {
            None
        } else {
            self.gaming_mode_config.trigger.as_ref()
        }
    }

    /// A press of the emergency exit's button, for the session to count.
    ///
    /// Every press is passed on, session or not; the session ignores them
    /// when it is closed. The window is the Gaming Mode trigger's, or its
    /// default when the trigger is switched off in the config.
    pub(super) fn note_emergency_press(&self, event: Event, value: i32) {
        if event != Event::Key(EMERGENCY_EXIT_KEY) || value != 1 {
            return;
        }
        let window_ms = self.gaming_mode_config.trigger.as_ref()
            .map_or(crate::config::DEFAULT_DOUBLE_CLICK_MS, |t| t.ms);
        self.override_session.emergency_press(
            tokio::time::Instant::now(), std::time::Duration::from_millis(window_ms));
    }

    /// `context.override_active` in a snapshot — and, while a session is
    /// open, no `gaming_mode_trigger`, since the trigger is off for the
    /// session and the HUD must not label the button with it.
    pub(super) fn mark_override(&self, state: &mut serde_json::Value) {
        let active = self.override_session.is_active();
        state["context"]["override_active"] = serde_json::Value::Bool(active);
        if active {
            state["gaming_mode_trigger"] = serde_json::Value::Null;
        }
    }
}
