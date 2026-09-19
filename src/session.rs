//! What desktop session this is, and what is focused in it.
//!
//! These three types say nothing about input devices, yet they used to live in
//! `udev_monitor` because that is where they were first written. Eight modules
//! imported them from there — and not one of them wanted a udev function. The
//! cost was three dependency cycles, the worst of which pointed `compositor/`,
//! a self-contained adapter package, back at the top of the module tree.
//!
//! This module therefore has no dependencies of its own and never will: it is
//! the vocabulary the compositor adapters, the config registry and the event
//! reader share, and nothing more.

use std::env;

/// The focused window, as far as anything outside the compositor can tell.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub enum Client {
    #[default]
    Default,
    /// Window class + caption (both forwarded raw by the KWin script) +
    /// the PID of the focused window's owning process (KDE only; None elsewhere).
    Class(String, String, Option<u32>),
}

/// Which display server was found, if any.
#[derive(Clone)]
pub enum Server {
    Connected(String),
    Unsupported,
    Failed,
}

/// The process environment the readers are started with.
#[derive(Clone)]
pub struct Environment {
    pub user: Result<String, env::VarError>,
    pub sudo_user: Result<String, env::VarError>,
    pub server: Server,
}
