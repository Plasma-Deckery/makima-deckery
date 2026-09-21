//! Starting, stopping and restarting the tasks that drive one device.
//!
//! Split out of `udev_monitor`, which had grown to hold four unrelated jobs:
//! watching udev, deriving the session environment, declaring the session
//! vocabulary, and this. What is left there now is the udev loop itself.
//!
//! `launch_tasks()` is called once at startup and again on every reinit — a
//! device error, a resume from suspend, a config reload. The output layer
//! (`VirtualDevices`) deliberately survives those restarts: recreating the
//! uinput nodes would make libinput rediscover them, and the gesture tools
//! would lose the pads for a second or two every time.

use crate::config::{DeviceClass, Event};
use crate::config_registry::ConfigRegistry;
use crate::device_session::TrackpadSession;
use crate::event_reader::EventReader;
use crate::session::{Client, Environment};
use crate::state_writer::{StateWriterHandle, StateCommand, AppLifecycle};
use crate::virtual_devices::VirtualDevices;
use deckery_controller::{SteamDeckController, LizardModeSuppression, ControllerEvent};
use std::{path::{Path, PathBuf}, process::Command, sync::Arc};
use tokio::sync::{broadcast, mpsc, Mutex, Notify};
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;

/// Everything `launch_tasks` needs that outlives a single device generation.
///
/// It used to be ten positional parameters, passed identically at all three
/// call sites and cloned at each one. Every field here is created once in
/// `start_monitoring_udev` and reused across every reinit; the task list is
/// the one thing that does not belong, because it is replaced each time.
pub struct DeviceSession {
    pub registry:            Arc<ConfigRegistry>,
    pub environment:         Environment,
    pub device_error_notify: Arc<Notify>,
    pub active_client:       Arc<Mutex<Client>>,
    pub window_changed:      Arc<Notify>,
    pub gaming_mode:         Arc<Mutex<bool>>,
    pub state_tx:            StateWriterHandle,
    pub ipc_tx:              broadcast::Sender<String>,
    /// Persistent output device layer, pre-created by `start_monitoring_udev`.
    /// All `EventReader` instances share this single set of virtual devices;
    /// the same `Arc` is reused across reinits so uinput nodes never disappear.
    pub virt_dev:            Arc<Mutex<VirtualDevices>>,
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

pub async fn release_held_modifiers(
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
    session: &DeviceSession,
    tasks: &mut Vec<JoinHandle<()>>,
) -> Arc<Mutex<Vec<Event>>> {
    let DeviceSession {
        registry, environment, device_error_notify, active_client,
        window_changed, gaming_mode, state_tx, ipc_tx, virt_dev,
    } = session;
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
            // The file name has nothing to do with this: a base config is
            // matched to hardware through `[device] names`, which is a list of
            // substrings tested against the evdev name. Saying otherwise sends
            // people renaming files that were never the problem.
            println!(
                "No matching devices found.\n\
                 Note: a base config finds its device through the `names` list in \
                 its [device] section — each entry is matched as a substring of \
                 the evdev device name reported by 'evtest'. The config's file \
                 name plays no part in it.\n"
            );
            let _ = state_tx.try_send(StateCommand::SetError {
                id:       "no_device".to_string(),
                message:  "No matching device found — check that [device] names in the base config matches the evdev device name from 'evtest'".to_string(),
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

#[cfg(test)]
#[path = "device_tasks_tests.rs"]
mod tests;
