mod active_client;
mod analog;
mod config;
mod device_session;
mod event_reader;
mod gesture_pad;
mod kde_input_defaults;
mod kwin_watcher;
mod steam_detector;
mod mt_trackpad;
mod pad_hidraw;
mod resolver;
mod steam_deck_controller;
mod scroll_pad;
mod state_export;
mod trackball;
mod trackpad;
mod trackpad_router;
mod udev_monitor;
mod virtual_devices;

use crate::udev_monitor::*;
use config::Config;
use std::env;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio;
use tokio::sync::Mutex;
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

pub fn load_config_files(config_dir: &str) -> Vec<Config> {
    let dir = match std::fs::read_dir(config_dir) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let mut config_files: Vec<Config> = Vec::new();
    for file in dir {
        let filename: String = file.as_ref().unwrap().file_name().into_string().unwrap();
        if filename.ends_with(".toml") && !filename.starts_with(".") {
            let name: String = filename.split(".toml").collect::<Vec<&str>>()[0].to_string();
            let config_file: Config =
                Config::new_from_file(file.unwrap().path().to_str().unwrap(), name);
            config_files.push(config_file);
        }
    }
    config_files
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
    let config_dir = match env::var("MAKIMA_CONFIG") {
        Ok(path) => {
            println!("\nMAKIMA_CONFIG set to {:?}.\n", path);
            if !std::path::Path::new(&path).is_dir() {
                println!("Directory not found, exiting Makima.");
                std::process::exit(0);
            }
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
            let user_config_path = format!("{}/.config/makima", user_home);
            // System-wide fallback: used when installed via RPM without per-user config.
            // The deckery RPM meta-package installs default configs here.
            let system_config_path = "/usr/share/makima-deckery/configs";
            if std::path::Path::new(&user_config_path).is_dir() {
                println!(
                    "\nMAKIMA_CONFIG environment variable is not set, using {:?}.\n",
                    user_config_path
                );
                user_config_path
            } else if std::path::Path::new(system_config_path).is_dir() {
                println!(
                    "\nMAKIMA_CONFIG environment variable is not set. \
                     No user config found at {:?}, falling back to system default at {:?}.\n",
                    user_config_path, system_config_path
                );
                system_config_path.to_string()
            } else {
                println!(
                    "\nMAKIMA_CONFIG environment variable is not set. \
                     No config found at {:?} or {:?}, exiting.\n",
                    user_config_path, system_config_path
                );
                std::process::exit(0);
            }
        }
    };
    let config_files = load_config_files(&config_dir);
    println!("makima: config loaded, +{}ms since startup", startup_ms());
    let tasks: Vec<JoinHandle<()>> = Vec::new();
    let gaming_mode: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    start_monitoring_udev(config_files, config_dir, tasks, gaming_mode).await;
}
