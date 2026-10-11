//! The transformation core, one decision at a time.
//!
//! This is the machine issue #32 asks for: state and input come in, the new state
//! and a list of `OutputAction` go out, and nothing here touches a device, a
//! socket or a process. The event reader applies what comes out; it no longer
//! decides anything these functions decide.
//!
//! It is a state machine and not a function, deliberately. `modifier_was_activated`
//! and the acceleration ramp are read and written by the same step that emits, so
//! pretending a decision here is stateless would be wrong about the thing being
//! modelled — and the loop that feeds state back in is what makes a mode change
//! observable as an action before it is believed as state.
//!
//! It was built beside the old path first and run against it: 380 real decisions
//! through `emit_event` with no disagreement, which is what justified replacing
//! the old half rather than keeping both. The comparison machinery is gone with
//! it — keeping a second implementation of every branch in order to check the
//! first is only worth it until the check has passed.
//!
//! Still performed where it is decided, and so still untestable: `convert_event`
//! itself, `emit_nonmapped_event`, and the trackpad path.

use crate::config::{Config, Event};
use crate::output::action::{Ev, OutputAction, OutputDevice, Raw};

use evdev::{EventType, Key};

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

/// One key at a given value, as a frame.
///
/// The three cleanup paths in the reader all amount to this, and all three used
/// to spell out the same `InputEvent::new_now` by hand.
pub fn key_at(key: Key, value: i32) -> OutputAction {
    key_frame(key.code(), value)
}

/// One key released.
pub fn key_up(key: Key) -> OutputAction {
    key_frame(key.code(), 0)
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

/// What one `emit_nonmapped_event` amounts to.
#[derive(Debug, Clone, PartialEq)]
pub struct NonmappedPlan {
    pub frames: Vec<OutputAction>,
    /// In order, and including the event's own toggle, which happens
    /// unconditionally between the released keys and whatever is emitted.
    pub toggles: Vec<(Event, i32)>,
    pub modifier_was_activated: bool,
}

/// Decide what an event with no binding of its own amounts to.
///
/// This is the `Unbound` tail of the resolver: the event is passed through, but
/// first any modifier output the held set resolves to is released, and a custom
/// modifier released unused is turned into a tap instead.
///
/// One quirk is preserved rather than corrected: the tap is synthesised from
/// `default_event`'s type and code but always goes to the keyboard device, even
/// when that type is not a key. Changing it would be a behaviour change, and
/// nothing is known to depend on it either way.
pub fn plan_nonmapped(
    config: &Config,
    event: Event,
    default_event: &evdev::InputEvent,
    value: i32,
    modifiers: &[Event],
    modifier_was_activated: bool,
) -> NonmappedPlan {
    let mut plan = NonmappedPlan {
        frames: Vec::new(),
        toggles: Vec::new(),
        modifier_was_activated,
    };

    if config.mapped_modifiers.all.contains(&event) && value != 2 {
        for key in released_keys(modifiers, config) {
            plan.toggles.push((Event::Key(key), 0));
            plan.frames.push(key_frame(key.code(), 0));
        }
    }
    // Always, and after the released keys — the event's own tracking does not
    // depend on them and they do not depend on it.
    plan.toggles.push((event, value));

    if config.mapped_modifiers.custom.contains(&event) {
        if value == 0 && !plan.modifier_was_activated {
            plan.frames.push(key_frame(default_event.code(), 1));
            plan.frames.push(key_frame(default_event.code(), 0));
            plan.modifier_was_activated = true;
        } else if value == 1 {
            plan.modifier_was_activated = false;
        }
    } else {
        plan.modifier_was_activated = true;
        if let Some(device) = device_for(default_event.event_type()) {
            plan.frames.push(OutputAction::Forward {
                device,
                events: vec![Raw(*default_event)],
            });
        }
    }

    plan
}

/// One relative-axis frame on the virtual pointer.
fn axis_frame(code: u16, value: i32) -> OutputAction {
    OutputAction::Frame {
        device: OutputDevice::Axis,
        events: vec![Ev { ev_type: EventType::RELATIVE.0, code, value }],
    }
}

/// The axis codes a loop writes to. `REL_X`/`REL_Y` for a pointer,
/// `REL_HWHEEL`/`REL_WHEEL` for scrolling — and note that the scroll pair is
/// stored X-then-Y but the codes run 12 then 11, which is not a typo in either
/// place.
pub const CURSOR_AXES: (u16, u16) = (0, 1);
pub const SCROLL_AXES: (u16, u16) = (12, 11);

/// One tick of an analog stick loop — `cursor_loop` or `scroll_loop`.
///
/// Both send X and Y as two separate frames, always both, never merged: the
/// kernel reads two motions rather than one diagonal, and that is observable.
/// The two loops differ in nothing but their axis codes and which invert setting
/// applies, which is why one function covers both.
pub fn plan_stick_tick(
    axes: (u16, u16),
    position: (i32, i32),
    invert: bool,
    activation_modifiers: &[Event],
    modifiers: &[Event],
    active: bool,
) -> Vec<OutputAction> {
    // A stick at rest sends nothing at all, rather than a stream of zeroes.
    if position == (0, 0) {
        return Vec::new();
    }
    // An empty activation set means "always"; a non-empty one has to match
    // exactly, so holding an extra modifier switches the stick off.
    if !activation_modifiers.is_empty() && activation_modifiers != modifiers {
        return Vec::new();
    }
    if !active {
        return Vec::new();
    }
    let (x, y) = if invert { (-position.0, -position.1) } else { position };
    vec![axis_frame(axes.0, x), axis_frame(axes.1, y)]
}

/// One tick of a key-driven loop — `key_cursor_loop` or `key_scroll_loop`.
///
/// Returns the frames and the new speed, because the acceleration ramp is state:
/// it builds while a direction is held and collapses the moment nothing is.
///
/// Unlike the analog loops, each axis is sent only when it is actually moving,
/// so a tick can produce two frames, one, or none.
pub fn plan_key_tick(
    axes: (u16, u16),
    movement: (i32, i32),
    speed: i32,
    acceleration: f32,
    current_speed: f32,
    active: bool,
) -> (Vec<OutputAction>, f32) {
    if movement == (0, 0) {
        // Released: the ramp starts from nothing next time.
        return (Vec::new(), 0.0);
    }
    let mut current_speed = current_speed + speed as f32 * acceleration / 10.0;
    if current_speed > speed as f32 {
        current_speed = speed as f32;
    }
    if !active {
        // The ramp still builds while suppressed, as it does today — only the
        // sending is skipped.
        return (Vec::new(), current_speed);
    }
    let mut frames = Vec::new();
    if movement.0 != 0 {
        frames.push(axis_frame(axes.0, movement.0 * current_speed as i32));
    }
    if movement.1 != 0 {
        frames.push(axis_frame(axes.1, movement.1 * current_speed as i32));
    }
    (frames, current_speed)
}

#[cfg(test)]
#[path = "machine_tests.rs"]
mod tests;
