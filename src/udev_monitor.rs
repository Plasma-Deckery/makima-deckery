use crate::config::{Associations, Event};
use crate::event_reader::EventReader;
use crate::virtual_devices::VirtualDevices;
use crate::Config;
use evdev::{Device, EventStream};
use std::{env, path::Path, process::Command, sync::Arc};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use crate::kwin_watcher;
use tokio_stream::StreamExt;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

#[derive(Debug, Default, Eq, PartialEq, Hash, Clone)]
pub enum Client {
    #[default]
    Default,
    Class(String),
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

pub async fn start_monitoring_udev(mut config_files: Vec<Config>, config_dir: String, mut tasks: Vec<JoinHandle<()>>) {
    let environment = set_environment();
    let device_error_notify = Arc::new(Notify::new());
    // Separate signal from `device_error_notify`: a real evdev read error
    // means the physical device may genuinely be gone/changed, so that path
    // still does a full VirtualDevices recreate. Resume is (near-certainly)
    // the same device coming back, so it's safe to reuse the existing
    // VirtualDevices and skip the libinput-rediscovery cost (see issue #39).
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

    // EXPERIMENTAL: in-process suspend/resume watcher (see resume_watcher.rs).
    // Replaces the external makima-resume-watcher script + `systemctl restart`
    // with a direct D-Bus subscription that triggers the existing in-process
    // reinit path on resume, instead of a full process restart.
    tokio::spawn(crate::resume_watcher::start_resume_watcher(
        resume_notify.clone(),
    ));

    // Suppress Steam Deck Lizard Mode — persists across device reinitializations.
    // Reads SUPPRESS_LIZARD_MODE from any base config (default associations).
    // Gracefully skips if the setting is absent or on non-Steam-Deck hardware.
    {
        use crate::lizard_mode::LizardModeSuppression;
        let lizard_cfg = config_files
            .iter()
            .filter(|c| c.associations == Associations::default())
            .find_map(|c| c.settings.get("SUPPRESS_LIZARD_MODE"))
            .and_then(|v| LizardModeSuppression::from_setting(v));
        if let Some(cfg) = lizard_cfg {
            tokio::spawn(crate::lizard_mode::run_lizard_mode_suppression(cfg));
        }
    }

    let (mut prev_virt_dev, mut prev_modifiers) = launch_tasks(&config_files, &mut tasks, environment.clone(), device_error_notify.clone(), active_client.clone(), window_changed.clone(), None);
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
                        (prev_virt_dev, prev_modifiers) = launch_tasks(&config_files, &mut tasks, environment.clone(), device_error_notify.clone(), active_client.clone(), window_changed.clone(), None);
                    }
                }
            }
            _ = device_error_notify.notified() => {
                println!("---------------------\n\nDevice error detected, reinitializing...\n");
                release_held_modifiers(&prev_virt_dev, &prev_modifiers).await;
                for task in &tasks {
                    task.abort();
                }
                tasks.clear();
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                (prev_virt_dev, prev_modifiers) = launch_tasks(&config_files, &mut tasks, environment.clone(), device_error_notify.clone(), active_client.clone(), window_changed.clone(), None);
            }
            _ = resume_notify.notified() => {
                println!("---------------------\n\nResume detected, reinitializing...\n");
                release_held_modifiers(&prev_virt_dev, &prev_modifiers).await;
                for task in &tasks {
                    task.abort();
                }
                tasks.clear();
                (prev_virt_dev, prev_modifiers) = launch_tasks(&config_files, &mut tasks, environment.clone(), device_error_notify.clone(), active_client.clone(), window_changed.clone(), prev_virt_dev.clone());
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
                (prev_virt_dev, prev_modifiers) = launch_tasks(&config_files, &mut tasks, environment.clone(), device_error_notify.clone(), active_client.clone(), window_changed.clone(), None);
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
    reuse_virt_dev: Option<Arc<Mutex<VirtualDevices>>>,
) -> (Option<Arc<Mutex<VirtualDevices>>>, Arc<Mutex<Vec<Event>>>) {
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
    // How many currently-enumerated devices have a name matching some config
    // — computed up front so the reuse decision below can see the total
    // before committing to it per-device (see `should_reuse_virt_dev`).
    let matched_device_count = evdev::enumerate()
        .filter(|device| {
            config_files.iter().any(|config| {
                let split_config_name = config.name.split("::").collect::<Vec<&str>>();
                split_config_name[0] == device.1.name().unwrap_or_default().replace("/", "")
            })
        })
        .count();
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
                            (Client::Class(split_config_name[1].to_string()), 0)
                        }
                    }
                    3 => {
                        if let Ok(layout) = split_config_name[1].parse::<u16>() {
                            (Client::Class(split_config_name[2].to_string()), layout)
                        } else if let Ok(layout) = split_config_name[2].parse::<u16>() {
                            (Client::Class(split_config_name[1].to_string()), layout)
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
            let stream = Arc::new(Mutex::new(get_event_stream(
                Path::new(&event_device),
                config_list.clone(),
            )));
            // Reuse the previous VirtualDevices (same uinput devices, same
            // /dev/input/eventN nodes) instead of rebuilding from scratch when
            // asked to — libinput takes seconds to rediscover a freshly
            // recreated uinput device (see issue #39), so a resume or
            // transient-read-error reinit should keep the existing virtual
            // devices alive rather than tearing them down and back up.
            // Only safe when exactly one physical device matched this round:
            // handing every device the same reused instance would silently
            // give a second/third device virtual output built from a
            // different device's capabilities.
            let virt_dev = if should_reuse_virt_dev(matched_device_count, reuse_virt_dev.is_some()) {
                reuse_virt_dev.as_ref().unwrap().clone()
            } else {
                Arc::new(Mutex::new(VirtualDevices::new(device.1)))
            };
            virt_dev_holder = Some(virt_dev.clone());
            let reader = EventReader::new(
                config_list.clone(),
                virt_dev,
                stream,
                modifiers.clone(),
                modifier_was_activated.clone(),
                environment.clone(),
                device_error_notify.clone(),
                active_client.clone(),
                window_changed.clone(),
                std::path::PathBuf::from(&event_device),
            );
            tasks.push(tokio::spawn(start_reader(reader)));
            devices_found += 1
        }
    }
    if devices_found == 0 && !user_has_access {
        println!("No matching devices found.\nNote: make sure that your user has access to event devices.\n");
    } else if devices_found == 0 && user_has_access {
        println!("No matching devices found.\nNote: double-check that your device and its associated config file have the same name, as reported by 'evtest'.\n");
    }
    if devices_found == 0 && reuse_virt_dev.is_some() {
        // The caller's persisted VirtualDevices (and its live uinput devices)
        // has no task left holding a clone of it after this call, so it gets
        // dropped here — the next reinit that does find a device will pay
        // the full libinput-rediscovery cost again. Likely a transient
        // enumeration race (e.g. right after resume, before the device is
        // back), not a bug, but worth surfacing since it silently forfeits
        // the persistence this reuse mechanism exists for.
        println!("Warning: no matching devices found this round, persisted virtual devices could not be carried over and will be rebuilt on next reinit.\n");
    }
    (virt_dev_holder, modifiers)
}

/// Whether it's safe to reuse a previously-built `VirtualDevices` for this
/// reinit round instead of constructing a fresh one. Reuse was requested by
/// the caller (a resume/read-error reinit, not a USB replug or config
/// reload) — but only actually safe when exactly one physical device
/// matched this round's configs. Handing every device the same reused
/// instance when more than one device matches would give a second/third
/// device virtual output devices built from a *different* device's
/// capabilities. Doesn't affect Steam Deck (always exactly one matched
/// device) but matters for generic multi-controller configs.
fn should_reuse_virt_dev(matched_device_count: usize, reuse_requested: bool) -> bool {
    reuse_requested && matched_device_count == 1
}

pub async fn start_reader(reader: EventReader) {
    reader.start().await;
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

pub fn get_event_stream(path: &Path, config: Vec<Config>) -> EventStream {
    let mut device: Device = Device::open(path).expect("Couldn't open device path.");
    match config
        .iter()
        .find(|&x| x.associations == Associations::default())
        .unwrap()
        .settings
        .get("GRAB_DEVICE")
    {
        Some(value) => {
            if value == &true.to_string() {
                device
                    .grab()
                    .expect("Unable to grab device. Is another instance of Makima running?")
            }
        }
        None => device
            .grab()
            .expect("Unable to grab device. Is another instance of Makima running?"),
    }
    let stream: EventStream = device.into_event_stream().unwrap();
    return stream;
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

    #[test]
    fn launch_tasks_returns_modifiers_and_virt_dev_holder() {
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

        let (virt_dev_opt, modifiers) = launch_tasks(
            &config_files,
            &mut tasks,
            env,
            error_notify,
            client,
            window_changed,
            None,
        );

        if let Some(_) = virt_dev_opt {
            // If a device was found, modifiers must be a connected Arc.
            let _ = modifiers;
        }
    }

    #[test]
    fn should_reuse_virt_dev_only_when_requested_and_single_device() {
        assert!(!should_reuse_virt_dev(0, true), "no matched device: nothing to reuse for");
        assert!(should_reuse_virt_dev(1, true), "exactly one matched device: safe to reuse");
        assert!(!should_reuse_virt_dev(2, true), "multiple matched devices: reuse would mix up per-device capabilities");
        assert!(!should_reuse_virt_dev(1, false), "reuse not requested (USB replug / config reload): always rebuild");
        assert!(!should_reuse_virt_dev(0, false));
        assert!(!should_reuse_virt_dev(2, false));
    }
}
