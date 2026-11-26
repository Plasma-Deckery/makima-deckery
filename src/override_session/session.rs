//! The shared handle: the session's state, and carrying out the planner.
//!
//! One per process, created in `main` next to the registry and handed to the
//! control socket, the device loop and every reader. The control socket asks
//! it to `handle` a command and writes back the reply; a reader asks it for
//! the emergency exit and whether a session is open; the device loop asks it
//! whether a file change has to wait, and receives the device work it cannot
//! do itself.
//!
//! What it does about a decision:
//!
//! * the live layer goes into the registry, which lays it over every config it
//!   resolves from then on;
//! * the readers are told through `subscribe`, re-resolve and publish;
//! * a rebuild of the device tasks, or a reload from disk followed by one, is
//!   sent to the device loop, which owns the tasks.

use super::planner::{self, Effect, Input, Reply, State};
use super::{Layer, OverrideCommand};
use crate::config::Config;
use crate::config_registry::ConfigRegistry;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch};

/// Work only the device loop can do, because it owns the device tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRequest {
    /// Rebuild the device tasks from the registry as it stands — what a
    /// trackpad mode change needs.
    Relaunch,
    /// Read the configs from disk, then rebuild — what a file change does.
    Reload,
}

struct Inner {
    state: State,
    /// The id the next `begin` hands out. Starts at 1 with every process, so
    /// an id says nothing across a restart — and there is no session to name
    /// across one either.
    next_id: u64,
}

pub struct OverrideSession {
    registry: Arc<ConfigRegistry>,
    inner: Mutex<Inner>,
    /// Whether `inner.state` is open, readable without its lock. The readers
    /// ask on every button press, and `run` holds the lock while it validates
    /// a layer against every base config; a press must not wait for that.
    /// Written under the lock, after the state, so it never runs ahead of it.
    active: AtomicBool,
    /// Bumped on every change to the live layer, including its end.
    changed: watch::Sender<u64>,
    devices: mpsc::UnboundedSender<DeviceRequest>,
}

impl OverrideSession {
    /// A closed session over `registry`, and the receiver of the device work
    /// it will ask for.
    pub fn new(registry: Arc<ConfigRegistry>) -> (Arc<Self>, mpsc::UnboundedReceiver<DeviceRequest>) {
        let (devices, device_rx) = mpsc::unbounded_channel();
        let (changed, _) = watch::channel(0);
        let session = Arc::new(Self {
            registry,
            inner: Mutex::new(Inner { state: State::Closed, next_id: 1 }),
            active: AtomicBool::new(false),
            changed,
            devices,
        });
        (session, device_rx)
    }

    /// Carry out one override command and say what to answer.
    pub fn handle(&self, command: &OverrideCommand) -> Reply {
        self.run(Input::Command(command), command_name(command))
    }

    /// The emergency combo was held: end the session if one is open.
    pub fn emergency_exit(&self) {
        if self.is_active() {
            eprintln!("deckery: override: emergency exit held — ending the session");
        }
        self.run(Input::EmergencyExit, "emergency exit");
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Changes to the live layer. The value is a counter and means nothing on
    /// its own; a change is the signal.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// A config file changed. While a session is open the reload waits, and
    /// this says so: the end of the session reads the files anyway.
    pub fn defers_file_reload(&self) -> bool {
        let deferred = self.is_active();
        if deferred {
            println!("deckery: override: config files changed — reload deferred until the session ends");
        }
        deferred
    }

    fn run(&self, input: Input<'_>, what: &str) -> Reply {
        let mut inner = self.inner.lock().unwrap();
        let decision = planner::decide(&inner.state, input, inner.next_id, |layer| self.validate(layer));
        match &decision.reply {
            Reply::Error(message) => eprintln!("deckery: override {what}: rejected — {message}"),
            Reply::Began(id) => println!("deckery: override: session {id} opened"),
            Reply::Ok if decision.effect == Effect::End => println!("deckery: override: session ended"),
            Reply::Ok => {}
        }
        if let Some(next) = decision.next {
            if let State::Open { id, .. } = next {
                inner.next_id = inner.next_id.max(id + 1);
            }
            inner.state = next;
            self.active.store(matches!(inner.state, State::Open { .. }), Ordering::Release);
        }
        // Applied under the lock, so two commands cannot reach the registry
        // in the other order than they were decided in.
        match decision.effect {
            Effect::None => {}
            Effect::Refresh | Effect::Relaunch => {
                if let State::Open { layer, .. } = &inner.state {
                    self.registry.set_override(Some(layer.clone()));
                }
                self.changed.send_modify(|n| *n += 1);
                if decision.effect == Effect::Relaunch {
                    let _ = self.devices.send(DeviceRequest::Relaunch);
                }
            }
            Effect::End => {
                self.registry.set_override(None);
                self.changed.send_modify(|n| *n += 1);
                let _ = self.devices.send(DeviceRequest::Reload);
            }
        }
        decision.reply
    }

    /// Whether `layer` may go live: valid over every base config the registry
    /// has, as loaded from disk, with the commands those configs bind as the
    /// allowlist. With no base config loaded there is nothing to lay it over,
    /// and it only has to parse on its own.
    fn validate(&self, layer: &Layer) -> Result<(), super::layer::Refusal> {
        let known = self.registry.known_commands();
        let bases = self.registry.base_configs_without_override();
        if bases.is_empty() {
            return layer.validate(&Config::new_empty("override".to_string()), &known);
        }
        bases.iter().try_for_each(|base| layer.validate(base, &known))
    }
}

fn command_name(command: &OverrideCommand) -> &'static str {
    match command {
        OverrideCommand::Begin => "begin",
        OverrideCommand::Set { .. } => "set",
        OverrideCommand::Clear => "clear",
        OverrideCommand::End => "end",
        OverrideCommand::Malformed(_) => "command",
    }
}
