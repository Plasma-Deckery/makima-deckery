use super::*;
use crate::config_registry::ConfigRegistry;
use crate::session::Server;
use crate::virtual_devices::VirtualDevices;

/// A session with nothing in it: no configs, no devices, no display server.
/// Enough to drive launch_tasks, which is what these tests are about.
fn empty_session(state_tx: StateWriterHandle) -> DeviceSession {
    DeviceSession {
        registry:            ConfigRegistry::empty(),
        environment:         Environment {
            user: Ok("test".to_string()),
            sudo_user: Err(std::env::VarError::NotPresent),
            server: Server::Unsupported,
        },
        device_error_notify: Arc::new(Notify::new()),
        active_client:       Arc::new(Mutex::new(Client::Default)),
        window_changed:      Arc::new(Notify::new()),
        gaming_mode:         Arc::new(Mutex::new(false)),
        state_tx,
        ipc_tx:              tokio::sync::broadcast::channel(1).0,
        virt_dev:            Arc::new(Mutex::new(VirtualDevices::new())),
    }
}

#[tokio::test]
async fn launch_tasks_returns_modifiers() {
    let (state_tx, _state_rx) = tokio::sync::mpsc::channel(8);
    let session = empty_session(state_tx);
    let mut tasks = Vec::new();

    let modifiers = launch_tasks(&session, &mut tasks).await;

    // With no configs and no devices, modifiers should be an empty Arc.
    assert!(modifiers.lock().await.is_empty());
}

/// When no configs are loaded, launch_tasks must:
///   1. transition lifecycle to Ready
///   2. set the "no_device" error slot
/// These are the two state commands the tray relies on to show the red
/// "no device" indicator instead of a false-positive green.
#[tokio::test]
async fn no_config_files_sends_lifecycle_ready_and_no_device_error() {
    let (state_tx, mut state_rx) = tokio::sync::mpsc::channel(8);
    let session = empty_session(state_tx);
    let mut tasks = Vec::new();

    launch_tasks(&session, &mut tasks).await;

    // Drain all commands sent synchronously by launch_tasks.
    let mut commands = Vec::new();
    while let Ok(cmd) = state_rx.try_recv() {
        commands.push(cmd);
    }

    let has_lifecycle_ready = commands.iter().any(|cmd| {
        matches!(cmd, StateCommand::SetLifecycle(AppLifecycle::Ready))
    });
    let has_no_device_error = commands.iter().any(|cmd| {
        matches!(cmd, StateCommand::SetError { id, .. } if id == "no_device")
    });

    assert!(has_lifecycle_ready, "expected SetLifecycle(Ready) but got: {:?}", commands);
    assert!(has_no_device_error, "expected SetError(no_device) but got: {:?}", commands);
}
