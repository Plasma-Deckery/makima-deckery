use super::*;
use crate::config_registry::{ConfigRegistry, ConfigEntry, ConfigError};
use crate::config::Config;

// ── report_base_config_error ──────────────────────────────────────────────────

fn make_registry_with_broken_base() -> std::sync::Arc<ConfigRegistry> {
    ConfigRegistry::from_entries(vec![ConfigEntry {
        name:    "Steam Deck".to_string(),
        config:  None,
        enabled: false,
        errors:  vec![ConfigError { severity: "error", message: "TOML parse error".into() }],
        from_user: false,
    }])
}

fn make_registry_with_valid_base() -> std::sync::Arc<ConfigRegistry> {
    let c = Config::new_empty("Steam Deck".to_string());
    ConfigRegistry::from_entries(vec![ConfigEntry {
        name:    "Steam Deck".to_string(),
        config:  Some(c),
        enabled: true,
        errors:  vec![],
        from_user: false,
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
