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

// ── is_mapped ─────────────────────────────────────────────────────────────────

fn registry_declaring(device_name: &str) -> std::sync::Arc<ConfigRegistry> {
    let mut c = Config::new_empty("Steam Deck".to_string());
    c.device = Some(crate::config::DeviceDeclaration {
        class: crate::config::DeviceClass::HidSteam,
        names: vec![device_name.to_string()],
    });
    ConfigRegistry::from_entries(vec![ConfigEntry {
        name:    "Steam Deck".to_string(),
        config:  Some(c),
        enabled: true,
        errors:  vec![],
        from_user: false,
    }])
}

#[test]
fn udev_quotes_are_stripped_from_the_name() {
    assert_eq!(normalize_udev_name(std::ffi::OsStr::new("\"Steam Deck\"")), "Steam Deck");
}

#[test]
fn slashes_are_dropped_like_on_the_evdev_side() {
    // launch_tasks drops slashes from the evdev name before matching. If only
    // one side did it, a hot-plug of a device with a slash in its name would
    // never trigger a reinit even though the scan would find it.
    assert_eq!(normalize_udev_name(std::ffi::OsStr::new("\"Foo/Bar Pad\"")), "FooBar Pad");
}

#[test]
fn a_matching_event_node_is_mapped() {
    let registry = registry_declaring("Steam Deck");
    assert!(name_is_mapped(true, Some(std::ffi::OsStr::new("\"Steam Deck\"")), &registry));
}

#[test]
fn an_event_without_a_devnode_is_never_mapped() {
    // udev fires one event for the parent input device (no devnode) and one
    // per event node. Reacting to the parent as well would reinit twice per plug.
    let registry = registry_declaring("Steam Deck");
    assert!(!name_is_mapped(false, Some(std::ffi::OsStr::new("\"Steam Deck\"")), &registry));
}

#[test]
fn an_event_without_a_name_is_not_mapped() {
    let registry = registry_declaring("Steam Deck");
    assert!(!name_is_mapped(true, None, &registry));
}

#[test]
fn an_unrelated_device_is_not_mapped() {
    let registry = registry_declaring("Steam Deck");
    assert!(!name_is_mapped(true, Some(std::ffi::OsStr::new("\"AT Translated Set 2 keyboard\"")), &registry));
}

// ── Config reload ─────────────────────────────────────────────────────────────
//
// Issue #42, item 5: after a config file changes, the new configs must be the
// ones in effect. These tests go through the disk, because that is the path a
// real edit takes. What they cannot reach is the device half — `launch_tasks`
// needs uinput — but that half reads `registry.base_configs()` afresh on every
// call, so the registry is where "in effect" is decided.

fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn deck_with_auto_detect(auto_detect: bool) -> String {
    format!("[device]\nclass = \"hid-steam\"\nnames = [\"Steam Deck\"]\n\n\
             [gaming_mode]\nauto_detect_steam_games = {auto_detect}\n")
}

fn drain(rx: &mut tokio::sync::mpsc::Receiver<StateCommand>) -> Vec<StateCommand> {
    let mut commands = Vec::new();
    while let Ok(cmd) = rx.try_recv() { commands.push(cmd); }
    commands
}

#[tokio::test]
async fn a_changed_base_config_is_what_the_next_launch_reads() {
    let root = scratch_dir("deckery-udev-reload-base");
    let system = root.join("system");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::write(system.join("Steam Deck.toml"), deck_with_auto_detect(true)).unwrap();
    let registry = ConfigRegistry::load(crate::config_registry::ConfigRoots {
        system: system.clone(), user: root.join("user"),
    });
    assert!(registry.base_configs()[0].gaming_mode_config.auto_detect_steam_games);

    std::fs::write(system.join("Steam Deck.toml"), deck_with_auto_detect(false)).unwrap();
    let (state_tx, _state_rx) = tokio::sync::mpsc::channel(8);
    reload_configs(&registry, &state_tx);

    let base = registry.base_configs();
    assert_eq!(base.len(), 1);
    assert!(!base[0].gaming_mode_config.auto_detect_steam_games,
        "the reloaded base config should have auto-detection off");

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_reload_tells_the_state_writer_in_order() {
    // Reinitializing first, so the tray greys out while the configs are being
    // read; then the new snapshot; then the base_config verdict on it.
    let root = scratch_dir("deckery-udev-reload-order");
    let system = root.join("system");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::write(system.join("Steam Deck.toml"), deck_with_auto_detect(true)).unwrap();
    let registry = ConfigRegistry::load(crate::config_registry::ConfigRoots {
        system: system.clone(), user: root.join("user"),
    });

    std::fs::write(system.join("extra.toml"), "[module]\n").unwrap();
    let (state_tx, mut state_rx) = tokio::sync::mpsc::channel(8);
    reload_configs(&registry, &state_tx);

    let commands = drain(&mut state_rx);
    assert!(matches!(commands[0], StateCommand::SetLifecycle(AppLifecycle::Reinitializing)),
        "expected Reinitializing first, got: {:?}", commands);
    match &commands[1] {
        StateCommand::SetLoadedConfigs(summaries) => {
            assert!(summaries.iter().any(|s| s.name == "extra"),
                "the snapshot should list the newly added file, got: {:?}", summaries);
        }
        other => panic!("expected SetLoadedConfigs second, got: {:?}", other),
    }
    assert!(matches!(&commands[2], StateCommand::ClearError { id } if id == "base_config"),
        "expected ClearError(base_config) third, got: {:?}", commands);

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn breaking_and_fixing_a_file_sets_and_clears_the_base_config_error() {
    let root = scratch_dir("deckery-udev-reload-broken");
    let system = root.join("system");
    std::fs::create_dir_all(&system).unwrap();
    std::fs::write(system.join("Steam Deck.toml"), deck_with_auto_detect(true)).unwrap();
    let registry = ConfigRegistry::load(crate::config_registry::ConfigRoots {
        system: system.clone(), user: root.join("user"),
    });
    let (state_tx, mut state_rx) = tokio::sync::mpsc::channel(16);

    std::fs::write(system.join("Steam Deck.toml"), "[device\nnot toml").unwrap();
    reload_configs(&registry, &state_tx);
    let broken = drain(&mut state_rx);
    assert!(broken.iter().any(|c| matches!(c, StateCommand::SetError { id, .. } if id == "base_config")),
        "expected SetError(base_config) after breaking the file, got: {:?}", broken);
    assert!(registry.base_configs().is_empty(), "a broken base config must not be launched");

    std::fs::write(system.join("Steam Deck.toml"), deck_with_auto_detect(true)).unwrap();
    reload_configs(&registry, &state_tx);
    let fixed = drain(&mut state_rx);
    assert!(fixed.iter().any(|c| matches!(c, StateCommand::ClearError { id } if id == "base_config")),
        "expected ClearError(base_config) after fixing the file, got: {:?}", fixed);
    assert_eq!(registry.base_configs().len(), 1);

    let _ = std::fs::remove_dir_all(&root);
}

// ── Debounce ──────────────────────────────────────────────────────────────────

#[tokio::test(start_paused = true)]
async fn settling_drops_the_queued_burst_and_waits() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
    tx.try_send(()).unwrap();
    rx.recv().await.unwrap(); // the signal that woke the loop
    tx.try_send(()).unwrap(); // the rest of the editor's burst

    let start = tokio::time::Instant::now();
    settle_config_changes(&mut rx).await;

    assert!(start.elapsed() >= CONFIG_SETTLE, "settling should wait {:?}", CONFIG_SETTLE);
    assert!(rx.try_recv().is_err(), "the burst should have been drained into one reload");
}

#[tokio::test(start_paused = true)]
async fn a_change_during_the_settle_window_is_kept_for_another_reload() {
    // The drain happens before the wait, not after. A save that lands while
    // we wait is newer than anything drained, so it must survive and make
    // the loop reload once more — otherwise the last edit would be lost.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
    let late = tokio::spawn(async move {
        tokio::time::sleep(CONFIG_SETTLE / 2).await;
        tx.try_send(()).unwrap();
        tx
    });

    settle_config_changes(&mut rx).await;
    let _tx = late.await.unwrap();

    assert!(rx.try_recv().is_ok(), "a change during the settle window should stay queued");
}
