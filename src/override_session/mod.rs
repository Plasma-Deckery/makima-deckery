//! The override session: a guided tour sets live bindings from outside.
//!
//! The onboarding tour unlocks the controller step by step — first only the
//! right pad and the triggers, then Enter and Esc, and so on — so it has to set
//! the bindings that are live while it runs, and everything has to fall back to
//! the user's own config when it ends, however it ends.
//!
//! While a session is open, one more layer sits on top of whatever the registry
//! resolves (base, modules, the focused app's override), so switching windows
//! cannot escape it. The layer is a config document in TOML form, built up one
//! `override set` at a time and parsed by the same code a config file is. When
//! the session ends the layer is dropped and the configs are read from disk
//! again, exactly as after a file change.
//!
//! * `command` — what follows `override` on the control socket
//! * `path` — a dotted path and a JSON value, as a change to the document
//! * `layer` — the document, and laying it over a resolved config
//! * `allowlist` — which `[commands]` an override may bind
//! * `checks` — refusing the fallbacks a file would only warn about
//! * `planner` — what a command does to the session; pure
//! * `emergency` — the hard-wired combo that always ends a session
//! * `session` — the shared handle that holds the state and carries out the
//!   planner's decisions
//!
//! The session lives in memory only. A makima restart starts without one, and
//! the tour, which talks to the new process, finds no session open.

pub mod allowlist;
mod checks;
pub mod command;
pub mod emergency;
pub mod layer;
pub mod path;
pub mod planner;
pub mod session;

pub use command::OverrideCommand;
pub use layer::Layer;
pub use session::{DeviceRequest, OverrideSession};
