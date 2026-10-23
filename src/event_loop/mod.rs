//! One device's reader: the tasks that drive it, and the state they share.
//!
//! This was a single 2349-line file holding five unrelated jobs. Each is now its
//! own neighbour:
//!
//! * `read_event` — raw evdev in, logical events out
//! * `convert_event` — what a logical event means, and applying it
//! * `state` — gathering the snapshot the overlay reads
//! * `ipc` — the control socket's commands
//! * `sticks` — the four timer-driven motion loops
//!
//! What stays here is the struct, its construction, and the tasks that
//! coordinate the rest: `start`, the Gaming Mode setter and its channel, the
//! focus watcher, and the few helpers the neighbours share.
//!
//! The deciding is in `machine` and the carrying out is in `output`; neither is
//! here.

mod convert_event;
mod ipc;
mod read_event;
mod state;
mod sticks;

use crate::active_client::*;
use crate::config::{Axis, Cursor, Event, GamingModeConfig, Relative, Scroll, TrackpadConfig};
use crate::config_registry::ConfigRegistry;
use crate::device_session::TrackpadSession;
use deckery_controller::{HapticPad, HapticRequest, LizardModeHandle};
use crate::resolver::{resolve_binding, ResolvedBinding};
use crate::state_export::LastAction;
use crate::state_writer::{StateWriterHandle, StateCommand};
use crate::trackpad::PadState;
use crate::trackpad_router;
use crate::session::{Client, Environment, Server};
use crate::virtual_devices::VirtualDevices;
use crate::config::Config;
use deckery_controller::ControllerEvent;
use evdev::{AbsoluteAxisType, EventType, InputEvent, Key, RelativeAxisType};
use std::{
    collections::HashMap,
    option::Option,
    sync::Arc,
};
use crate::settings::Settings;
use tokio::sync::{broadcast, mpsc, Mutex, Notify};




pub struct EventReader {
    registry: Arc<ConfigRegistry>,
    /// Name of the base config this reader was launched for — the registry key
    /// used to resolve, not the kernel-reported device name.
    base_name: String,
    /// Receiver for controller events from the reconnecting reader task.
    /// `ControllerEvent::Input` carries normal evdev events; `Reconnected`
    /// signals a transparent reconnect (EventReader must release held keys).
    event_rx: Mutex<mpsc::Receiver<ControllerEvent>>,
    /// Whether the connected device is a graphics tablet (BTN_TOOL_PEN).
    /// Queried once at open time and carried here to avoid re-querying on reconnect.
    is_tablet: bool,
    /// Maximum absolute axis value across all axes, queried once at open time.
    max_abs_wheel: i32,
    /// Where the decided actions go. Built from the same handles as the fields
    /// around it, so clipping the device port out is all it takes to run this
    /// reader against nothing — which is what makes the decisions testable
    /// without a kernel. The trackpad session still takes `virt_dev` directly;
    /// until it goes through a port too, that one path stays unclipped.
    outputs: crate::output::Outputs,
    lstick_position: Arc<Mutex<Vec<i32>>>,
    rstick_position: Arc<Mutex<Vec<i32>>>,
    /// Raw hardware position of left stick (ABS_X, ABS_Y), always tracked regardless of mode.
    lstick_raw: Arc<Mutex<(i32, i32)>>,
    /// Raw hardware position of right stick (ABS_RX, ABS_RY), always tracked regardless of mode.
    rstick_raw: Arc<Mutex<(i32, i32)>>,
    /// Last written analog snapshot (rounded to 3 dp). Skips write when unchanged.
    /// Order: [lstick_x, lstick_y, rstick_x, rstick_y, lpad_x, lpad_y, rpad_x, rpad_y]
    analog_snapshot: Arc<Mutex<[f32; 8]>>,
    /// Raw IMU values (ABS_HAT2X, ABS_HAT2Y) — gyroscope/accelerometer, range 0..32767.
    imu_raw: Arc<Mutex<(i32, i32)>>,
    /// When false, write_state_analog() is a no-op — no 60 Hz analog writes to state.json.
    /// The UI sends "analog on"/"analog off" via the IPC socket to control this.
    analog_state_export: Arc<Mutex<bool>>,
    cursor_movement: Arc<Mutex<(i32, i32)>>,
    scroll_movement: Arc<Mutex<(i32, i32)>>,
    modifiers: Arc<Mutex<Vec<Event>>>,
    modifier_was_activated: Arc<Mutex<bool>>,
    paused: Arc<Mutex<bool>>,
    gaming_mode: Arc<Mutex<bool>>,
    last_action: Arc<Mutex<Option<LastAction>>>,
    held_keys: Arc<Mutex<Vec<Event>>>,
    /// Maps each currently held physical button to the output keys it last emitted.
    /// On key-up (value=0), every stored output key for this button is released,
    /// ensuring that base-remap outputs (e.g. KEY_F10) are always cleaned up even
    /// when a mid-hold modifier change (e.g. R1) switched the resolution to a combo.
    emitted_outputs: Arc<Mutex<HashMap<Event, Vec<Key>>>>,
    device_is_connected: Arc<Mutex<bool>>,
    /// Held as an `Arc` because it is cloned out of the mutex on every event so
    /// the lock is not carried into the conversion — a deep copy there would put
    /// the whole merged config on the per-keypress path.
    current_config: Arc<Mutex<Arc<Config>>>,
    environment: Environment,
    settings: Settings,
    active_client: Arc<Mutex<Client>>,
    window_changed: Arc<Notify>,
    trackpad: TrackpadConfig,
    lpad: PadState,
    rpad: PadState,
    /// True while a combined gesture session is active.
    gesture_session: Arc<Mutex<bool>>,
    /// Setter handle for Lizard Mode config. Kept here for its lifetime —
    /// dropping it signals the hidraw writer to exit. Call `lizard_mode.set(cfg)`
    /// to update suppression settings live without a session reinit.
    /// `None` for non-Steam-Deck devices (no hidraw writer to signal).
    _lizard_mode: Option<LizardModeHandle>,
    /// Gaming Mode trigger configuration (from `[gaming_mode]` in the base config).
    /// Stored here so `convert_event` can check it without locking anything.
    gaming_mode_config: GamingModeConfig,
    /// Timestamp of the last Gaming Mode trigger button press.
    /// Used to detect a double-click within `gaming_mode_config.double_click_ms`.
    gaming_mode_trigger_ts: Arc<Mutex<Option<std::time::Instant>>>,
    /// Sender half of the unified Gaming Mode channel. Cloned into the IPC
    /// handler so external commands (`gaming_mode enable/disable`) go through
    /// the same `gaming_mode_set_loop` as steam auto-detection.
    gaming_mode_tx: mpsc::Sender<bool>,
    /// Channel to the central state writer task. All state.json writes go
    /// through this sender — EventReader never touches the file directly.
    state_tx: StateWriterHandle,
    /// Asks `state_write_loop` for a state write. Created here rather than in
    /// `start()` so a detached task — one waiting on a spawned command — can
    /// hold a clone and make its result visible without borrowing `self`.
    state_write_tx: mpsc::Sender<crate::trackpad_router::StateWrite>,
    /// Receiver half of the above, taken once by `start()`.
    state_write_rx: Mutex<Option<mpsc::Receiver<crate::trackpad_router::StateWrite>>>,
}


impl EventReader {
    pub fn new(
        base_config: Config,
        registry: Arc<ConfigRegistry>,
        base_name: String,
        virt_dev: Arc<Mutex<VirtualDevices>>,
        event_rx: mpsc::Receiver<ControllerEvent>,
        is_tablet: bool,
        max_abs_wheel: i32,
        haptic_tx: Option<mpsc::Sender<HapticRequest>>,
        lizard_mode: Option<LizardModeHandle>,
        modifiers: Arc<Mutex<Vec<Event>>>,
        modifier_was_activated: Arc<Mutex<bool>>,
        environment: Environment,
        active_client: Arc<Mutex<Client>>,
        window_changed: Arc<Notify>,
        gaming_mode: Arc<Mutex<bool>>,
        gaming_mode_tx: mpsc::Sender<bool>,
        state_tx: StateWriterHandle,
    ) -> Self {
        let mut position_vector: Vec<i32> = Vec::new();
        for i in [0, 0] {
            position_vector.push(i)
        }
        let lstick_position = Arc::new(Mutex::new(position_vector.clone()));
        let rstick_position = Arc::new(Mutex::new(position_vector.clone()));
        let lstick_raw = Arc::new(Mutex::new((0i32, 0i32)));
        let rstick_raw = Arc::new(Mutex::new((0i32, 0i32)));
        let imu_raw = Arc::new(Mutex::new((0i32, 0i32)));
        let analog_state_export = Arc::new(Mutex::new(false));
        let analog_snapshot = Arc::new(Mutex::new([f32::NAN; 8]));
        let cursor_movement = Arc::new(Mutex::new((0, 0)));
        let scroll_movement = Arc::new(Mutex::new((0, 0)));
        let device_is_connected: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));
        let paused: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
        let last_action: Arc<Mutex<Option<LastAction>>> = Arc::new(Mutex::new(None));
        let (state_write_tx, state_write_rx) = mpsc::channel(8);
        let held_keys: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        let emitted_outputs: Arc<Mutex<HashMap<Event, Vec<Key>>>> = Arc::new(Mutex::new(HashMap::new()));
        let current_config: Arc<Mutex<Arc<Config>>> =
            Arc::new(Mutex::new(Arc::new(base_config.clone())));

        // The settings are typed by the config layer now, which is where the
        // defaults live too. Anything unusable is a problem to report rather
        // than a panic — see `settings.rs` for why that distinction mattered.
        let (settings, setting_problems) = crate::settings::Settings::from_config(&base_config);
        for key in crate::settings::REPORTABLE_KEYS {
            let id = crate::settings::error_id(key);
            match setting_problems.iter().find(|p| p.key == *key) {
                Some(problem) => {
                    eprintln!("deckery: {problem}");
                    let _ = state_tx.try_send(StateCommand::SetError {
                        id,
                        message: problem.to_string(),
                        severity: "warning",
                    });
                }
                // Cleared whether or not it was ever set, because this runs on
                // every reinit and the config may have been fixed since.
                None => { let _ = state_tx.try_send(StateCommand::ClearError { id }); }
            }
        }


        let trackpad = base_config.trackpad.clone();
        let gaming_mode_config = base_config.gaming_mode_config.clone();

        Self {
            registry,
            base_name,
            event_rx: Mutex::new(event_rx),
            is_tablet,
            max_abs_wheel,
            outputs: crate::output::Outputs {
                devices:  Some(virt_dev.clone()),
                state:    Some(state_tx.clone()),
                commands: Some(environment.clone()),
                internal: Some(gaming_mode_tx.clone()),
                haptics:  haptic_tx.clone(),
                last_action: Some(last_action.clone()),
                publish:  Some(state_write_tx.clone()),
                tap:      None,
            },
            lstick_position,
            rstick_position,
            lstick_raw,
            rstick_raw,
            imu_raw,
            analog_state_export,
            analog_snapshot,
            cursor_movement,
            scroll_movement,
            modifiers,
            modifier_was_activated,
            paused,
            gaming_mode,
            last_action,
            held_keys,
            emitted_outputs,
            device_is_connected,
            current_config,
            environment,
            settings,
            active_client,
            window_changed,
            trackpad,
            lpad: PadState::new(true),
            rpad: PadState::new(false),
            gesture_session: Arc::new(Mutex::new(false)),
            _lizard_mode: lizard_mode,
            gaming_mode_config,
            gaming_mode_trigger_ts: Arc::new(Mutex::new(None)),
            gaming_mode_tx,
            state_tx,
            state_write_tx,
            state_write_rx: Mutex::new(Some(state_write_rx)),
        }
    }

    pub async fn start(&self, gaming_rx: mpsc::Receiver<bool>, ipc_rx: broadcast::Receiver<String>, session: Option<TrackpadSession>) {
        let name = self.base_name.clone();
        let name = &name;
        println!("{:?} detected, reading events. +{}ms since startup\n", name, crate::startup_ms());
        let state_tx = self.state_write_tx.clone();
        let state_rx = self.state_write_rx.lock().await.take()
            .expect("start() runs once per EventReader");
        self.write_state().await;

        tokio::join!(
            self.read_events(),
            self.cursor_loop(),
            self.scroll_loop(),
            self.key_cursor_loop(),
            self.key_scroll_loop(),
            self.window_changed_loop(),
            self.gaming_mode_set_loop(gaming_rx),
            self.state_write_loop(state_rx),
            self.ipc_command_loop(ipc_rx),
            async {
                // The trackpad session is a producer of its own — hidraw frames,
                // not events — so it still takes the devices directly rather
                // than going through a port. It reads the handle out of the
                // device port, which means an unclipped port stops it too: no
                // devices, no trackpad session, and nothing reaches the kernel
                // from here either.
                match (session, self.outputs.devices.as_ref()) {
                    (Some(s), Some(devices)) => {
                        s.run(devices, &self.lpad, &self.rpad, &self.gesture_session,
                              state_tx, self.gaming_mode.clone()).await;
                    }
                    _ => {}
                }
            },
        );
    }

    /// Listens for window-activation changes fired by kwin_watcher (KDE only).
    /// For other compositors this future returns immediately.
    async fn window_changed_loop(&self) {
        if let Server::Connected(s) = &self.environment.server {
            if s == "KDE" {
                // notify_waiters() keeps no permit (unlike notify_one()), so a push
                // that arrives while nobody is registered is lost outright. Both
                // guards below exist for that single reason.
                //
                // Guard 1 — register before the first read. enable() puts this
                // future in the waiter list without awaiting it, closing the window
                // between the read below and the first `.await` in the loop.
                let mut notified = Box::pin(self.window_changed.notified());
                notified.as_mut().enable();

                // Read the current focus once before waiting. The adapter pushes the
                // already-focused window right after loading its KWin script, which
                // happens long before this loop exists, so that first push is gone.
                // active_client still holds the value, so reading it here recovers
                // it. This also covers device reconnects, where a fresh EventReader
                // starts while the focus has not changed.
                self.update_config().await;

                loop {
                    notified.as_mut().await;

                    // Guard 2 — re-arm *before* the work, not after. update_config()
                    // runs a full registry.resolve() plus write_state(), which is
                    // milliseconds; re-arming afterwards would leave that entire span
                    // unwatched on every iteration, so two quick window switches
                    // could drop the second one.
                    notified = Box::pin(self.window_changed.notified());
                    notified.as_mut().enable();

                    self.update_config().await;
                }
            }
        }
    }

    /// Single handler for all Gaming Mode state changes.
    ///
    /// When `paused` is false (normal mode): sets the flag, fires the
    /// appropriate haptic chain, writes last_action and state.
    /// When `paused` is true (preview mode): skips the actual toggle and
    /// haptics, but still writes last_action so the HUD preview shows
    /// what the action would do.
    async fn set_gaming_mode(&self, new_state: bool, paused: bool) {
        if !paused {
            *self.gaming_mode.lock().await = new_state;
            println!("deckery: Gaming Mode {}", if new_state { "enabled" } else { "disabled" });
            let chain = if new_state { &self.gaming_mode_config.haptic_on }
                        else        { &self.gaming_mode_config.haptic_off };
            self.outputs.dispatch(&[crate::output::action::OutputAction::Haptic {
                pad: HapticPad::Both,
                chain: chain.clone(),
            }]).await;
        }
        // Said even while paused: the overlay shows what the press would do.
        let announcement = crate::machine::gaming_mode_toggle(new_state, paused);
        self.outputs.dispatch(&announcement[1..]).await;
        self.write_state().await;
    }

    /// Unified Gaming Mode setter — the single consumer of the `gaming_mode_tx`
    /// channel.  Both steam auto-detection and IPC commands send `bool` here;
    /// this loop applies the change when the desired state differs from the current one.
    ///
    /// Has full `self` access, so `set_gaming_mode()` works without any workarounds.
    /// Returns when all senders are dropped (device disconnected / non-KDE).
    async fn gaming_mode_set_loop(&self, mut rx: mpsc::Receiver<bool>) {
        while let Some(new_state) = rx.recv().await {
            if new_state != *self.gaming_mode.lock().await {
                println!("deckery: Gaming Mode trigger — channel, new_state={}", new_state);
                self.set_gaming_mode(new_state, false).await;
            }
        }
    }

    /// Release all currently held output keys and clear modifier/held-key state.
    ///
    /// Called on `ControllerEvent::Reconnected` after a suspend/resume cycle.
    /// Emits key-up events for all keys tracked in `emitted_outputs` (the
    /// actual output keys that got stuck) and clears `modifiers` and `held_keys`
    /// so combo and HUD state don't carry stale data across the reconnect.
    async fn release_all_held(&self) {
        // Collected before anything is sent, so the device port is held for the
        // frames alone rather than across the three locks below.
        let frames: Vec<crate::output::action::OutputAction> = {
            let mut eo = self.emitted_outputs.lock().await;
            eo.drain()
                .flat_map(|(_input_ev, output_keys)| output_keys)
                .map(|key| crate::machine::key_up(key))
                .collect()
        };
        self.apply_frames(&frames).await;
        // Release modifier output keys (e.g. Shift held via L4).
        // `modifiers` tracks input events but those that map to output keys will
        // already be in `emitted_outputs`; we clear modifiers to reset combo state.
        self.modifiers.lock().await.clear();
        // Clear HUD held-key state.
        self.held_keys.lock().await.clear();
    }

    /// Bridges `trackpad_router::run`'s state-write *requests* to the actual
    /// writes. The router in `trackpad_router.rs` knows *when* pad state
    /// changed and needs persisting, but not *how* — it has no access to
    /// sticks/IMU/config/modifiers, which live here. Keeping this as a
    /// separate small loop (instead of passing `&self` into `trackpad_router`)
    /// is what keeps that module decoupled from the rest of makima's state.
    async fn state_write_loop(&self, mut rx: mpsc::Receiver<trackpad_router::StateWrite>) {
        while let Some(req) = rx.recv().await {
            match req {
                trackpad_router::StateWrite::Immediate => self.write_state().await,
                trackpad_router::StateWrite::Analog => self.write_state_analog().await,
            }
        }
    }

    /// The focused window, however this compositor reports one.
    ///
    /// Both callers below need it and used to derive it with the same ten
    /// lines each.
    async fn current_client(&self) -> Client {
        match &self.environment.server {
            // Event-driven compositors push focus changes into active_client — read directly.
            Server::Connected(s) if crate::compositor::detect(s).is_event_driven() => {
                self.active_client.lock().await.clone()
            }
            // Polling fallback (sway, niri, x11): query on demand.
            _ => {
                let known = self.registry.window_class_modules();
                get_active_window(&self.environment, &known).await
            }
        }
    }

    /// Re-resolve the active config for the currently focused window.
    ///
    /// This used to cycle through four global layouts looking for one that
    /// resolved, and to recurse between here and change_active_layout() while
    /// doing it. Both are gone with the layouts themselves: resolve() now
    /// answers None only when the base config is missing or switched off, and
    /// there is no second question to ask in that case.
    async fn update_config(&self) {
        let client = self.current_client().await;
        match self.registry.resolve(&self.base_name, &client) {
            Some(config) => { *self.current_config.lock().await = config; }
            // The last config stays live rather than being cleared: it is
            // stale, but it is what the controller is holding, and dropping it
            // would make every button dead instead of wrong.
            None => {}
        }
        self.write_state().await;
    }

    /// Store emitted output keys for a physical button so they can be released
    /// in full on key-up, even if a mid-hold modifier change switched the resolution.
    async fn store_emitted_outputs(&self, event: &Event, keys: &[Key]) {
        if let Event::Key(_) = event {
            self.emitted_outputs.lock().await.insert(*event, keys.to_vec());
        }
    }


}

// No test module. What was testable here lives in `machine.rs` now, and what
// was left were five tests that built a local HashMap, inserted, removed and
// asserted HashMap semantics — no production code in any of them. The behaviour
// they meant to pin, that a release clears what its press emitted even when a
// combo overwrote it mid-hold, is covered by `machine::releases_emitted` and
// the key-up frames it produces.
