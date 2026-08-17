use crate::config::{Associations, Event};
use crate::event_reader::EventReader;
use crate::steam_deck_controller::{SteamDeckController, LizardModeSuppression};
use crate::steam_deck_controller::resume_watcher;
use crate::virtual_devices::VirtualDevices;
use crate::Config;
use std::{env, path::Path, process::Command, sync::Arc};
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::task::JoinHandle;
use crate::kwin_watcher;
use tokio_stream::StreamExt;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub enum Client {
    #[default]
    Default,
    /// Window class + caption (both forwarded raw by the KWin script) +
    /// the PID of the focused window's owning process (KDE only; None elsewhere).
    Class(String, String, Option<u32>),
}

#[derive(Clone)]
pub enum Server {
    Connected(String),
    Unsupported,
    Failed,
}

#[derive(Clone)]
pub struct Environment {
    pub user: Result<String, env::VarError>,
    pub sudo_user: Result<String, env::VarError>,
    pub server: Server,
}

pub async fn start_monitoring_udev(mut config_files: Vec<Config>, config_dir: String, mut tasks: Vec<JoinHandle<()>>, gaming_mode: Arc<Mutex<bool>>) {
    let environment = set_environment();
    let device_error_notify = Arc::new(Notify::new());
    // resume_notify is shared between the resume watcher (fires it on logind
    // PrepareForSleep) and the per-device reconnecting reader task (listens to
    // it for proactive reconnect on suspend). udev_monitor no longer acts on it
    // directly — resume is now handled transparently inside the controller.
    let resume_notify = Arc::new(Notify::new());
    let active_client: Arc<Mutex<Client>> = Arc::new(Mutex::new(Client::Default));
    let window_changed: Arc<Notify> = Arc::new(Notify::new());

    // Start the KWin watcher once — persists across device reinitializations.
    if let Server::Connected(s) = &environment.server {
        if s == "KDE" {
            tokio::spawn(kwin_watcher::start_kwin_watcher(
                active_client.clone(),
                window_changed.clone(),
            ));
        }
    }

    // Spawn the resume watcher once — fires resume_notify on logind
    // PrepareForSleep(false). Lives for the lifetime of the process.
    tokio::spawn(resume_watcher::start_resume_watcher(resume_notify.clone()));

    // Parse Lizard Mode config once from the base config section.
    // Passed into launch_tasks so controller.start() can set up the heartbeat.
    // Defaults to "buttons,mouse"; set SUPPRESS_LIZARD_MODE = "false" to opt out.
    let lizard_cfg = {
        let setting = config_files
            .iter()
            .filter(|c| c.associations == Associations::default())
            .find_map(|c| c.settings.get("SUPPRESS_LIZARD_MODE"))
            .map(|s| s.as_str())
            .unwrap_or("buttons,mouse");
        LizardModeSuppression::from_setting(setting)
    };

    let (mut prev_virt_dev, mut prev_modifiers) = launch_tasks(&config_files, &mut tasks, environment.clone(), device_error_notify.clone(), active_client.clone(), window_changed.clone(), resume_notify.clone(), lizard_cfg.clone(), gaming_mode.clone());
    let mut monitor = tokio_udev::AsyncMonitorSocket::new(
        tokio_udev::MonitorBuilder::new()
            .unwrap()
            .match_subsystem(std::ffi::OsStr::new("input"))
            .unwrap()
            .listen()
            .unwrap(),
    )
    .unwrap();

    // Config file watcher — reloads on any .toml change in the watched dirs.
    // Watches the config dir directly, plus the parent dirs of any symlinked
    // .toml files (e.g. files in a git repo). Watching parent dirs instead of
    // individual files survives editor renames (sed -i, vim swapfiles, etc.)
    // that would otherwise invalidate an inode-based per-file watch.
    // Events are filtered to .toml files so unrelated files in those dirs are ignored.
    let (config_tx, mut config_rx) = tokio::sync::mpsc::channel::<()>(1);
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                use notify::EventKind::*;
                match event.kind {
                    Create(_) | Modify(_) | Remove(_) => {
                        let is_toml = event.paths.iter().any(|p| {
                            p.extension().and_then(|e| e.to_str()) == Some("toml")
                        });
                        if is_toml { let _ = config_tx.try_send(()); }
                    }
                    _ => {}
                }
            }
        },
        notify::Config::default(),
    ).expect("Failed to create config file watcher");
    watcher.watch(std::path::Path::new(&config_dir), RecursiveMode::NonRecursive)
        .expect("Failed to watch config directory");
    // Also watch parent dirs of symlink targets.
    if let Ok(entries) = std::fs::read_dir(&config_dir) {
        let mut extra_dirs: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                if let Ok(real) = std::fs::canonicalize(&path) {
                    if real != path {
                        if let Some(parent) = real.parent() {
                            extra_dirs.insert(parent.to_path_buf());
                        }
                    }
                }
            }
        }
        for dir in extra_dirs {
            let _ = watcher.watch(&dir, RecursiveMode::NonRecursive);
        }
    }

    loop {
        tokio::select! {
            event = monitor.next() => {
                if let Some(Ok(event)) = event {
                    if is_mapped(&event.device(), &config_files) {
                        println!("---------------------\n\nReinitializing...\n");
                        release_held_modifiers(&prev_virt_dev, &prev_modifiers).await;
                        for task in &tasks {
                            task.abort();
                        }
                        tasks.clear();
                        (prev_virt_dev, prev_modifiers) = launch_tasks(&config_files, &mut tasks, environment.clone(), device_error_notify.clone(), active_client.clone(), window_changed.clone(), resume_notify.clone(), lizard_cfg.clone(), gaming_mode.clone());
                    }
                }
            }
            _ = device_error_notify.notified() => {
                // A genuine device error (USB unplug or reconnect timeout from the
                // controller's reconnecting task). Full reinit — rebuild VirtualDevices.
                println!("---------------------\n\nDevice error detected, reinitializing...\n");
                release_held_modifiers(&prev_virt_dev, &prev_modifiers).await;
                for task in &tasks {
                    task.abort();
                }
                tasks.clear();
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                (prev_virt_dev, prev_modifiers) = launch_tasks(&config_files, &mut tasks, environment.clone(), device_error_notify.clone(), active_client.clone(), window_changed.clone(), resume_notify.clone(), lizard_cfg.clone(), gaming_mode.clone());
            }
            Some(_) = config_rx.recv() => {
                // Debounce: drain any queued events, then wait briefly for the
                // editor to finish writing.
                while config_rx.try_recv().is_ok() {}
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                println!("---------------------\n\nConfig changed, reloading...\n");
                config_files = crate::load_config_files(&config_dir);
                release_held_modifiers(&prev_virt_dev, &prev_modifiers).await;
                for task in &tasks {
                    task.abort();
                }
                tasks.clear();
                (prev_virt_dev, prev_modifiers) = launch_tasks(&config_files, &mut tasks, environment.clone(), device_error_notify.clone(), active_client.clone(), window_changed.clone(), resume_notify.clone(), lizard_cfg.clone(), gaming_mode.clone());
            }
        }
    }
}

/// Before destroying the old virtual devices during reinit, release all held
/// modifier output keys so the kernel (and thus XWayland/compositor) knows
/// those keys are no longer pressed. Without this, a stuck-modifier state
/// persists across the reinit — modifiers held at reinit time (e.g. Ctrl+Alt
/// from a paddle+button combo) are never released, causing phantom combo
/// activation until the user physically presses and releases those keys again.
async fn release_held_modifiers(
    prev_virt_dev: &Option<Arc<Mutex<VirtualDevices>>>,
    prev_modifiers: &Arc<Mutex<Vec<Event>>>,
) {
    let virt_dev = match prev_virt_dev {
        Some(vd) => vd,
        None => return,
    };
    let held = prev_modifiers.lock().await.clone();
    if held.is_empty() {
        return;
    }
    let mut vd = virt_dev.lock().await;
    for modifier in &held {
        if let Event::Key(key) = modifier {
            let _ = vd.keys.emit(&[
                evdev::InputEvent::new_now(evdev::EventType::KEY, key.code(), 0),
            ]);
        }
    }
}

pub fn launch_tasks(
    config_files: &Vec<Config>,
    tasks: &mut Vec<JoinHandle<()>>,
    environment: Environment,
    device_error_notify: Arc<Notify>,
    active_client: Arc<Mutex<Client>>,
    window_changed: Arc<Notify>,
    resume_notify: Arc<Notify>,
    lizard_cfg: Option<LizardModeSuppression>,
    gaming_mode: Arc<Mutex<bool>>,
) -> (Option<Arc<Mutex<VirtualDevices>>>, Arc<Mutex<Vec<Event>>>) {
    // Unified Gaming Mode channel: steam detection and IPC both send here.
    // The EventReader's gaming_mode_set_loop is the sole consumer.
    let (gaming_mode_tx, gaming_mode_rx) = mpsc::channel::<bool>(32);

    // Steam detection task — spawned once per session, outside any EventReader.
    // Sends detected gaming state changes via gaming_mode_tx.
    // No compositor check needed: if window_changed is never fired (non-KDE),
    // the task simply blocks on notified().await forever — zero overhead.
    let auto_detect = config_files
        .iter()
        .find(|c| c.associations == Associations::default())
        .map(|c| c.gaming_mode_config.auto_detect_steam_games)
        .unwrap_or(true);
    tokio::spawn(crate::steam_detector::steam_detection_task(
        window_changed.clone(),
        active_client.clone(),
        gaming_mode_tx.clone(),
        auto_detect,
    ));

    // The rx goes to the first matched reader (wrapped in Option so only one
    // reader takes it). Subsequent readers (multi-device) get a dead receiver.
    let mut gaming_mode_rx_opt = Some(gaming_mode_rx);

    let modifiers: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Default::default()));
    let modifier_was_activated: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));
    let mut virt_dev_holder: Option<Arc<Mutex<VirtualDevices>>> = None;
    let user_has_access = match Command::new("groups").output() {
        Ok(groups)
            if std::str::from_utf8(&groups.stdout.as_slice())
                .unwrap()
                .contains("input") =>
        {
            println!("Evdev permissions available.\nScanning for event devices with a matching config file...\n");
            true
        }
        Ok(groups)
            if std::str::from_utf8(&groups.stdout.as_slice())
                .unwrap()
                .contains("root") =>
        {
            println!("Root permissions available.\nScanning for event devices with a matching config file...\n");
            true
        }
        Ok(_) => {
            println!("Warning: user has no access to event devices, Makima might not be able to detect all connected devices.\n\
                    Note: Run Makima with 'sudo -E makima' or as a system service. Refer to the docs for more info. Continuing...\n");
            false
        }
        Err(_) => {
            println!(
                "Warning: unable to determine if user has access to event devices. Continuing...\n"
            );
            false
        }
    };
    let devices: evdev::EnumerateDevices = evdev::enumerate();
    let mut devices_found = 0;
    for device in devices {
        let mut config_list: Vec<Config> = Vec::new();
        for mut config in config_files.clone() {
            let split_config_name = config.name.split("::").collect::<Vec<&str>>();
            let associated_device_name = split_config_name[0];
            if associated_device_name == device.1.name().unwrap().replace("/", "") {
                let (window_class, layout) = match split_config_name.len() {
                    1 => (Client::Default, 0),
                    2 => {
                        if let Ok(layout) = split_config_name[1].parse::<u16>() {
                            (Client::Default, layout)
                        } else {
                            (Client::Class(split_config_name[1].to_string(), String::new(), None), 0)
                        }
                    }
                    3 => {
                        if let Ok(layout) = split_config_name[1].parse::<u16>() {
                            (Client::Class(split_config_name[2].to_string(), String::new(), None), layout)
                        } else if let Ok(layout) = split_config_name[2].parse::<u16>() {
                            (Client::Class(split_config_name[1].to_string(), String::new(), None), layout)
                        } else {
                            println!("Warning: unable to parse layout number in {}, treating it as default.", config.name);
                            (Client::Default, 0)
                        }
                    }
                    _ => {
                        println!("Warning: too many arguments in config file name {}, treating it as default.", config.name);
                        (Client::Default, 0)
                    }
                };
                config.associations.client = window_class;
                config.associations.layout = layout;
                config_list.push(config.clone());
            };
        }
        // Merge base config into every app-specific config so they only need
        // to declare overrides. The base is the one with default associations.
        if let Some(base) = config_list.iter().find(|x| x.associations == Associations::default()).cloned() {
            for config in config_list.iter_mut() {
                if config.associations != Associations::default() {
                    config.merge_base(&base);
                }
            }
        }
        if config_list.len() > 0
            && !config_list
                .iter()
                .any(|x| x.associations == Associations::default())
        {
            config_list.push(Config::new_empty(device.1.name().unwrap().replace("/", "")));
        }
        let event_device = device.0.as_path().to_str().unwrap().to_string();
        if config_list.len() != 0 {
            let grab = config_list
                .iter()
                .find(|c| c.associations == Associations::default())
                .and_then(|c| c.settings.get("GRAB_DEVICE"))
                .map_or(true, |v| v == "true");
            let controller = SteamDeckController::from_evdev(Path::new(&event_device));
            // controller.start() spawns the reconnecting evdev reader, hidraw
            // reader/writer, and Lizard Mode heartbeat — all suspend-transparent.
            // On genuine device removal it fires device_error_notify and closes
            // the channel — EventReader exits, udev_monitor does a full reinit.
            let session = controller.start(
                grab,
                resume_notify.clone(),
                device_error_notify.clone(),
                lizard_cfg.clone(),
            );
            // Steam Deck controller capabilities are known hardware constants —
            // no runtime query needed. is_tablet=false: no BTN_TOOL_PEN.
            // max_abs_wheel=0: EventReader uses its default scroll scaling.
            let is_tablet = false;
            let max_abs_wheel = 0i32;
            let virt_dev = Arc::new(Mutex::new(VirtualDevices::new(device.1)));
            virt_dev_holder = Some(virt_dev.clone());
            // First reader takes the real rx; subsequent readers get a dead one.
            let gaming_rx = gaming_mode_rx_opt.take().unwrap_or_else(|| {
                let (_, dead_rx) = mpsc::channel(1);
                dead_rx
            });
            let reader = EventReader::new(
                config_list.clone(),
                virt_dev,
                session.event_rx,
                is_tablet,
                max_abs_wheel,
                session.pad_rx,
                session.hidraw_tx,
                modifiers.clone(),
                modifier_was_activated.clone(),
                environment.clone(),
                active_client.clone(),
                window_changed.clone(),
                std::path::PathBuf::from(&event_device),
                gaming_mode.clone(),
                gaming_mode_tx.clone(),
            );
            tasks.push(tokio::spawn(start_reader(reader, gaming_rx)));
            devices_found += 1
        }
    }
    if devices_found == 0 && !user_has_access {
        println!("No matching devices found.\nNote: make sure that your user has access to event devices.\n");
    } else if devices_found == 0 && user_has_access {
        println!("No matching devices found.\nNote: double-check that your device and its associated config file have the same name, as reported by 'evtest'.\n");
    }
    (virt_dev_holder, modifiers)
}

pub async fn start_reader(reader: EventReader, gaming_rx: mpsc::Receiver<bool>) {
    reader.start(gaming_rx).await;
}

fn set_environment() -> Environment {
    match env::var("DBUS_SESSION_BUS_ADDRESS") {
        Ok(_) => copy_variables(),
        Err(_) => {
            let uid = Command::new("sh").arg("-c").arg("id -u").output().unwrap();
            let uid_number = std::str::from_utf8(uid.stdout.as_slice()).unwrap().trim();
            if uid_number != "0" {
                let bus_address = format!("unix:path=/run/user/{}/bus", uid_number);
                env::set_var("DBUS_SESSION_BUS_ADDRESS", bus_address);
                copy_variables()
            } else {
                println!("Warning: unable to inherit user environment.\n\
                        Launch Makima with 'sudo -E makima' or make sure that your systemd unit is running with the 'User=<username>' parameter.\n");
            }
        }
    };
    if let (Err(env::VarError::NotPresent), Ok(_)) =
        (env::var("XDG_SESSION_TYPE"), env::var("WAYLAND_DISPLAY"))
    {
        env::set_var("XDG_SESSION_TYPE", "wayland")
    }

    let supported_compositors = vec!["Hyprland", "sway", "KDE", "niri"]
        .into_iter()
        .map(|str| String::from(str))
        .collect::<Vec<String>>();
    let (x11, wayland) = (String::from("x11"), String::from("wayland"));
    let server: Server = match (
        env::var("XDG_SESSION_TYPE"),
        env::var("XDG_CURRENT_DESKTOP"),
    ) {
        (Ok(session), Ok(desktop))
            if session == wayland && supported_compositors.contains(&desktop) =>
        {
            println!("Running on {}, per application bindings enabled.", desktop);
            Server::Connected(desktop)
        }
        (Ok(session), Ok(desktop)) if session == wayland => {
            println!("Warning: unsupported compositor: {}, won't be able to change bindings according to the active window.\n\
                    Currently supported desktops: Hyprland, Sway, Niri, Plasma/KWin, X11.\n", desktop);
            Server::Unsupported
        }
        (Ok(session), _) if session == x11 => {
            println!("Running on X11, per application bindings enabled.");
            Server::Connected(session)
        }
        (Ok(session), Err(_)) if session == wayland => {
            println!("Warning: unable to retrieve the current desktop based on XDG_CURRENT_DESKTOP env var.\n\
                    Won't be able to change bindings according to the active window.\n");
            Server::Unsupported
        }
        (Err(_), _) => {
            println!("Warning: unable to retrieve the session type based on XDG_SESSION_TYPE or WAYLAND_DISPLAY env vars.\n\
                    Is your Wayland compositor or X server running?\n\
                    Exiting Makima.");
            std::process::exit(0);
        }
        _ => Server::Failed,
    };

    Environment {
        user: env::var("USER"),
        sudo_user: env::var("SUDO_USER"),
        server,
    }
}

fn copy_variables() {
    let command = Command::new("sh")
        .arg("-c")
        .arg("systemctl --user show-environment")
        .output()
        .unwrap();
    let vars = std::str::from_utf8(command.stdout.as_slice())
        .unwrap()
        .split("\n")
        .collect::<Vec<&str>>();
    for var in vars {
        if let Some((variable, value)) = var.split_once("=") {
            if let Err(env::VarError::NotPresent) = env::var(variable) {
                env::set_var(variable, value);
            } else if variable == "PATH" {
                env::set_var("PATH", format!("{}:{}", value, env::var("PATH").unwrap()));
            }
        }
    }
}


pub fn is_mapped(udev_device: &tokio_udev::Device, config_files: &Vec<Config>) -> bool {
    match udev_device.devnode() {
        Some(devnode) => {
            let evdev_devices: evdev::EnumerateDevices = evdev::enumerate();
            for evdev_device in evdev_devices {
                for config in config_files {
                    if config
                        .name
                        .contains(&evdev_device.1.name().unwrap().to_string().replace("/", ""))
                        && devnode.to_path_buf() == evdev_device.0
                    {
                        return true;
                    }
                }
            }
        }
        _ => return false,
    }
    return false;
}

#[cfg(test)]
mod tests {
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
        let resume_notify = Arc::new(Notify::new());
        let (virt_dev_opt, modifiers) = launch_tasks(
            &config_files,
            &mut tasks,
            env,
            error_notify,
            client,
            window_changed,
            resume_notify,
            None, // no lizard cfg in test
            gaming_mode,
        );

        if let Some(_) = virt_dev_opt {
            // If a device was found, modifiers must be a connected Arc.
            let _ = modifiers;
        }
    }

}
