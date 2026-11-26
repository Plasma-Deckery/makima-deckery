//! What a command does to the session. Pure.
//!
//! The session is either closed or open with a layer. A decision says what
//! the session becomes, what the sender is told, and what has to happen
//! outside: the readers re-resolve, the device session is rebuilt, or the
//! configs are read from disk again. `session` carries that out; nothing here
//! touches a lock, a socket or a file, so every transition is a unit test.
//!
//! Validating a layer needs the loaded configs, so it is passed in as a
//! function. The planner decides *when* a layer is validated — before it
//! replaces the live one, never after — and what an error does: nothing,
//! except being answered.

use super::layer::Refusal;
use super::{Layer, OverrideCommand};

/// The session as the planner sees it.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum State {
    #[default]
    Closed,
    Open { id: u64, layer: Layer },
}

/// What the planner is asked about.
#[derive(Debug, Clone, PartialEq)]
pub enum Input<'a> {
    Command(&'a OverrideCommand),
    /// The hard-wired combo was held long enough; see `emergency`.
    EmergencyExit,
}

/// The one line the sender gets back.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Ok,
    /// `begin` succeeded; the session's id.
    Began(u64),
    Error(String),
}

impl std::fmt::Display for Reply {
    /// `ok`, `ok <id>` or `error <message>`. A message that came from the TOML
    /// parser spans several lines, and the reply is one: line breaks become
    /// spaces.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reply::Ok => write!(f, "ok"),
            Reply::Began(id) => write!(f, "ok {id}"),
            Reply::Error(message) => {
                let flat: Vec<&str> = message.split_whitespace().collect();
                write!(f, "error {}", flat.join(" "))
            }
        }
    }
}

/// What has to happen outside the session for the decision to take effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Nothing changed.
    None,
    /// The live layer changed in what the readers resolve per event — the
    /// bindings. They re-resolve and publish.
    Refresh,
    /// The live layer changed in what a device session reads once at its
    /// start — trackpad modes, settings — so the device tasks are rebuilt,
    /// as a config reload rebuilds them.
    Relaunch,
    /// The session ended: the layer is dropped and the configs are read from
    /// disk, which also applies any file change the session held back.
    End,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// What the session becomes, or `None` to stay as it is.
    pub next: Option<State>,
    pub reply: Reply,
    pub effect: Effect,
}

impl Decision {
    fn refuse(message: impl Into<String>) -> Self {
        Self { next: None, reply: Reply::Error(message.into()), effect: Effect::None }
    }
}

/// Decide what `input` does to `state`.
///
/// `next_id` is the id a `begin` would hand out. `validate` is asked about a
/// changed layer before it is accepted, and its refusal is the reply, under
/// the path unless it names its own spot.
pub fn decide(
    state: &State,
    input: Input<'_>,
    next_id: u64,
    validate: impl FnOnce(&Layer) -> Result<(), Refusal>,
) -> Decision {
    let open = match state {
        State::Open { id, layer } => Some((*id, layer)),
        State::Closed => None,
    };
    let command = match input {
        Input::EmergencyExit => return match open {
            Some(_) => end(),
            None => Decision { next: None, reply: Reply::Ok, effect: Effect::None },
        },
        Input::Command(command) => command,
    };
    match (command, open) {
        (OverrideCommand::Malformed(why), _) => Decision::refuse(why.clone()),

        (OverrideCommand::Begin, Some((id, _))) => {
            Decision::refuse(format!("a session is already open (id {id})"))
        }
        (OverrideCommand::Begin, None) => Decision {
            next: Some(State::Open { id: next_id, layer: Layer::default() }),
            reply: Reply::Began(next_id),
            effect: Effect::Refresh,
        },

        (_, None) => Decision::refuse("no session is open"),

        (OverrideCommand::End, Some(_)) => end(),

        (OverrideCommand::Clear, Some((id, layer))) => {
            let next = layer.cleared();
            changed(id, layer, next)
        }

        (OverrideCommand::Set { path, value }, Some((id, layer))) => {
            let json: serde_json::Value = match serde_json::from_str(value) {
                Ok(json) => json,
                Err(e) => return Decision::refuse(format!("{path}: invalid JSON: {e}")),
            };
            let next = match super::path::json_to_toml(&json)
                .and_then(|value| layer.with_value(path, value))
            {
                Ok(next) => next,
                Err(e) => return Decision::refuse(format!("{path}: {e}")),
            };
            if let Err(refusal) = validate(&next) {
                return Decision::refuse(refusal.for_path(path));
            }
            changed(id, layer, next)
        }
    }
}

fn end() -> Decision {
    Decision { next: Some(State::Closed), reply: Reply::Ok, effect: Effect::End }
}

/// The session moving from `before` to `after`, and how far the change
/// reaches.
fn changed(id: u64, before: &Layer, after: Layer) -> Decision {
    let effect = if before.device_sections_differ(&after) { Effect::Relaunch } else { Effect::Refresh };
    Decision { next: Some(State::Open { id, layer: after }), reply: Reply::Ok, effect }
}

#[cfg(test)]
#[path = "planner_tests.rs"]
mod tests;
