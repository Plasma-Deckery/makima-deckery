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
use crate::override_session::emergency::{EMERGENCY_EXIT_COMBO, EMERGENCY_EXIT_HOLD};

impl EventReader {
    /// Apply every change to the live layer as it comes.
    ///
    /// A session may have opened while no reader was running — during a
    /// reinit, or with no controller connected — and the Gaming Mode flag is
    /// shared across reader generations. So a reader starting under an open
    /// session ends Gaming Mode first, before any change arrives.
    pub(super) async fn override_loop(&self) {
        let mut changes = self.override_session.subscribe();
        if self.override_session.is_active() {
            // The new reader's combo tracker is empty, and the controller
            // reports a button already held only once it is let go — see
            // `track_emergency_combo`. Said so the journal shows it.
            println!("deckery: override: reader started under an open session — \
                      the emergency combo has to be pressed anew");
        }
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

    /// Record one key edge for the emergency exit.
    ///
    /// The tracker belongs to this reader and starts empty with it. A rebuild
    /// of the device tasks — a reinit, a trackpad mode change — therefore
    /// forgets a combo held across it, and the hold has to start again. That
    /// is left so on purpose: a rebuild is rare, short, and announced by the
    /// pads resetting, and the next press of either button rearms it.
    ///
    /// While a session is open every edge of the combo's buttons is logged,
    /// with what the tracker holds afterwards. A press costs one line, and
    /// only during the tour; it is what tells, from the journal alone, whether
    /// the buttons reached the reader at all.
    pub(super) fn track_emergency_combo(&self, event: Event, value: i32) {
        let Event::Key(key) = event else { return };
        let (changed, held) = {
            let mut tracker = self.emergency_combo.lock().unwrap();
            let changed = tracker.update(key, value);
            (changed, tracker.held_keys())
        };
        if self.override_session.is_active() && EMERGENCY_EXIT_COMBO.contains(&key) && value != 2 {
            println!("deckery: override: emergency combo: {key:?} {} (held: {held:?})",
                if value == 1 { "down" } else { "up" });
        }
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
            let since = tokio::time::Instant::now();
            let session = self.override_session.is_active();
            if session {
                println!("deckery: override: emergency exit armed — hold for {} s",
                    EMERGENCY_EXIT_HOLD.as_secs());
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
                    if session {
                        println!("deckery: override: emergency exit released after {} ms — not triggered",
                            since.elapsed().as_millis());
                    }
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
