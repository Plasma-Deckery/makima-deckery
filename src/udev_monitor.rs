use crate::config::{DeviceClass, Event};
use crate::config_registry::ConfigRegistry;
use crate::device_session::TrackpadSession;
use crate::event_reader::EventReader;
use deckery_controller::{
    SteamDeckController, LizardModeSuppression, ControllerEvent,
};
use crate::virtual_devices::VirtualDevices;
use std::{env, path::{Path, PathBuf}, process::Command, sync::Arc};
use tokio::sync::{broadcast, mpsc, Mutex, Notify};
use tokio::task::JoinHandle;
use crate::compositor;
use crate::state_writer::{StateWriterHandle, StateCommand, AppLifecycle};
use tokio_stream::StreamExt;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub enum Client {
    #[default]
    Default,
    /// Window class + caption (both forwarded raw by the KWin script) +
    /// the PID of the focused window's owning process (KDE only; None elsewhere).
    Class(String, String, Option<u32>),
}

#[derive(Clone)]
pub enum Server {
    Connected(String),
    Unsupported,
    Failed,
}

#[derive(Clone)]
pub struct Environment {
    pub user: Result<String, env::VarError>,
    pub sudo_user: Result<String, env::VarError>,
    pub server: Server,
}

/// Spawn a reconnecting evdev reader for a non-Steam-Deck device.
///
/// Returns the `Receiver` end of a `ControllerEvent` channel, or `None` if
/// the device cannot be opened (e.g. race condition: appeared in enumeration
/// but disappeared before we could open it). Fires `device_error_notify` if
/// the device does not return within the reconnect timeout after a stream error.
///
/// Generic devices have no resume watcher (no logind integration needed) — they
/// reconnect reactively on stream errors only.
/// Open a generic (non-Steam Deck) evdev device and spawn a simple event
/// reader task. On stream error, fires `device_error_notify` to trigger a
/// full device reinit via the udev monitor loop — no reconnect logic needed.
fn spawn_event_reader(
    path: PathBuf,
    grab: bool,
    device_error_notify: Arc<Notify>,
) -> Option<mpsc::Receiver<ControllerEvent>> {
    let mut device = match evdev::Device::open(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("deckery: cannot open {:?}: {} — skipping device", path, e);
            return None;
        }
    };
    if grab {
        if let Err(e) = device.grab() {
            eprintln!("deckery: cannot grab {:?}: {} — skipping device", path, e);
            return None;
        }
        println!("deckery: grabbed {:?} (exclusive evdev access)", path);
    } else {
        println!("deckery: opened {:?} (no grab)", path);
    }
    let mut stream = match device.into_event_stream() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("deckery: cannot stream {:?}: {} — skipping device", path, e);
            return None;
        }
    };
    let (event_tx, event_rx) = mpsc::channel(64);
    tokio::spawn(async move {
        loop {
            match stream.next().await {
                Some(Ok(event)) => {
                    if event_tx.send(ControllerEvent::Input(event)).await.is_err() {
                        break;
                    }
                }
                Some(Err(e)) => {
                    eprintln!("deckery: {:?} stream error: {} — triggering reinit", path, e);
                    device_error_notify.notify_one();
                    break;
                }
                None => break,
            }
        }
    });
    Some(event_rx)
}

/// Compute the Lizard Mode suppression config from a device's base config.
fn lizard_cfg_from_base(base: Option<crate::config::Config>) -> Option<LizardModeSuppression> {
    let setting = base
        .as_ref()
        .and_then(|c| c.settings.get("SUPPRESS_LIZARD_MODE"))
        .map(|s| s.as_str())
        .unwrap_or("buttons,mouse");
    LizardModeSuppression::from_setting(setting)
}

pub async fn start_monitoring_udev(registry: Arc<ConfigRegistry>, config_dir: String, mut tasks: Vec<JoinHandle<()>>, gaming_mode: Arc<Mutex<bool>>, state_tx: StateWriterHandle, ipc_tx: broadcast::Sender<String>) {
    let environment = set_environment();
    // Modules gated by `[module] requires_compositor` can only be judged once
    // the session environment is known, which is later than registry load time.
    registry.set_compositor(match &environment.server {
        Server::Connected(name) => Some(name.clone()),
        Server::Unsupported | Server::Failed => None,
    });
    // The snapshot main.rs published at load time hid every gated module — with
    // no compositor known yet, none of them could match. Republish now that it is.
    let _ = state_tx.try_send(StateCommand::SetLoadedConfigs(registry.snapshot()));
    let device_error_notify = Arc::new(Notify::new());
    let active_client: Arc<Mutex<Client>> = Arc::new(Mutex::new(Client::Default));
    let window_changed: Arc<Notify> = Arc::new(Notify::new());

    // Subscribe to logind PrepareForSleep — fires device_error_notify on resume
    // so the existing reinit path handles reconnect without a full process restart.
    tokio::spawn(crate::resume_watcher::start_resume_watcher(
        device_error_notify.clone(),
    ));

    // Start the compositor focus-watcher once — persists across device reinitializations.
    // Event-driven adapters (KDE, Hyprland) push focus changes into active_client + notify.
    // The Fallback adapter is a no-op; EventReader falls back to get_active_window() polling.
    if let Server::Connected(s) = &environment.server {
        let adapter = compositor::detect(s);
        println!("deckery: compositor adapter: {}", adapter.name());
        tokio::spawn(adapter.run_focus_watcher(
            active_client.clone(),
            window_changed.clone(),
        ));
    }

    // Pre-create the output device layer once at startup.
    //
    // Virtual keyboard/mouse/pointer devices are instantiated here, before any
    // physical controller is detected. They persist for the entire lifetime of
    // the process — across device connect/disconnect cycles and full reinits.
    // This means:
    //   • KDE/libinput see stable, persistent virtual device nodes (no "device
    //     disappeared" flicker on controller reconnect or config reload).
    //   • The correct output layer is always available, even briefly before the
    //     physical controller is enumerated.
    //   • Multiple evdev nodes that map to the same hidraw sibling share this
    //     single output layer (deduplication in launch_tasks prevents double
    //     sessions, so only one EventReader ever writes to these devices).
    //
    // Trackpad virtual devices (lpad, rpad, gesture_pad) start as None and are
    // enabled the first time launch_tasks finds a Steam Deck controller —
    // VirtualDevices::enable_trackpads() is idempotent, so subsequent reinits
    // leave the already-active uinput nodes untouched.
    let virt_dev = Arc::new(Mutex::new(VirtualDevices::new()));

    let mut prev_modifiers = launch_tasks(&registry, &mut tasks, environment.clone(), device_error_notify.clone(), active_client.clone(), window_changed.clone(), gaming_mode.clone(), state_tx.clone(), &ipc_tx, virt_dev.clone()).await;
    let mut monitor = tokio_udev::AsyncMonitorSocket::new(
        tokio_udev::MonitorBuilder::new()
            .unwrap()
            .match_subsystem(std::ffi::OsStr::new("input"))
            .unwrap()
            .listen()
            .unwrap(),
    )
    .unwrap();

    // Config file watcher — reloads on any .toml change in the watched dirs.
    // Watches the config dir directly, plus the parent dirs of any symlinked
    // .toml files (e.g. files in a git repo). Watching parent dirs instead of
    // individual files survives editor renames (sed -i, vim swapfiles, etc.)
    // that would otherwise invalidate an inode-based per-file watch.
    // Events are filtered to .toml files so unrelated files in those dirs are ignored.
    let (config_tx, mut config_rx) = tokio::sync::mpsc::channel::<()>(1);
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                use notify::EventKind::*;
                match event.kind {
                    Create(_) | Modify(_) | Remove(_) => {
                        let is_toml = event.paths.iter().any(|p| {
                            p.extension().and_then(|e| e.to_str()) == Some("toml")
                        });
                        if is_toml { let _ = config_tx.try_send(()); }
                    }
                    _ => {}
                }
            }
        },
        notify::Config::default(),
    ).expect("Failed to create config file watcher");
    watcher.watch(std::path::Path::new(&config_dir), RecursiveMode::NonRecursive)
        .expect("Failed to watch config directory");
    // Also watch parent dirs of symlink targets.
    if let Ok(entries) = std::fs::read_dir(&config_dir) {
        let mut extra_dirs: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                if let Ok(real) = std::fs::canonicalize(&path) {
                    if real != path {
                        if let Some(parent) = real.parent() {
                            extra_dirs.insert(parent.to_path_buf());
                        }
                    }
                }
            }
        }
        for dir in extra_dirs {
            let _ = watcher.watch(&dir, RecursiveMode::NonRecursive);
        }
    }

    loop {
        tokio::select! {
            event = monitor.next() => {
                if let Some(Ok(event)) = event {
                    if is_mapped(&event.device(), &registry) {
                        println!("---------------------\n\nReinitializing...\n");
                        let _ = state_tx.try_send(StateCommand::SetLifecycle(AppLifecycle::Reinitializing));
                        release_held_modifiers(&virt_dev, &prev_modifiers).await;
                        for task in &tasks {
                            task.abort();
                        }
                        tasks.clear();
                        prev_modifiers = launch_tasks(&registry, &mut tasks, environment.clone(), device_error_notify.clone(), active_client.clone(), window_changed.clone(), gaming_mode.clone(), state_tx.clone(), &ipc_tx, virt_dev.clone()).await;
                    }
                }
            }
            _ = device_error_notify.notified() => {
                // A genuine device error (USB unplug or reconnect timeout from the
                // controller's reconnecting task). Restart input tasks — the output
                // layer (virt_dev) is preserved across reinits.
                println!("---------------------\n\nDevice error detected, reinitializing...\n");
                let _ = state_tx.try_send(StateCommand::SetLifecycle(AppLifecycle::Reinitializing));
                release_held_modifiers(&virt_dev, &prev_modifiers).await;
                for task in &tasks {
                    task.abort();
                }
                tasks.clear();
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                prev_modifiers = launch_tasks(&registry, &mut tasks, environment.clone(), device_error_notify.clone(), active_client.clone(), window_changed.clone(), gaming_mode.clone(), state_tx.clone(), &ipc_tx, virt_dev.clone()).await;
            }
            Some(_) = config_rx.recv() => {
                // Debounce: drain any queued events, then wait briefly for the
                // editor to finish writing.
                while config_rx.try_recv().is_ok() {}
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                println!("---------------------\n\nConfig changed, reloading...\n");
                let _ = state_tx.try_send(StateCommand::SetLifecycle(AppLifecycle::Reinitializing));
                registry.reload(&config_dir);
                let _ = state_tx.try_send(StateCommand::SetLoadedConfigs(registry.snapshot()));
                report_base_config_error(&registry, &state_tx);
                release_held_modifiers(&virt_dev, &prev_modifiers).await;
                for task in &tasks {
                    task.abort();
                }
                tasks.clear();
                prev_modifiers = launch_tasks(&registry, &mut tasks, environment.clone(), device_error_notify.clone(), active_client.clone(), window_changed.clone(), gaming_mode.clone(), state_tx.clone(), &ipc_tx, virt_dev.clone()).await;
            }
        }
    }
}

/// Before restarting input tasks on reinit, release all held modifier output
/// keys so the kernel (and thus XWayland/compositor) knows those keys are no
/// longer pressed. Without this, a stuck-modifier state persists across the
/// reinit — modifiers held at reinit time (e.g. Ctrl+Alt from a paddle+button
/// combo) are never released, causing phantom combo activation until the user
/// physically presses and releases those keys again.
///
/// The output device layer (`virt_dev`) is preserved across reinits, so the
/// release events land on the same uinput nodes the compositor already knows.
async fn release_held_modifiers(
    virt_dev: &Arc<Mutex<VirtualDevices>>,
    prev_modifiers: &Arc<Mutex<Vec<Event>>>,
) {
    let held = prev_modifiers.lock().await.clone();
    if held.is_empty() {
        return;
    }
    let mut vd = virt_dev.lock().await;
    for modifier in &held {
        if let Event::Key(key) = modifier {
            let _ = vd.keys.emit(&[
                evdev::InputEvent::new_now(evdev::EventType::KEY, key.code(), 0),
            ]);
        }
    }
}

pub async fn launch_tasks(
    registry: &Arc<ConfigRegistry>,
    tasks: &mut Vec<JoinHandle<()>>,
    environment: Environment,
    device_error_notify: Arc<Notify>,
    active_client: Arc<Mutex<Client>>,
    window_changed: Arc<Notify>,
    gaming_mode: Arc<Mutex<bool>>,
    state_tx: StateWriterHandle,
    ipc_tx: &broadcast::Sender<String>,
    // Persistent output device layer, pre-created by start_monitoring_udev.
    // All EventReader instances share this single set of virtual devices; the
    // same Arc is reused across reinits so uinput nodes never disappear.
    virt_dev: Arc<Mutex<VirtualDevices>>,
) -> Arc<Mutex<Vec<Event>>> {
    // Unified Gaming Mode channel: steam detection and IPC both send here.
    // The EventReader's gaming_mode_set_loop is the sole consumer.
    let (gaming_mode_tx, gaming_mode_rx) = mpsc::channel::<bool>(32);

    // Steam detection task — spawned once per session, outside any EventReader.
    // auto_detect comes from the first base config found in the registry;
    // defaults to true (detect by default) when no base config is loaded yet.
    let auto_detect = registry.base_configs()
        .first()
        .map(|c| c.gaming_mode_config.auto_detect_steam_games)
        .unwrap_or(true);
    tokio::spawn(crate::steam_detector::steam_detection_task(
        window_changed.clone(),
        active_client.clone(),
        gaming_mode_tx.clone(),
        auto_detect,
    ));

    // The rx goes to the first matched reader (wrapped in Option so only one
    // reader takes it). Subsequent readers (multi-device) get a dead receiver.
    let mut gaming_mode_rx_opt = Some(gaming_mode_rx);

    let modifiers: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Default::default()));
    let modifier_was_activated: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));

    let user_has_access = match Command::new("groups").output() {
        Ok(groups)
            if std::str::from_utf8(&groups.stdout.as_slice())
                .unwrap()
                .contains("input") =>
        {
            println!("Evdev permissions available.\nScanning for event devices with a matching config file...\n");
            true
        }
        Ok(groups)
            if std::str::from_utf8(&groups.stdout.as_slice())
                .unwrap()
                .contains("root") =>
        {
            println!("Root permissions available.\nScanning for event devices with a matching config file...\n");
            true
        }
        Ok(_) => {
            println!("Warning: user has no access to event devices, Makima might not be able to detect all connected devices.\n\
                    Note: Run Makima with 'sudo -E makima' or as a system service. Refer to the docs for more info. Continuing...\n");
            false
        }
        Err(_) => {
            println!(
                "Warning: unable to determine if user has access to event devices. Continuing...\n"
            );
            false
        }
    };

    // ── Hidraw deduplication ─────────────────────────────────────────────────
    //
    // hid-steam on kernels ≥7.1 creates multiple evdev nodes per physical
    // controller (one per HID interface: mouse, keyboard, …). All of them share
    // the same hidraw sibling. Without deduplication, launch_tasks would open
    // one EventReader per evdev node → doubled key events, doubled virtual
    // devices, doubled haptics.
    //
    // Solution: track the hidraw paths we have already claimed this cycle.
    // The first evdev node that leads to a given hidraw path wins; subsequent
    // nodes that resolve to the same path are silently skipped.
    let mut seen_hidraw: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    let mut devices_found = 0;

    // ── New path: content-based discovery ────────────────────────────────────
    //
    // Iterate base configs (those with a [device] section). For each, scan all
    // evdev devices and open the first one whose name matches the declaration.
    // This inverts the old loop: config → find device (instead of device → find config).
    let base_configs = registry.base_configs();
    for base_config in base_configs {
        let decl = base_config.device.as_ref().unwrap(); // guaranteed by base_configs()
        let is_hid_steam = decl.class == DeviceClass::HidSteam;

        let lizard_cfg = lizard_cfg_from_base(Some(base_config.clone()));
        let grab = base_config.settings.get("GRAB_DEVICE").map_or(false, |v| v == "true");
        let trackpad_config = base_config.trackpad.clone();
        let config_name = base_config.name.clone();

        // Find the first physical device matching this declaration.
        let matched = evdev::enumerate().find(|(_, d)| {
            let name = d.name().unwrap_or("").replace("/", "");
            decl.matches_evdev_name(&name)
        });

        let (event_device_path, evdev_device) = match matched {
            Some((path, dev)) => (path, dev),
            None => {
                println!("deckery: no device found matching config {:?} (names: {:?})", config_name, decl.names);
                continue;
            }
        };
        let event_device = event_device_path.to_str().unwrap_or("").to_string();

        let (is_tablet, max_abs_wheel) = if is_hid_steam {
            (false, 0i32)
        } else {
            let tablet = evdev_device.supported_keys()
                .map_or(false, |keys| keys.contains(evdev::Key::BTN_TOOL_PEN));
            let wheel = evdev_device.get_abs_state()
                .ok()
                .and_then(|abs| abs.get(evdev::AbsoluteAxisType::ABS_WHEEL.0 as usize)
                    .map(|info| info.maximum))
                .unwrap_or(0);
            (tablet, wheel)
        };

        // A pen's axis ranges are device-specific, so the shared output layer
        // can only learn them once an actual tablet has been discovered.
        if is_tablet {
            virt_dev.lock().await.enable_tablet(&evdev_device);
        }

        let (event_rx, pad_rx, haptic_tx, lizard_mode, click_pressure) = if is_hid_steam {
            let controller = SteamDeckController::from_evdev(Path::new(&event_device));
            match &controller.hidraw_path {
                Some(hidraw) => {
                    if !seen_hidraw.insert(hidraw.clone()) {
                        println!(
                            "deckery: {:?} — hidraw {:?} already claimed, \
                             skipping duplicate evdev node",
                            event_device, hidraw
                        );
                        continue;
                    }
                }
                None => {
                    eprintln!("deckery: {:?}: no hidraw sibling found — skipping device", event_device);
                    continue;
                }
            }
            let session = match controller.start(device_error_notify.clone(), lizard_cfg).await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("deckery: cannot open {:?}: {} — skipping device", event_device, e);
                    continue;
                }
            };
            (session.event_rx, session.pad_rx, session.haptic_tx, Some(session.lizard_mode), session.click_pressure)
        } else {
            let rx = match spawn_event_reader(event_device_path, grab, device_error_notify.clone()) {
                Some(rx) => rx,
                None => continue,
            };
            (rx, None, None, None, None)
        };

        let session = if is_hid_steam {
            Some(TrackpadSession::setup(&trackpad_config, &virt_dev, pad_rx, haptic_tx.clone(), click_pressure).await)
        } else {
            None
        };

        let gaming_rx = gaming_mode_rx_opt.take().unwrap_or_else(|| {
            let (_, dead_rx) = mpsc::channel(1);
            dead_rx
        });
        let reader = EventReader::new(
            base_config,
            registry.clone(),
            config_name.clone(),
            virt_dev.clone(),
            event_rx,
            is_tablet,
            max_abs_wheel,
            haptic_tx,
            lizard_mode,
            modifiers.clone(),
            modifier_was_activated.clone(),
            environment.clone(),
            active_client.clone(),
            window_changed.clone(),
            gaming_mode.clone(),
            gaming_mode_tx.clone(),
            state_tx.clone(),
        );
        tasks.push(tokio::spawn(start_reader(reader, gaming_rx, ipc_tx.subscribe(), session)));
        devices_found += 1;
    }


    // Lifecycle: scan complete — transition to "ready" regardless of result.
    // Error slot: set when no device found, clear when at least one is active.
    let _ = state_tx.try_send(StateCommand::SetLifecycle(AppLifecycle::Ready));
    if devices_found == 0 {
        if !user_has_access {
            println!("No matching devices found.\nNote: make sure that your user has access to event devices.\n");
            let _ = state_tx.try_send(StateCommand::SetError {
                id:       "no_device".to_string(),
                message:  "No matching device found — user may lack event device access".to_string(),
                severity: "error",
            });
        } else {
            println!(
                "No matching devices found.\n\
                 Note: for Steam Deck / Steam Controller, name the config file \
                 \"Steam Deck.toml\" — makima normalises all kernel-reported name \
                 variants to that canonical name automatically.\n\
                 For other devices, the config file name must match the evdev device \
                 name as reported by 'evtest'.\n"
            );
            let _ = state_tx.try_send(StateCommand::SetError {
                id:       "no_device".to_string(),
                message:  "No matching device found — for Steam Deck use \"Steam Deck.toml\"; for other devices check evdev name matches config file name".to_string(),
                severity: "error",
            });
        }
    } else {
        let _ = state_tx.try_send(StateCommand::ClearError { id: "no_device".to_string() });
    }

    modifiers
}

pub async fn start_reader(reader: EventReader, gaming_rx: mpsc::Receiver<bool>, ipc_rx: broadcast::Receiver<String>, session: Option<TrackpadSession>) {
    reader.start(gaming_rx, ipc_rx, session).await;
}

fn set_environment() -> Environment {
    match env::var("DBUS_SESSION_BUS_ADDRESS") {
        Ok(_) => copy_variables(),
        Err(_) => {
            let uid = Command::new("sh").arg("-c").arg("id -u").output().unwrap();
            let uid_number = std::str::from_utf8(uid.stdout.as_slice()).unwrap().trim();
            if uid_number != "0" {
                let bus_address = format!("unix:path=/run/user/{}/bus", uid_number);
                env::set_var("DBUS_SESSION_BUS_ADDRESS", bus_address);
                copy_variables()
            } else {
                println!("Warning: unable to inherit user environment.\n\
                        Launch Makima with 'sudo -E makima' or make sure that your systemd unit is running with the 'User=<username>' parameter.\n");
            }
        }
    };
    if let (Err(env::VarError::NotPresent), Ok(_)) =
        (env::var("XDG_SESSION_TYPE"), env::var("WAYLAND_DISPLAY"))
    {
        env::set_var("XDG_SESSION_TYPE", "wayland")
    }

    // Compositors listed here get Server::Connected and enable per-app bindings.
    // KDE and Hyprland use event-driven adapters (compositor module).
    // sway and niri use the legacy get_active_window() polling fallback.
    let supported_compositors = vec!["KDE", "Hyprland", "sway", "niri"]
        .into_iter()
        .map(|str| String::from(str))
        .collect::<Vec<String>>();
    let (x11, wayland) = (String::from("x11"), String::from("wayland"));
    let server: Server = match (
        env::var("XDG_SESSION_TYPE"),
        env::var("XDG_CURRENT_DESKTOP"),
    ) {
        (Ok(session), Ok(desktop))
            if session == wayland && supported_compositors.contains(&desktop) =>
        {
            println!("Running on {}, per application bindings enabled.", desktop);
            Server::Connected(desktop)
        }
        (Ok(session), Ok(desktop)) if session == wayland => {
            println!("Warning: unsupported compositor: {}, won't be able to change bindings according to the active window.\n\
                    Currently supported desktops: Plasma/KWin (event-driven), Hyprland (event-driven), Sway (polling), Niri (polling), X11.\n", desktop);
            Server::Unsupported
        }
        (Ok(session), _) if session == x11 => {
            println!("Running on X11, per application bindings enabled.");
            Server::Connected(session)
        }
        (Ok(session), Err(_)) if session == wayland => {
            println!("Warning: unable to retrieve the current desktop based on XDG_CURRENT_DESKTOP env var.\n\
                    Won't be able to change bindings according to the active window.\n");
            Server::Unsupported
        }
        (Err(_), _) => {
            println!("Warning: unable to retrieve the session type based on XDG_SESSION_TYPE or WAYLAND_DISPLAY env vars.\n\
                    Is your Wayland compositor or X server running?\n\
                    Exiting Makima.");
            std::process::exit(0);
        }
        _ => Server::Failed,
    };

    Environment {
        user: env::var("USER"),
        sudo_user: env::var("SUDO_USER"),
        server,
    }
}

fn copy_variables() {
    let command = Command::new("sh")
        .arg("-c")
        .arg("systemctl --user show-environment")
        .output()
        .unwrap();
    let vars = std::str::from_utf8(command.stdout.as_slice())
        .unwrap()
        .split("\n")
        .collect::<Vec<&str>>();
    for var in vars {
        if let Some((variable, value)) = var.split_once("=") {
            if let Err(env::VarError::NotPresent) = env::var(variable) {
                env::set_var(variable, value);
            } else if variable == "PATH" {
                env::set_var("PATH", format!("{}:{}", value, env::var("PATH").unwrap()));
            }
        }
    }
}


/// Check the registry for a broken base config and report it via the state
/// writer: sends `SetError { id: "base_config" }` when broken, `ClearError`
/// when healthy.  Called once at startup (main.rs) and after every config
/// reload (udev_monitor event loop) so the tray icon stays in sync.
pub(crate) fn report_base_config_error(registry: &ConfigRegistry, state_tx: &StateWriterHandle) {
    match registry.base_config_error() {
        Some(msg) => {
            eprintln!("deckery: base config has parse errors — system degraded");
            let _ = state_tx.try_send(StateCommand::SetError {
                id: "base_config".to_string(), message: msg, severity: "error",
            });
        }
        None => {
            let _ = state_tx.try_send(StateCommand::ClearError {
                id: "base_config".to_string(),
            });
        }
    }
}

pub fn is_mapped(udev_device: &tokio_udev::Device, registry: &Arc<ConfigRegistry>) -> bool {
    // Only consider devices that have an actual evdev node (/dev/input/eventX).
    // udev fires multiple events per plug — one for the parent input device (no devnode)
    // and one per event node. Without this check, we'd reinit for every sub-event.
    if udev_device.devnode().is_none() {
        return false;
    }
    if let Some(name) = udev_device.property_value("NAME") {
        let name = name.to_string_lossy().replace("\"", "").replace("/", "");
        return registry.any_device_matches(&name);
    }
    false
}

#[cfg(test)]
#[path = "udev_monitor_tests.rs"]
mod tests;
