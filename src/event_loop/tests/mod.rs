//! The reader end to end: Steam Deck input in through its channel, actions out
//! through the tap.
//!
//! Each decision `convert_event` consults is tested in `machine` and `ledger`.
//! What those tests cannot see is the order the reader applies them in — the
//! ledger's key-ups before the re-resolve, the announcement before the frames,
//! the modifier bookkeeping after them, what a pause in between does to all of
//! it — and a stuck key or a modifier that never lets go comes from that order,
//! not from any one decision. So these drive the production reader, with its
//! real read loop, its real IPC loop and its real Gaming Mode setter, and read
//! back what it did.
//!
//! The reader is built through `EventReader::assemble`, the constructor `new`
//! itself goes through, with a tap as the only port: no `/dev/uinput`, no
//! session and no state file, and nothing in production that knows it is being
//! tested.
//!
//! The input is only what a Steam Deck produces: the key and axis events that
//! `deckery_controller::hid_report` synthesises from hidraw, each report closed
//! by a SYN_REPORT. Buttons, the D-pad and the digital triggers arrive as
//! `EV_KEY`; the sticks as `ABS_X/Y` and `ABS_RX/RY`.
//!
//! The config goes through `ConfigRegistry::load` from a scratch directory,
//! which is the path production takes, so aliases, the automatic registration
//! of custom modifiers and the merge are the real ones.

mod analog_export;
mod gaming_mode;
mod ledger;
mod modifiers;
mod override_session;
mod pause;
mod read_path;
mod remaps;

use super::*;
use crate::config_registry::ConfigRoots;
use crate::output::action::{Emission, OutputAction, StateAction};
use crate::output::outputs::Tap;
use crate::override_session::DeviceRequest;
use std::path::PathBuf;

/// The shipped base config, cut down to what these tests press, plus the few
/// bindings it has no example of: a custom modifier that also has an output of
/// its own (L4), a combo whose release can change mid-hold (R1-Start), a
/// `while_gaming` binding, a Hold binding (R4), and a stick bound to
/// directions.
///
/// L1 is a layer key — `L1 = []` plus `L1-X` — and Steam a modifier with no
/// binding of its own, only `Steam-X`. R5 emits Ctrl and is a hint modifier
/// through `[hints]`, never a real one. Select has no binding at all.
pub(super) const BASE: &str = r#"
[device]
class = "hid-steam"
names = ["Steam Deck"]
aliases = { A = "BTN_SOUTH", B = "BTN_EAST", X = "BTN_WEST", Y = "BTN_NORTH", L1 = "BTN_TL", R1 = "BTN_TR", L2 = "BTN_TL2", R2 = "BTN_TR2", L3 = "BTN_THUMBL", R3 = "BTN_THUMBR", L4 = "BTN_GRIPL", R4 = "BTN_GRIPR", Down = "BTN_DPAD_DOWN", Start = "BTN_START", Steam = "BTN_MODE", R5 = "BTN_GRIPR2" }

[remap]
L2 = { keys = ["BTN_RIGHT"], no_pause = true, silent = true }
R2 = { keys = ["BTN_LEFT"],  no_pause = true, silent = true }
A     = ["KEY_ENTER"]
B     = ["KEY_ESC"]
X     = ["KEY_BACKSPACE"]
Down  = ["KEY_DOWN"]
Start = ["KEY_F10"]
Y     = { keys = ["KEY_SPACE"], while_gaming = true }
L1 = []
R1 = []
L4 = ["KEY_LEFTSHIFT"]
R5 = ["KEY_LEFTCTRL"]
"-R4" = ["KEY_TAB"]
LSTICK_UP = ["KEY_UP"]

L1-X     = { keys = ["KEY_LEFTCTRL", "KEY_C"], label = "Copy" }
L4-A     = { keys = ["KEY_LEFTCTRL", "KEY_S"], label = "Save" }
R1-Start = { keys = ["KEY_LEFTALT", "KEY_F4"], label = "Close" }
Steam-X  = { keys = [], label = "(Steam Keyboard)" }

[hints]
KEY_LEFTCTRL-KEY_ENTER = "Send"

[commands]
L3 = { run = ["deckery-hud-toggle"], no_pause = true, silent = true, label = "Toggle HUD" }
R3 = { run = ["notify-send", "hello"], label = "Notify" }

[settings]
16_BIT_AXIS = "true"
LSTICK = "bind"

[gaming_mode]
trigger = { key = "BTN_BASE", ms = 400 }

[trackpad.right]
mode = "mt-trackpad"

[trackpad.left]
mode = "disabled"
"#;

/// An app override, matched by window class the way KWin reports it.
const FIREFOX: &str = r#"
[module]
match_window_class = ["firefox"]

[remap]
A = ["KEY_SPACE"]
"#;

impl EventReader {
    /// A reader for `base_config` whose only port is `tap`.
    ///
    /// Everything that is not a port is what `device_tasks` hands to `new` for
    /// a Steam Deck, except that nothing outside this reader shares it: the
    /// modifier set and the Gaming Mode flag are its own, and the state
    /// commands it would send elsewhere have their receiver dropped, so
    /// whatever is sent is refused at once instead of piling up. Gaming Mode
    /// requests go to the caller, which is how a test sees the override
    /// session end Gaming Mode. The session is one with no display server unless the test
    /// says KDE, where the focused window is whatever `active_client` holds.
    fn for_test(
        base_config: Config,
        registry: Arc<ConfigRegistry>,
        event_rx: mpsc::Receiver<ControllerEvent>,
        environment: Environment,
        tap: Tap,
        analog_state_export: Arc<Mutex<bool>>,
        gaming_mode_tx: mpsc::Sender<bool>,
        override_session: Arc<OverrideSession>,
    ) -> Self {
        let base_name = base_config.name.clone();
        // The reader's own cell, written by the port as in production, so a
        // snapshot built in a test says what the overlay would.
        let last_action = Arc::new(Mutex::new(None));
        let (_, state_write_rx) = mpsc::channel(1);
        let ports = Ports {
            outputs: crate::output::Outputs {
                last_action: Some(last_action.clone()),
                tap: Some(tap),
                ..Default::default()
            },
            last_action,
            state_write_rx,
        };
        let (state_tx, _) = mpsc::channel(1);
        Self::assemble(
            base_config, registry, base_name, event_rx, false, 0, None,
            Arc::new(Mutex::new(Vec::new())),
            // What `device_tasks` starts it at.
            Arc::new(Mutex::new(true)),
            environment, Arc::new(Mutex::new(Client::Default)), Arc::new(Notify::new()),
            Arc::new(Mutex::new(false)), gaming_mode_tx, state_tx, analog_state_export, ports, override_session,
        )
    }
}

/// One reader with its read loop running, its tap, and the directory its config
/// was loaded from.
pub(super) struct Deck {
    pub reader: Arc<EventReader>,
    events: mpsc::Sender<ControllerEvent>,
    tap: mpsc::Receiver<Emission>,
    read_loop: tokio::task::JoinHandle<()>,
    /// The reader's emergency-exit timer, which runs beside the read loop in
    /// production too.
    emergency_loop: tokio::task::JoinHandle<()>,
    dir: PathBuf,
    server: Server,
    /// The override session this reader shares with the control socket.
    pub session: Arc<OverrideSession>,
    /// What the session asked the device loop for, which the harness stands in
    /// for — see `settle`.
    device_requests: tokio::sync::mpsc::UnboundedReceiver<DeviceRequest>,
    /// Every device request seen so far, oldest first.
    pub device_log: Vec<DeviceRequest>,
    /// Gaming Mode requests the reader sent, waiting for `settle`.
    gaming_requests: mpsc::Receiver<bool>,
}

impl Drop for Deck {
    fn drop(&mut self) {
        self.read_loop.abort();
        self.emergency_loop.abort();
        if !self.dir.as_os_str().is_empty() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

impl Deck {
    /// A reader for `BASE` in a session with no display server.
    pub fn new(name: &str) -> Self {
        Self::with(name, Server::Unsupported, Arc::new(Mutex::new(false)))
    }

    /// A reader as a rebuild starts it: with the analog export flag of the
    /// device session it belongs to, which outlives every reader.
    pub fn in_session(name: &str, analog_state_export: Arc<Mutex<bool>>) -> Self {
        Self::with(name, Server::Unsupported, analog_state_export)
    }

    /// A reader on KDE, where the focused window is whatever `active_client`
    /// says — which is how a test switches apps without a compositor.
    pub fn on_kde(name: &str) -> Self {
        Self::with(name, Server::Connected("KDE".to_string()), Arc::new(Mutex::new(false)))
    }

    fn with(name: &str, server: Server, analog_state_export: Arc<Mutex<bool>>) -> Self {
        // Unique per test and per run: the tests run in parallel, and a
        // directory left behind by a crashed run must not leak into this one.
        let dir = std::env::temp_dir()
            .join(format!("deckery-reader-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let system = dir.join("system");
        std::fs::create_dir_all(system.join("apps")).unwrap();
        std::fs::write(system.join("Steam Deck.toml"), BASE).unwrap();
        std::fs::write(system.join("apps").join("firefox.toml"), FIREFOX).unwrap();
        Self::from_dir(dir, server, analog_state_export)
    }

    /// A reader for the configs already in `dir`, as a freshly started makima
    /// would load them.
    fn from_dir(dir: PathBuf, server: Server, analog_state_export: Arc<Mutex<bool>>) -> Self {
        let registry = ConfigRegistry::load(ConfigRoots { system: dir.join("system"), user: dir.join("user") });
        let (session, device_requests) = OverrideSession::new(registry.clone());
        let (gaming_mode_tx, gaming_requests) = mpsc::channel(8);
        // What `device_tasks` hands to `new`.
        let base_config = registry.base_configs().into_iter().next()
            .expect("the base config parses");
        // One slot, so a send returns only once the loop has taken the event
        // before it — which is what `fence` counts on.
        let (events, event_rx) = mpsc::channel(1);
        let (tap_tx, tap) = mpsc::channel(4096);
        let environment = Environment {
            user: Err(std::env::VarError::NotPresent),
            sudo_user: Err(std::env::VarError::NotPresent),
            server: server.clone(),
        };
        let reader = Arc::new(EventReader::for_test(
            base_config, registry, event_rx, environment, Tap::new(tap_tx), analog_state_export,
            gaming_mode_tx, session.clone(),
        ));
        // The production loop, running for the whole test: what it keeps
        // between events — which stick direction is held, which trigger is
        // down — carries over from one `feed` to the next as it does on a Deck.
        let read_loop = tokio::spawn({
            let reader = reader.clone();
            async move { reader.read_events().await }
        });
        let emergency_loop = tokio::spawn({
            let reader = reader.clone();
            async move { reader.emergency_exit_loop().await }
        });
        Self {
            reader, events, tap, read_loop, emergency_loop, dir, server, session,
            device_requests, device_log: Vec::new(), gaming_requests,
        }
    }

    /// makima stopped and started again over the same config files.
    pub fn restart(mut self) -> Self {
        let dir = std::mem::take(&mut self.dir);
        let server = self.server.clone();
        drop(self);
        // A new process: nothing of the old device session survives.
        Self::from_dir(dir, server, Arc::new(Mutex::new(false)))
    }

    /// Replace the base config on disk. Nothing reads it until a reload.
    pub fn write_base(&self, text: &str) {
        std::fs::write(self.dir.join("system").join("Steam Deck.toml"), text).unwrap();
    }

    /// One override line through the control socket's own `answer`, then
    /// everything the session asked for carried out. Returns the reply and
    /// what the reader did meanwhile.
    pub async fn override_cmd(&mut self, line: &str) -> (String, Vec<OutputAction>) {
        let reply = crate::control_socket::answer(line, &self.session)
            .expect("an override line is answered by the control socket");
        self.settle().await;
        (reply, self.take())
    }

    /// Stand in for the device loop and the reader's override loop: carry out
    /// what the session asked the device loop for, let the reader apply the
    /// change, and pass its Gaming Mode requests to the production setter.
    ///
    /// A reload is what the device loop does too. A rebuild of the device
    /// tasks cannot be done here — there is no device — so it is only
    /// recorded in `device_log`; the reader re-resolves either way.
    pub async fn settle(&mut self) {
        while let Ok(request) = self.device_requests.try_recv() {
            if request == DeviceRequest::Reload {
                self.reader.registry.reload();
            }
            self.device_log.push(request);
        }
        self.reader.apply_override_change().await;
        self.run_gaming_requests().await;
    }

    /// Pass the Gaming Mode requests the reader sent to the production setter.
    pub async fn run_gaming_requests(&mut self) {
        while let Ok(on) = self.gaming_requests.try_recv() {
            let (tx, rx) = mpsc::channel(1);
            tx.send(on).await.unwrap();
            drop(tx);
            self.reader.gaming_mode_set_loop(rx).await;
        }
    }

    /// The config files changed, as the watcher reports it to the device
    /// loop: read them again unless the session holds the reload back.
    pub async fn config_files_changed(&mut self) {
        if !self.session.defers_file_reload() {
            self.reader.registry.reload();
        }
        self.reader.update_config().await;
    }

    /// Send `events` through the read loop and return what it did.
    pub async fn feed(&mut self, events: Vec<InputEvent>) -> Vec<OutputAction> {
        for event in events {
            self.send(ControllerEvent::Input(event)).await;
        }
        self.fence().await;
        self.take()
    }

    /// One controller event that is not input — a reconnect, a release-all —
    /// through the same loop.
    pub async fn signal(&mut self, event: ControllerEvent) -> Vec<OutputAction> {
        self.send(event).await;
        self.fence().await;
        self.take()
    }

    pub async fn press(&mut self, key: Key) -> Vec<OutputAction> {
        self.feed(report(key, 1)).await
    }

    pub async fn release(&mut self, key: Key) -> Vec<OutputAction> {
        self.feed(report(key, 0)).await
    }

    /// One line on the control socket, through the production IPC loop.
    ///
    /// The loop runs until its channel closes, and the sender is dropped once
    /// the line is in, so this returns when the command has been handled in
    /// full. The read loop is idle meanwhile — every `feed` ends on a fence —
    /// so the two never interleave in a way a test did not ask for.
    pub async fn ipc(&mut self, line: &str) -> Vec<OutputAction> {
        let (tx, rx) = broadcast::channel(4);
        tx.send(line.to_string()).unwrap();
        drop(tx);
        self.reader.ipc_command_loop(rx).await;
        self.take()
    }

    pub async fn pause(&mut self) -> Vec<OutputAction> {
        self.ipc("pause").await
    }

    pub async fn resume(&mut self) -> Vec<OutputAction> {
        self.ipc("resume").await
    }

    /// Gaming Mode switched by whatever asks for it — Steam detection, the IPC
    /// command — through the production setter, the one consumer of those
    /// requests.
    pub async fn set_gaming_mode(&mut self, on: bool) -> Vec<OutputAction> {
        let (tx, rx) = mpsc::channel(1);
        tx.send(on).await.unwrap();
        drop(tx);
        self.reader.gaming_mode_set_loop(rx).await;
        self.take()
    }

    async fn send(&mut self, event: ControllerEvent) {
        self.events.send(event).await.expect("the read loop is running");
    }

    /// Return once everything sent so far has been handled.
    ///
    /// The channel holds one event, so a send returns only once the loop has
    /// taken the one before it, and the loop takes the next only after it has
    /// finished with the last. Two SYN_REPORTs after the input therefore mean
    /// the input is done: the second send returns once the first has been
    /// taken. A SYN_REPORT on its own is what the Deck sends with every report,
    /// and with no stick moved since the last one the loop does nothing with it.
    async fn fence(&mut self) {
        for _ in 0..2 {
            self.send(ControllerEvent::Input(syn())).await;
        }
    }

    /// What the tap has seen since the last call.
    pub fn take(&mut self) -> Vec<OutputAction> {
        let mut out = Vec::new();
        while let Ok(emission) = self.tap.try_recv() {
            out.push(emission.action);
        }
        out
    }

    pub async fn modifiers(&self) -> Vec<Event> {
        self.reader.modifiers.lock().await.clone()
    }

    pub async fn held_keys(&self) -> Vec<Event> {
        self.reader.held_keys.lock().await.clone()
    }

    /// Whether the ledger has nothing recorded as held down.
    pub async fn ledger_is_empty(&self) -> bool {
        self.reader.emitted_outputs.lock().await.is_empty()
    }

    pub async fn modifier_was_activated(&self) -> bool {
        *self.reader.modifier_was_activated.lock().await
    }
}

pub(super) fn syn() -> InputEvent {
    InputEvent::new(EventType::SYNCHRONIZATION, 0, 0)
}

/// One hidraw report in which `key` changed, as `hid_report` synthesises it.
pub(super) fn report(key: Key, value: i32) -> Vec<InputEvent> {
    vec![InputEvent::new(EventType::KEY, key.code(), value), syn()]
}

/// One report in which the left stick's Y axis moved to `y`.
pub(super) fn lstick_y(y: i32) -> Vec<InputEvent> {
    vec![InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_Y.0, y), syn()]
}

/// Every key event that went out, synthesised or forwarded and on whichever
/// device, as (key, value) in the order it went out.
pub(super) fn keys(actions: &[OutputAction]) -> Vec<(Key, i32)> {
    let key = EventType::KEY.0;
    actions
        .iter()
        .flat_map(|a| match a {
            OutputAction::Frame { events, .. } => events
                .iter()
                .filter(|e| e.ev_type == key)
                .map(|e| (Key(e.code), e.value))
                .collect::<Vec<_>>(),
            OutputAction::Forward { events, .. } => events
                .iter()
                .filter(|r| r.0.event_type().0 == key)
                .map(|r| (Key(r.0.code()), r.0.value()))
                .collect(),
            _ => vec![],
        })
        .collect()
}

/// The keys pressed among `actions`. A tap is a press, so is a re-pressed
/// output, and a test about what may not happen after a pause asks this.
pub(super) fn presses(actions: &[OutputAction]) -> Vec<Key> {
    keys(actions).into_iter().filter(|&(_, v)| v != 0).map(|(k, _)| k).collect()
}

/// `keys`, sorted, for a test about a set of key events that does not depend
/// on the ledger's map order.
pub(super) fn sorted_keys(actions: &[OutputAction]) -> Vec<(Key, i32)> {
    let mut keys = keys(actions);
    keys.sort_by_key(|&(k, v)| (k.code(), v));
    keys
}

/// The keys still down after replaying `actions` the way the kernel would.
///
/// The property most tests end on: whatever went down came back up. A repeated
/// release is a no-op to the kernel, so it is one here.
pub(super) fn still_down(actions: &[OutputAction]) -> Vec<Key> {
    let mut down: Vec<Key> = Vec::new();
    for (key, value) in keys(actions) {
        match value {
            0 => down.retain(|k| *k != key),
            _ if !down.contains(&key) => down.push(key),
            _ => {}
        }
    }
    down
}

/// The labels `last_action` was set to, in order.
pub(super) fn announced(actions: &[OutputAction]) -> Vec<Option<String>> {
    actions
        .iter()
        .filter_map(|a| match a {
            OutputAction::State(StateAction::SetLastAction(Some(spec))) => Some(spec.label.clone()),
            _ => None,
        })
        .collect()
}

/// The last state snapshot published among `actions`, if any.
pub(super) fn last_published(actions: &[OutputAction]) -> Option<&serde_json::Value> {
    actions.iter().rev().find_map(|a| match a {
        OutputAction::State(StateAction::Publish { snapshot, .. }) => Some(&**snapshot),
        _ => None,
    })
}

pub(super) fn commands(actions: &[OutputAction]) -> Vec<Vec<String>> {
    actions
        .iter()
        .filter_map(|a| match a {
            OutputAction::Command { argv, .. } => Some(argv.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn ev(key: Key) -> Event {
    Event::Key(key)
}
