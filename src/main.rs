mod active_client;
mod analog;
mod config;
mod event_reader;
mod kwin_watcher;
mod lizard_mode;
mod resolver;
mod state_export;
mod udev_monitor;
mod virtual_devices;

use crate::udev_monitor::*;
use config::Config;
use std::env;
use tokio;
use tokio::task::JoinHandle;

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
            let default_config_path = format!("{}/.config/makima", user_home);
            println!(
                "\nMAKIMA_CONFIG environment variable is not set, defaulting to {:?}.\n",
                default_config_path
            );
            if !std::path::Path::new(&default_config_path).is_dir() {
                println!("Directory not found, exiting Makima.");
                std::process::exit(0);
            }
            default_config_path
        }
    };
    let config_files = load_config_files(&config_dir);
    let tasks: Vec<JoinHandle<()>> = Vec::new();
    start_monitoring_udev(config_files, config_dir, tasks).await;
}
