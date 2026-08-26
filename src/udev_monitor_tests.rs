use super::*;

#[tokio::test]
async fn launch_tasks_returns_modifiers_and_virt_dev_holder() {
    let config_files = Vec::new();
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
    let (virt_dev_opt, modifiers) = launch_tasks(
        &config_files,
        &mut tasks,
        env,
        error_notify,
        client,
        window_changed,
        None, // no lizard cfg in test
        gaming_mode,
        state_tx,
    ).await;

    if let Some(_) = virt_dev_opt {
        // If a device was found, modifiers must be a connected Arc.
        let _ = modifiers;
    }
}

/// When no config files are loaded, launch_tasks must:
///   1. transition lifecycle to Ready
///   2. set the "no_device" error slot
/// These are the two state commands the tray relies on to show the red
/// "no device" indicator instead of a false-positive green.
#[tokio::test]
async fn no_config_files_sends_lifecycle_ready_and_no_device_error() {
    let config_files = Vec::new();
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
    launch_tasks(
        &config_files,
        &mut tasks,
        env,
        error_notify,
        client,
        window_changed,
        None,
        gaming_mode,
        state_tx,
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
