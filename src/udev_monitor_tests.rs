use super::*;
use crate::config_registry::{ConfigRegistry, ConfigEntry, ConfigError};
use crate::config::Config;
use crate::virtual_devices::VirtualDevices;

#[tokio::test]
async fn launch_tasks_returns_modifiers() {
    let registry = ConfigRegistry::empty();
    let mut tasks = Vec::new();
    let env = Environment {
        user: Ok("test".to_string()),
        sudo_user: Err(std::env::VarError::NotPresent),
        server: Server::Unsupported,
    };
    let error_notify = Arc::new(Notify::new());
    let client = Arc::new(Mutex::new(Client::Default));
    let window_changed = Arc::new(Notify::new());
    let gaming_mode = Arc::new(Mutex::new(false));
    let (state_tx, _state_rx) = tokio::sync::mpsc::channel(8);
    let (ipc_tx, _) = tokio::sync::broadcast::channel(1);
    let virt_dev = Arc::new(Mutex::new(VirtualDevices::new()));
    let modifiers = launch_tasks(
        &registry,
        &mut tasks,
        env,
        error_notify,
        client,
        window_changed,
        gaming_mode,
        state_tx,
        &ipc_tx,
        virt_dev,
    ).await;

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
    let registry = ConfigRegistry::empty();
    let mut tasks = Vec::new();
    let env = Environment {
        user: Ok("test".to_string()),
        sudo_user: Err(std::env::VarError::NotPresent),
        server: Server::Unsupported,
    };
    let error_notify = Arc::new(Notify::new());
    let client = Arc::new(Mutex::new(Client::Default));
    let window_changed = Arc::new(Notify::new());
    let gaming_mode = Arc::new(Mutex::new(false));

    let (state_tx, mut state_rx) = tokio::sync::mpsc::channel(8);
    let (ipc_tx, _) = tokio::sync::broadcast::channel(1);
    let virt_dev = Arc::new(Mutex::new(VirtualDevices::new()));
    launch_tasks(
        &registry,
        &mut tasks,
        env,
        error_notify,
        client,
        window_changed,
        gaming_mode,
        state_tx,
        &ipc_tx,
        virt_dev,
    ).await;

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

// ── report_base_config_error ──────────────────────────────────────────────────

fn make_registry_with_broken_base() -> std::sync::Arc<ConfigRegistry> {
    ConfigRegistry::from_entries(vec![ConfigEntry {
        name:    "Steam Deck".to_string(),
        config:  None,
        enabled: false,
        errors:  vec![ConfigError { severity: "error", message: "TOML parse error".into() }],
    }])
}

fn make_registry_with_valid_base() -> std::sync::Arc<ConfigRegistry> {
    let c = Config::new_empty("Steam Deck".to_string());
    ConfigRegistry::from_entries(vec![ConfigEntry {
        name:    "Steam Deck".to_string(),
        config:  Some(c),
        enabled: true,
        errors:  vec![],
    }])
}

#[tokio::test]
async fn broken_base_config_sends_set_error() {
    let registry = make_registry_with_broken_base();
    let (state_tx, mut state_rx) = tokio::sync::mpsc::channel(8);

    report_base_config_error(&registry, &state_tx);

    let mut commands = Vec::new();
    while let Ok(cmd) = state_rx.try_recv() { commands.push(cmd); }

    let has_set_error = commands.iter().any(|cmd| {
        matches!(cmd, StateCommand::SetError { id, severity, .. }
            if id == "base_config" && *severity == "error")
    });
    assert!(has_set_error, "expected SetError(base_config) but got: {:?}", commands);
}

#[tokio::test]
async fn healthy_base_config_sends_clear_error() {
    let registry = make_registry_with_valid_base();
    let (state_tx, mut state_rx) = tokio::sync::mpsc::channel(8);

    report_base_config_error(&registry, &state_tx);

    let mut commands = Vec::new();
    while let Ok(cmd) = state_rx.try_recv() { commands.push(cmd); }

    let has_clear_error = commands.iter().any(|cmd| {
        matches!(cmd, StateCommand::ClearError { id } if id == "base_config")
    });
    assert!(has_clear_error, "expected ClearError(base_config) but got: {:?}", commands);
}

#[tokio::test]
async fn report_transitions_from_error_to_clear() {
    // Simulate: first reload with broken config, second reload after fix.
    let (state_tx, mut state_rx) = tokio::sync::mpsc::channel(8);

    report_base_config_error(&make_registry_with_broken_base(), &state_tx);
    report_base_config_error(&make_registry_with_valid_base(),  &state_tx);

    let mut commands = Vec::new();
    while let Ok(cmd) = state_rx.try_recv() { commands.push(cmd); }

    // First command must be SetError, second ClearError.
    assert!(matches!(&commands[0], StateCommand::SetError { id, .. } if id == "base_config"),
        "expected first command SetError(base_config), got: {:?}", commands[0]);
    assert!(matches!(&commands[1], StateCommand::ClearError { id } if id == "base_config"),
        "expected second command ClearError(base_config), got: {:?}", commands[1]);
}
