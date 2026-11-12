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
        game_focus:          tokio::sync::watch::channel(Default::default()).1,
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

// ── Scan result (no_device slot) ──────────────────────────────────────────────
//
// The tests above can only ever reach the "nothing found" half of the scan —
// finding a device needs one. `report_scan_result` is the decision on its own,
// so the "found" half can be checked without hardware: that a scan which
// found something clears the red "no device" icon left by an earlier one.

fn drain(rx: &mut tokio::sync::mpsc::Receiver<StateCommand>) -> Vec<StateCommand> {
    let mut commands = Vec::new();
    while let Ok(cmd) = rx.try_recv() {
        commands.push(cmd);
    }
    commands
}

#[tokio::test]
async fn a_found_device_clears_the_no_device_error() {
    let (state_tx, mut state_rx) = tokio::sync::mpsc::channel(8);

    report_scan_result(1, true, &state_tx);

    let commands = drain(&mut state_rx);
    assert!(matches!(commands[0], StateCommand::SetLifecycle(AppLifecycle::Ready)),
        "expected SetLifecycle(Ready) first, got: {:?}", commands);
    assert!(matches!(&commands[1], StateCommand::ClearError { id } if id == "no_device"),
        "expected ClearError(no_device) second, got: {:?}", commands);
    assert_eq!(commands.len(), 2, "unexpected extra commands: {:?}", commands);
}

#[tokio::test]
async fn a_found_device_clears_the_error_even_without_group_access() {
    // Access only decides which hint goes with the error. Once a device is
    // running — say makima was started as a service — there is no error.
    let (state_tx, mut state_rx) = tokio::sync::mpsc::channel(8);

    report_scan_result(2, false, &state_tx);

    let commands = drain(&mut state_rx);
    assert!(commands.iter().any(|c| matches!(c, StateCommand::ClearError { id } if id == "no_device")),
        "expected ClearError(no_device), got: {:?}", commands);
    assert!(!commands.iter().any(|c| matches!(c, StateCommand::SetError { .. })),
        "expected no SetError, got: {:?}", commands);
}

#[tokio::test]
async fn nothing_found_without_access_points_at_permissions() {
    let (state_tx, mut state_rx) = tokio::sync::mpsc::channel(8);

    report_scan_result(0, false, &state_tx);

    let commands = drain(&mut state_rx);
    assert!(commands.iter().any(|c| matches!(c,
        StateCommand::SetError { id, message, severity: "error" }
            if id == "no_device" && message.contains("access"))),
        "expected an access hint in SetError(no_device), got: {:?}", commands);
}

#[tokio::test]
async fn nothing_found_with_access_points_at_the_device_names() {
    // With access the problem is the `[device] names` list, not permissions —
    // the message must say so, or people chase a permission problem they do
    // not have.
    let (state_tx, mut state_rx) = tokio::sync::mpsc::channel(8);

    report_scan_result(0, true, &state_tx);

    let commands = drain(&mut state_rx);
    assert!(commands.iter().any(|c| matches!(c,
        StateCommand::SetError { id, message, severity: "error" }
            if id == "no_device" && message.contains("[device] names"))),
        "expected a names hint in SetError(no_device), got: {:?}", commands);
}

#[tokio::test]
async fn a_reinit_that_finds_the_device_again_clears_the_earlier_error() {
    // The sequence a hidraw I/O error produces: the first scan after the
    // error comes up empty, the next one finds the controller again. The
    // state writer keeps the last command per id, so the order is what counts.
    let (state_tx, mut state_rx) = tokio::sync::mpsc::channel(8);

    report_scan_result(0, true, &state_tx);
    report_scan_result(1, true, &state_tx);

    let last_no_device = drain(&mut state_rx).into_iter().rev().find(|c| matches!(c,
        StateCommand::SetError { id, .. } | StateCommand::ClearError { id } if id == "no_device"));
    assert!(matches!(&last_no_device, Some(StateCommand::ClearError { .. })),
        "expected the last no_device command to be a ClearError, got: {:?}", last_no_device);
}

// ── Evdev access ──────────────────────────────────────────────────────────────

#[test]
fn the_input_group_grants_access() {
    assert_eq!(evdev_access("philipp wheel input audio\n"), EvdevAccess::InputGroup);
}

#[test]
fn root_grants_access() {
    assert_eq!(evdev_access("root\n"), EvdevAccess::Root);
}

#[test]
fn input_wins_over_root_when_both_are_present() {
    // Only the log line differs, but it is the one people paste into issues.
    assert_eq!(evdev_access("root input\n"), EvdevAccess::InputGroup);
}

#[test]
fn no_relevant_group_means_no_access() {
    assert_eq!(evdev_access("philipp wheel audio\n"), EvdevAccess::Missing);
}

#[test]
#[ignore = "substring match: a group like `uinput` or `chroot` counts as access — see issue #42 report"]
fn a_group_that_merely_contains_input_does_not_grant_access() {
    // `groups` prints whole names separated by spaces. Testing for a
    // substring lets `uinput` pass as `input`, which picks the wrong hint
    // when no device is found.
    assert_eq!(evdev_access("philipp uinput\n"), EvdevAccess::Missing);
    assert_eq!(evdev_access("philipp chroot\n"), EvdevAccess::Missing);
}

// ── Hidraw deduplication ──────────────────────────────────────────────────────

#[test]
fn the_first_node_claims_its_hidraw_sibling() {
    let mut seen = std::collections::HashSet::new();
    assert_eq!(claim_hidraw(&mut seen, Some(Path::new("/dev/hidraw2"))), HidrawClaim::Claimed);
}

#[test]
fn a_second_node_on_the_same_hidraw_is_skipped() {
    // hid-steam on newer kernels creates several evdev nodes per controller,
    // all sharing one hidraw. Opening each would double every event.
    let mut seen = std::collections::HashSet::new();
    claim_hidraw(&mut seen, Some(Path::new("/dev/hidraw2")));
    assert_eq!(claim_hidraw(&mut seen, Some(Path::new("/dev/hidraw2"))), HidrawClaim::AlreadyClaimed);
}

#[test]
fn a_different_hidraw_is_a_different_controller() {
    let mut seen = std::collections::HashSet::new();
    claim_hidraw(&mut seen, Some(Path::new("/dev/hidraw2")));
    assert_eq!(claim_hidraw(&mut seen, Some(Path::new("/dev/hidraw3"))), HidrawClaim::Claimed);
}

#[test]
fn a_node_without_hidraw_is_not_claimed_and_claims_nothing() {
    let mut seen = std::collections::HashSet::new();
    assert_eq!(claim_hidraw(&mut seen, None), HidrawClaim::NoSibling);
    assert!(seen.is_empty());
}

#[test]
fn a_fresh_scan_can_claim_the_same_hidraw_again() {
    // launch_tasks starts each scan with an empty set. That is what lets a
    // reinit after an I/O error pick the controller up again; a set that
    // outlived the scan would skip it as a duplicate of itself.
    let mut first_scan = std::collections::HashSet::new();
    claim_hidraw(&mut first_scan, Some(Path::new("/dev/hidraw2")));
    let mut second_scan = std::collections::HashSet::new();
    assert_eq!(claim_hidraw(&mut second_scan, Some(Path::new("/dev/hidraw2"))), HidrawClaim::Claimed);
}

// ── Held modifiers on reinit ──────────────────────────────────────────────────

#[test]
fn only_held_keys_are_released() {
    use crate::config::Axis;
    let held = vec![
        Event::Key(evdev::Key::KEY_LEFTCTRL),
        Event::Axis(Axis::BTN_DPAD_UP),
        Event::Hold,
        Event::Key(evdev::Key::KEY_LEFTALT),
    ];
    assert_eq!(keys_to_release(&held), vec![evdev::Key::KEY_LEFTCTRL, evdev::Key::KEY_LEFTALT]);
}

#[test]
fn nothing_held_releases_nothing() {
    assert!(keys_to_release(&[]).is_empty());
}

// ── Steam auto-detection ──────────────────────────────────────────────────────

fn base_with_auto_detect(name: &str, auto_detect: bool) -> crate::config::Config {
    let mut c = crate::config::Config::new_empty(name.to_string());
    c.gaming_mode_config.auto_detect_steam_games = auto_detect;
    c
}

#[test]
fn steam_detection_defaults_on_without_a_base_config() {
    // Before the first config loads there is nothing to read the setting
    // from; detecting is the documented default.
    assert!(auto_detect_steam(&[]));
}

#[test]
fn steam_detection_follows_the_base_config() {
    assert!(!auto_detect_steam(&[base_with_auto_detect("Steam Deck", false)]));
    assert!(auto_detect_steam(&[base_with_auto_detect("Steam Deck", true)]));
}
