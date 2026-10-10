//! The transformation core, one decision at a time.
//!
//! This is the machine issue #32 asks for: state and input come in, the new state
//! and a list of `OutputAction` go out, and nothing here touches a device, a
//! socket or a process. It is built beside the existing path rather than in place
//! of it — the old path stays the one that actually reaches the kernel, both run
//! on every event, and the two are compared. A disagreement is a finding with the
//! input that produced it, instead of a hypothesis.
//!
//! It is a state machine and not a function, deliberately: `modifier_was_activated`
//! is read and written inside the same loop that emits, so pretending the decision
//! is stateless would be wrong about the thing being modelled.
//!
//! Only `emit_event` is modelled so far. The rest follows once this one has run
//! quietly against the real path for a while.

use crate::config::{Config, Event};
use crate::output::action::{Ev, OutputAction, OutputDevice, Raw};

use evdev::{EventType, Key};
use std::sync::atomic::{AtomicU64, Ordering};

/// What one `emit_event` amounts to.
#[derive(Debug, Clone, PartialEq)]
pub struct EmitPlan {
    /// Frames, in the order the kernel should see them. One per `emit` call,
    /// because that is one kernel frame.
    pub frames: Vec<OutputAction>,
    /// Modifier bookkeeping to apply, in order. Separate from the frames because
    /// it is state, not output — and because the old path interleaves the two,
    /// which is what made it untestable.
    pub toggles: Vec<(Event, i32)>,
    /// `modifier_was_activated` after this decision.
    pub modifier_was_activated: bool,
}

/// One keyboard frame on the virtual keyboard.
fn key_frame(code: u16, value: i32) -> OutputAction {
    OutputAction::Frame {
        device: OutputDevice::Keys,
        events: vec![Ev { ev_type: EventType::KEY.0, code, value }],
    }
}

/// Every output key that the currently held modifier set resolves to.
///
/// Reads nothing but the config, so it was already pure — it just happened to be
/// an `async fn` on `&self`.
pub fn released_keys(modifiers: &[Event], config: &Config) -> Vec<Key> {
    let mut out: Vec<Key> = Vec::new();
    for hashmap in config.bindings.remap.values() {
        if let Some(event_list) = hashmap.get(modifiers) {
            out.extend(event_list);
        }
    }
    out
}

/// Decide what `emit_event` sends, without sending it.
///
/// Mirrors the three branches of the existing function exactly, including the
/// order the frames go out in — a plan that agreed about the set but not the
/// sequence would be agreeing about something the kernel does not see.
pub fn plan_emit(
    config: &Config,
    event_list: &[Key],
    value: i32,
    modifiers: &[Event],
    release_keys: bool,
    ignore_modifiers: bool,
    modifier_was_activated: bool,
) -> EmitPlan {
    let mut plan = EmitPlan {
        frames: Vec::new(),
        toggles: Vec::new(),
        modifier_was_activated,
    };

    if release_keys && value != 2 && !modifiers.is_empty() {
        // Release the output keys the held modifiers resolve to, but only those
        // that are modifiers themselves. With no modifiers held there is nothing
        // to clean up, and releasing everything would knock out a system modifier
        // that merely shares a key code with a base remap output.
        for key in released_keys(modifiers, config) {
            if config.mapped_modifiers.all.contains(&Event::Key(key)) {
                plan.toggles.push((Event::Key(key), 0));
                plan.frames.push(key_frame(key.code(), 0));
            }
        }
    } else if ignore_modifiers {
        // An explicit combo consumed its modifier, so the modifier's *output* key
        // is released before the combo's keys go out — not the raw input code.
        for modifier in modifiers {
            let codes: Vec<u16> = config
                .bindings
                .remap
                .get(modifier)
                .and_then(|m| m.get(&vec![]))
                .map(|keys| keys.iter().map(|k| k.code()).collect())
                .unwrap_or_else(|| match modifier {
                    Event::Key(k) => vec![k.code()],
                    _ => vec![],
                });
            for code in codes {
                plan.frames.push(key_frame(code, 0));
            }
        }
    }

    for key in event_list {
        if release_keys && value != 2 && config.mapped_modifiers.custom.contains(&Event::Key(*key))
        {
            // Only custom modifiers are tracked. Tracking a system modifier that
            // appears as a remap output would corrupt the set and break the next
            // combo lookup.
            plan.toggles.push((Event::Key(*key), value));
        }
        if config.mapped_modifiers.custom.contains(&Event::Key(*key)) {
            if value == 0 && !plan.modifier_was_activated {
                // Held and released without being used as a modifier, so it acts
                // as a tap: press and release, two frames, not one.
                plan.frames.push(key_frame(key.code(), 1));
                plan.frames.push(key_frame(key.code(), 0));
                plan.modifier_was_activated = true;
            } else if value == 1 {
                plan.modifier_was_activated = false;
            }
        } else {
            plan.frames.push(key_frame(key.code(), value));
            plan.modifier_was_activated = true;
        }
    }

    plan
}

/// Which virtual device a passed-through event belongs on.
///
/// The pen and the tablet share the `abs` device, which is why MISC lands there
/// too — a pen's `MSC_SERIAL` is part of the same report as its position.
fn device_for(event_type: EventType) -> Option<OutputDevice> {
    match event_type {
        EventType::KEY => Some(OutputDevice::Keys),
        EventType::RELATIVE => Some(OutputDevice::Axis),
        EventType::ABSOLUTE | EventType::MISC => Some(OutputDevice::Abs),
        // Anything else is not forwarded. The incoming SYN in particular is
        // consumed for rate limiting and never passed on, because `emit`
        // appends one of its own.
        _ => None,
    }
}

/// Decide what `emit_default_event` sends: the event itself, or nothing.
///
/// The whole decision is which device an untouched event belongs on, and whether
/// a pause swallows it. It is modelled anyway, because it is the function where
/// forwarding *is* the job — and therefore the one that shows whether `Forward`
/// keeping the hardware timestamp is right.
pub fn plan_default(event: &evdev::InputEvent, paused: bool) -> Vec<OutputAction> {
    if paused {
        return Vec::new();
    }
    match device_for(event.event_type()) {
        Some(device) => vec![OutputAction::Forward { device, events: vec![Raw(*event)] }],
        None => Vec::new(),
    }
}

/// What the old path just sent, in the new vocabulary, so the two can be compared.
pub fn recorded(code: u16, value: i32) -> OutputAction {
    key_frame(code, value)
}

/// The same, for an event the old path passed straight through.
pub fn forwarded(device: OutputDevice, event: evdev::InputEvent) -> OutputAction {
    OutputAction::Forward { device, events: vec![Raw(event)] }
}

/// Per-site tally of how much has been compared, and how much disagreed.
///
/// One of these per call site rather than one shared between them. A shared
/// counter answers "how much of the input path has been checked", which sounds
/// like the interesting number but is not: the question that actually comes up
/// is whether *this* function ran at all, and a shared counter cannot say.
pub struct Tally {
    compared: AtomicU64,
    reported: AtomicU64,
}

impl Tally {
    pub const fn new() -> Self {
        Self { compared: AtomicU64::new(0), reported: AtomicU64::new(0) }
    }
}

/// Compare a plan against what the old path actually sent.
pub fn compare_actions(
    tally: &Tally,
    what: &str,
    planned: &[OutputAction],
    performed: &[OutputAction],
    input: &str,
) {
    let plan = EmitPlan {
        frames: planned.to_vec(),
        toggles: Vec::new(),
        modifier_was_activated: false,
    };
    compare(tally, what, &plan, performed, false, input);
}

/// Compare the two machines and say so when they disagree.
///
/// Rate-limited and one line per kind of disagreement rather than per press: a
/// mismatch that fires on every keystroke would bury the first and only
/// interesting instance of it, and at 60 Hz it would bury the journal too.
///
/// Silent when they agree, which is meant to be always — this exists to be
/// boring, and a line from it is a finding.
pub fn compare(
    tally: &Tally,
    what: &str,
    planned: &EmitPlan,
    performed: &[OutputAction],
    modifier_was_activated: bool,
    input: &str,
) {
    const MAX: u64 = 50;
    const EVERY: u64 = 20;

    // Silence on agreement is what makes a line from this worth reading — but it
    // also makes "they never disagreed" and "this never ran" look identical. So
    // the count is reported now and then: without it, a quiet journal is not
    // evidence of anything.
    let compared = tally.compared.fetch_add(1, Ordering::Relaxed) + 1;
    // The first one too, not only every twentieth: the useful question early on
    // is whether this path runs at all, and waiting for a round number to answer
    // it is what made the first attempt say nothing.
    if compared == 1 || compared % EVERY == 0 {
        println!("deckery: {what}: {compared} decisions compared, {} disagreements",
                 tally.reported.load(Ordering::Relaxed));
    }

    let frames_agree = planned.frames == performed;
    let state_agrees = planned.modifier_was_activated == modifier_was_activated;
    if frames_agree && state_agrees {
        return;
    }
    if tally.reported.fetch_add(1, Ordering::Relaxed) >= MAX {
        return;
    }

    eprintln!("deckery: {what}: the two machines disagree
  input:     {input}");
    if !frames_agree {
        eprintln!("  planned:   {:?}
  performed: {:?}", planned.frames, performed);
    }
    if !state_agrees {
        eprintln!("  modifier_was_activated: planned {}, actual {}",
                  planned.modifier_was_activated, modifier_was_activated);
    }
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;
