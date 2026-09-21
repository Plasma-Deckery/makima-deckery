use crate::compositor;
use crate::config_registry::ConfigRegistry;
use crate::device_tasks::{self, DeviceSession};
use crate::session::{Client, Server};
use crate::state_writer::{StateWriterHandle, StateCommand, AppLifecycle};
use crate::virtual_devices::VirtualDevices;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, Notify};
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;

pub async fn start_monitoring_udev(registry: Arc<ConfigRegistry>, mut tasks: Vec<JoinHandle<()>>, gaming_mode: Arc<Mutex<bool>>, state_tx: StateWriterHandle, ipc_tx: broadcast::Sender<String>) {
    let environment = crate::session::set_environment();
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

    // Everything a device generation needs, assembled once. The task list is
    // the only thing that does not belong in it — that is replaced on every
    // reinit, which is exactly what the other fields are not.
    let session = DeviceSession {
        registry:            registry.clone(),
        environment,
        device_error_notify: device_error_notify.clone(),
        active_client,
        window_changed,
        gaming_mode,
        state_tx:            state_tx.clone(),
        ipc_tx,
        virt_dev:            virt_dev.clone(),
    };

    let mut prev_modifiers = device_tasks::launch_tasks(&session, &mut tasks).await;
    let mut monitor = tokio_udev::AsyncMonitorSocket::new(
        tokio_udev::MonitorBuilder::new()
            .unwrap()
            .match_subsystem(std::ffi::OsStr::new("input"))
            .unwrap()
            .listen()
            .unwrap(),
    )
    .unwrap();

    let (config_tx, mut config_rx) = tokio::sync::mpsc::channel::<()>(1);
    // The registry owns all file-watching logic — it knows which directories it
    // scans and watches them recursively.  Keep the handle alive for the loop.
    let _config_watcher = registry.start_watcher(config_tx);

    loop {
        tokio::select! {
            event = monitor.next() => {
                if let Some(Ok(event)) = event {
                    if is_mapped(&event.device(), &registry) {
                        println!("---------------------\n\nReinitializing...\n");
                        let _ = state_tx.try_send(StateCommand::SetLifecycle(AppLifecycle::Reinitializing));
                        device_tasks::release_held_modifiers(&virt_dev, &prev_modifiers).await;
                        for task in &tasks {
                            task.abort();
                        }
                        tasks.clear();
                        prev_modifiers = device_tasks::launch_tasks(&session, &mut tasks).await;
                    }
                }
            }
            _ = device_error_notify.notified() => {
                // A genuine device error (USB unplug or reconnect timeout from the
                // controller's reconnecting task). Restart input tasks — the output
                // layer (virt_dev) is preserved across reinits.
                println!("---------------------\n\nDevice error detected, reinitializing...\n");
                let _ = state_tx.try_send(StateCommand::SetLifecycle(AppLifecycle::Reinitializing));
                device_tasks::release_held_modifiers(&virt_dev, &prev_modifiers).await;
                for task in &tasks {
                    task.abort();
                }
                tasks.clear();
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                prev_modifiers = device_tasks::launch_tasks(&session, &mut tasks).await;
            }
            Some(_) = config_rx.recv() => {
                // Debounce: drain any queued events, then wait briefly for the
                // editor to finish writing.
                while config_rx.try_recv().is_ok() {}
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                println!("---------------------\n\nConfig changed, reloading...\n");
                let _ = state_tx.try_send(StateCommand::SetLifecycle(AppLifecycle::Reinitializing));
                registry.reload();
                let _ = state_tx.try_send(StateCommand::SetLoadedConfigs(registry.snapshot()));
                report_base_config_error(&registry, &state_tx);
                device_tasks::release_held_modifiers(&virt_dev, &prev_modifiers).await;
                for task in &tasks {
                    task.abort();
                }
                tasks.clear();
                prev_modifiers = device_tasks::launch_tasks(&session, &mut tasks).await;
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
