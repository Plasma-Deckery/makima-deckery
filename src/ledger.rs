//! What makima holds down, and the one place a key-up comes from.
//!
//! Every path that presses an output key records it here against the input
//! that pressed it — a remap, a combo, a Hold, an axis edge, the modifier
//! intercept, a passed-through key. Every key-up is read back from here: a
//! release lifts what its own press recorded, a pause lifts everything, and a
//! cleanup before a press lifts only what is actually down. Nothing re-resolves
//! a binding to find out what to release, because what a button resolves to
//! *now* is not what it pressed when the modifiers changed in between — that
//! was the bug class: stray key-ups for keys never pressed, key-ups sent twice,
//! and keys left down because no path had recorded them.
//!
//! Pure, like `machine`: the reader keeps the `Ledger` behind a lock and hands
//! it in; nothing here sends anything.
//!
//! No ref-counting. When two held buttons press the same output key, the first
//! release lifts it for both: a key code is either up or down on the virtual
//! keyboard, and the old path behaved the same way. The ledger keeps its
//! invariant instead — a key-up removes the key from every entry, so what is
//! recorded is always a key that is down and a second release sends nothing.
//! Counting would only matter for two buttons bound to one key held at once,
//! which no shipped config does.

use crate::config::Event;
use crate::machine::{key_at, key_up};
use crate::output::action::OutputAction;

use evdev::Key;
use std::collections::HashMap;

/// What one press left behind, for its release to undo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pressed {
    /// Output keys that went down.
    pub keys: Vec<Key>,
    /// Custom-modifier outputs the press armed instead of pressing. Their
    /// release is the binding's own effect: a tap if nothing used them.
    pub armed: Vec<Key>,
    /// Whether the press also tracked `armed` in the modifier set — a base
    /// binding does, a combo does not — so the release untracks them.
    pub tracked: bool,
}

impl Pressed {
    fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.armed.is_empty()
    }
}

/// Every input currently holding something down, and what.
pub type Ledger = HashMap<Event, Pressed>;

/// Record what a press did. Merged into what is already there, so a press
/// whose release was never seen cannot hide the keys it left down.
pub fn record(ledger: &mut Ledger, event: Event, pressed: Pressed) {
    if pressed.is_empty() {
        return;
    }
    let entry = ledger.entry(event).or_default();
    for key in pressed.keys {
        if !entry.keys.contains(&key) {
            entry.keys.push(key);
        }
    }
    for key in pressed.armed {
        if !entry.armed.contains(&key) {
            entry.armed.push(key);
        }
    }
    entry.tracked |= pressed.tracked;
}

/// Plain output keys at `value`, as frames, recorded on a press. The modifier
/// intercept's whole output.
pub fn press(ledger: &mut Ledger, event: Event, keys: &[Key], value: i32) -> Vec<OutputAction> {
    if value == 1 {
        record(ledger, event, Pressed { keys: keys.to_vec(), ..Default::default() });
    }
    keys.iter().map(|key| key_at(*key, value)).collect()
}

/// Key-ups for those of `keys` that are down, in the order asked for, and the
/// ledger forgets them wherever they are recorded. A key that is not down gets
/// no key-up — this is what a cleanup before a press uses.
pub fn release_keys(ledger: &mut Ledger, keys: &[Key]) -> Vec<OutputAction> {
    let mut frames = Vec::new();
    for key in keys {
        let mut was_down = false;
        for entry in ledger.values_mut() {
            let before = entry.keys.len();
            entry.keys.retain(|k| k != key);
            was_down |= entry.keys.len() != before;
        }
        if was_down {
            frames.push(key_up(*key));
        }
    }
    ledger.retain(|_, entry| !entry.is_empty());
    frames
}

/// Key-ups for every key down. The armed modifiers stay recorded: a pause
/// releases outputs but keeps what is held (issue #62), and the button's own
/// release still has to untrack them. The order is the map's, which is
/// arbitrary — every frame is a release, so no order means anything here.
pub fn release_all(ledger: &mut Ledger) -> Vec<OutputAction> {
    let keys: Vec<Key> = ledger.values().flat_map(|entry| entry.keys.clone()).collect();
    release_keys(ledger, &keys)
}

/// What one release amounts to.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleasePlan {
    pub frames: Vec<OutputAction>,
    /// Modifiers to untrack, as `toggle_modifiers` takes them.
    pub toggles: Vec<(Event, i32)>,
    pub modifier_was_activated: bool,
}

/// Undo what `event`'s press did: key-ups for exactly the keys it pressed, and
/// the release's own effects — the armed modifiers untracked, and the tap of an
/// armed modifier nothing used. Every release goes through here, whatever the
/// event resolves to now and whatever branch it takes after.
pub fn plan_release(ledger: &mut Ledger, event: Event, modifier_was_activated: bool) -> ReleasePlan {
    let mut plan = ReleasePlan { frames: Vec::new(), toggles: Vec::new(), modifier_was_activated };
    let Some(pressed) = ledger.remove(&event) else {
        return plan;
    };
    // Put back for a moment so `release_keys` lifts them here and wherever
    // else they are recorded, which keeps every entry a key that is down.
    ledger.insert(event, Pressed { keys: pressed.keys.clone(), ..Default::default() });
    plan.frames = release_keys(ledger, &pressed.keys);
    for key in &pressed.armed {
        if pressed.tracked {
            plan.toggles.push((Event::Key(*key), 0));
        }
        if !plan.modifier_was_activated {
            // Held and released without being used as a modifier, so it acts
            // as a tap: press and release, two frames, not one.
            plan.frames.push(key_at(*key, 1));
            plan.frames.push(key_up(*key));
            plan.modifier_was_activated = true;
        }
    }
    plan
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;
