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
//! * watch the raw buttons for the emergency exit, which works whatever the
//!   layer has cleared;
//! * say in the state export that a session is open, and that the Gaming
//!   Mode trigger is not there while it is.

use super::*;
use crate::config::DoubleclickTrigger;
use crate::override_session::emergency::EMERGENCY_EXIT_HOLD;

impl EventReader {
    /// Apply every change to the live layer as it comes.
    pub(super) async fn override_loop(&self) {
        let mut changes = self.override_session.subscribe();
        while changes.changed().await.is_ok() {
            self.apply_override_change().await;
        }
    }

    /// One change to the live layer: end Gaming Mode if a session is open,
    /// then re-resolve, which publishes.
    ///
    /// Gaming Mode goes off through its channel, so the one setter switches it
    /// with its haptic and its announcement, however many readers see the
    /// change.
    pub(super) async fn apply_override_change(&self) {
        if self.override_session.is_active() && *self.gaming_mode.lock().await {
            println!("deckery: override session open — ending Gaming Mode");
            let _ = self.gaming_mode_tx.try_send(false);
        }
        self.update_config().await;
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

    /// Record one key edge for the emergency exit.
    pub(super) fn track_emergency_combo(&self, event: Event, value: i32) {
        let Event::Key(key) = event else { return };
        let changed = self.emergency_combo.lock().unwrap().update(key, value);
        if let Some(held) = changed {
            self.emergency_held.send_replace(held);
        }
    }

    /// End the session once the emergency combo has been held for
    /// `EMERGENCY_EXIT_HOLD`.
    ///
    /// A timer rather than a check on the next edge: the point of holding is
    /// that nothing else is pressed. Letting go of either button before the
    /// time is up starts over.
    pub(super) async fn emergency_exit_loop(&self) {
        let mut held = self.emergency_held.subscribe();
        loop {
            while !*held.borrow_and_update() {
                if held.changed().await.is_err() { return; }
            }
            tokio::select! {
                _ = tokio::time::sleep(EMERGENCY_EXIT_HOLD) => {
                    if *held.borrow() {
                        self.override_session.emergency_exit();
                    }
                    // One exit per hold: the next one needs a release first.
                    while *held.borrow_and_update() {
                        if held.changed().await.is_err() { return; }
                    }
                }
                changed = held.changed() => {
                    if changed.is_err() { return; }
                }
            }
        }
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
