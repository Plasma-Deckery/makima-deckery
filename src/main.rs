mod active_client;
mod analog;
mod analog_export;
mod auto_mute;
mod config;
mod config_registry;
mod control_socket;
mod decode;
mod device_session;
mod device_tasks;
mod game_focus;
mod event_loop;
mod gesture_pad;
mod compositor;
mod kde_input_defaults;
mod resume_watcher;
mod steam_detector;
mod subprocess;
mod ledger;
mod machine;
mod mt_trackpad;
mod output;
mod process_switches;
mod pad_keys;
mod override_session;
mod preferences;
mod resolver;
mod scroll_pad;
mod settings;
mod session;
mod state_export;
mod state_writer;
mod trackball;
mod trackpad;
mod trackpad_router;
mod udev_monitor;
mod virtual_devices;

use crate::config_registry::ConfigRegistry;
use crate::udev_monitor::start_monitoring_udev;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio;
use tokio::task::JoinHandle;

/// Process start time, set once at the top of `main()`. Used to log
/// "+Nms since startup" markers at key init milestones — cheap way to
/// profile how long makima takes to become fully active, e.g. across
/// the suspend/resume restart done by `makima-resume-watcher`.
static START: OnceLock<Instant> = OnceLock::new();

/// Milliseconds elapsed since process start, for startup-profiling log lines.
pub fn startup_ms() -> u128 {
    START.get().map(|s| s.elapsed().as_millis()).unwrap_or(0)
}

/// Microseconds elapsed since process start.
///
/// Same clock as `startup_ms` and as `trace_record`'s `t_us`, so a recorded
/// input trace and a recorded action journal can be laid over each other.
pub fn startup_us() -> u128 {
    START.get().map(|s| s.elapsed().as_micros()).unwrap_or(0)
}

fn wait_for_config_dir(path: &std::path::Path) {
    if path.is_dir() {
        return;
    }
    eprintln!("deckery: config dir {:?} not found — waiting (install may still be running)", path);
    loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if path.is_dir() {
            eprintln!("deckery: config dir {:?} appeared, continuing startup", path);
            return;
        }
        eprintln!("deckery: still waiting for config dir {:?}", path);
    }
}


#[tokio::main]
async fn main() {
    START.set(Instant::now()).ok();
    // Any panic anywhere in the process must kill the whole process immediately.
    // Without this, Tokio tasks are independent: a panic in the event reader
    // would leave the Lizard Mode heartbeat running, keeping the Steam Deck
    // trackpads suppressed with no input handling active.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        std::process::exit(1);
    }));
    let roots = config_registry::ConfigRoots::resolve();
    eprintln!("deckery: system configs: {:?}", roots.system);
    eprintln!("deckery: user configs:   {:?}", roots.user);
    // Only the system root has to be there. The user root holds overrides and
    // preferences, and stays absent until the user actually makes one.
    wait_for_config_dir(&roots.system);
    let registry = ConfigRegistry::load(roots);
    // Hints resolve against the merged config, so a dead or ambiguous one can
    // only be detected here — not while parsing the file it was written in.
    // Reported, never fatal: a hint is display-only.
    for warning in registry.hint_warnings() {
        eprintln!("deckery: {warning}");
    }
    eprintln!("deckery: config loaded, +{}ms since startup", startup_ms());
    let state_tx = state_writer::spawn_state_writer();
    // Publish initial config list so the tray sees all configs on startup,
    // even before any device is connected.
    let _ = state_tx.try_send(state_writer::StateCommand::SetLoadedConfigs(registry.snapshot()));
    // Where those configs came from — the tray offers both folders for opening.
    let _ = state_tx.try_send(
        state_writer::StateCommand::SetConfigRoots(registry.roots().clone()));
    // A broken base config is a global failure — escalate to a top-level error
    // so the tray shows red, not just a per-config marker in the submenu.
    udev_monitor::report_base_config_error(&registry, &state_tx);
    let tasks: Vec<JoinHandle<()>> = Vec::new();
    let gaming_mode: Arc<tokio::sync::Mutex<bool>> = Arc::new(tokio::sync::Mutex::new(false));

    // The override session outlives every device generation, like the registry
    // it lays its layer into. Held in memory only: a restart starts without one.
    let (override_session, device_requests) =
        override_session::OverrideSession::new(registry.clone());
    // What the control socket switches outlives every device generation —
    // see `process_switches`. The socket is bound once, here.
    let switches = process_switches::Switches::default();
    tokio::spawn(control_socket::serve(control_socket::Control {
        session: override_session.clone(),
        switches: switches.clone(),
        registry: registry.clone(),
        state_tx: state_tx.clone(),
    }));

    start_monitoring_udev(
        registry, tasks, gaming_mode, state_tx, override_session, device_requests, switches,
    ).await;
}
