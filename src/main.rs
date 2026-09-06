mod active_client;
mod analog;
mod config;
mod config_registry;
mod device_session;
mod event_reader;
mod gesture_pad;
mod compositor;
mod kde_input_defaults;
mod resume_watcher;
mod steam_detector;
mod mt_trackpad;
mod resolver;
mod scroll_pad;
mod state_export;
mod state_writer;
mod trackball;
mod trackpad;
mod trackpad_router;
mod udev_monitor;
mod virtual_devices;

use crate::config_registry::ConfigRegistry;
use crate::udev_monitor::*;
use std::env;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio;
use tokio::net::UnixListener;
use tokio::sync::broadcast;
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

fn wait_for_config_dir(path: &str) {
    if std::path::Path::new(path).is_dir() {
        return;
    }
    eprintln!("deckery: config dir {:?} not found — waiting (tray may still be seeding)", path);
    loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if std::path::Path::new(path).is_dir() {
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
    // DECKERY_CONFIG is the canonical env var; MAKIMA_CONFIG is the legacy name
    // kept for backwards compatibility with hand-edited service overrides.
    let config_dir = match env::var("DECKERY_CONFIG").or_else(|_| env::var("MAKIMA_CONFIG")) {
        Ok(path) => {
            eprintln!("deckery: config dir: {:?}", path);
            wait_for_config_dir(&path);
            path
        }
        Err(_) => {
            let user_home = match env::var("HOME") {
                Ok(user_home) if user_home == "/root".to_string() => match env::var("SUDO_USER") {
                    Ok(sudo_user) => format!("/home/{}", sudo_user),
                    _ => user_home,
                },
                Ok(user_home) => user_home,
                _ => "/root".to_string(),
            };
            let path = format!("{}/.config/deckery", user_home);
            eprintln!("deckery: DECKERY_CONFIG not set, using {:?}", path);
            wait_for_config_dir(&path);
            path
        }
    };
    let registry = ConfigRegistry::load(&config_dir);
    eprintln!("deckery: config loaded, +{}ms since startup", startup_ms());
    let state_tx = state_writer::spawn_state_writer();
    // Publish initial config list so the tray sees all configs on startup,
    // even before any device is connected.
    let _ = state_tx.try_send(state_writer::StateCommand::SetLoadedConfigs(registry.snapshot()));
    // A broken base config is a global failure — escalate to a top-level error
    // so the tray shows red, not just a per-config marker in the submenu.
    udev_monitor::report_base_config_error(&registry, &state_tx);
    let tasks: Vec<JoinHandle<()>> = Vec::new();
    let gaming_mode: Arc<tokio::sync::Mutex<bool>> = Arc::new(tokio::sync::Mutex::new(false));

    // IPC socket — bound once here, broadcast to all active EventReaders.
    let (ipc_tx, _) = broadcast::channel::<String>(16);
    {
        let ipc_tx = ipc_tx.clone();
        tokio::spawn(async move {
            let _ = std::fs::remove_file("/tmp/makima-control.sock");
            let listener = match UnixListener::bind("/tmp/makima-control.sock") {
                Ok(l) => l,
                Err(e) => { eprintln!("deckery: IPC socket bind failed: {}", e); return; }
            };
            loop {
                let Ok((stream, _)) = listener.accept().await else { continue };
                use tokio::io::AsyncBufReadExt;
                let mut reader = tokio::io::BufReader::new(stream);
                let mut line = String::new();
                let read = tokio::time::timeout(
                    std::time::Duration::from_millis(500),
                    reader.read_line(&mut line),
                ).await;
                if read.is_err() || read.unwrap().is_err() { continue; }
                let cmd = line.trim().to_string();
                if !cmd.is_empty() { let _ = ipc_tx.send(cmd); }
            }
        });
    }

    start_monitoring_udev(registry, config_dir, tasks, gaming_mode, state_tx, ipc_tx).await;
}
