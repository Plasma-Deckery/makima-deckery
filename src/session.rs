//! What desktop session this is, and what is focused in it.
//!
//! These three types say nothing about input devices, yet they used to live in
//! `udev_monitor` because that is where they were first written. Eight modules
//! imported them from there — and not one of them wanted a udev function. The
//! cost was three dependency cycles, the worst of which pointed `compositor/`,
//! a self-contained adapter package, back at the top of the module tree.
//!
//! This module therefore depends on nothing inside the crate and never will:
//! it is the vocabulary the compositor adapters, the config registry and the
//! event reader share. `set_environment()` sits here for the same reason —
//! it is what builds an `Environment`, and it has nothing to do with udev.

use std::env;
use std::process::Command;

/// The focused window, as far as anything outside the compositor can tell.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub enum Client {
    #[default]
    Default,
    /// Window class + caption (both forwarded raw by the KWin script) +
    /// the PID of the focused window's owning process (KDE only; None elsewhere).
    Class(String, String, Option<u32>),
}

/// Which display server was found, if any.
#[derive(Clone)]
pub enum Server {
    Connected(String),
    Unsupported,
    Failed,
}

/// The process environment the readers are started with.
#[derive(Clone)]
pub struct Environment {
    pub user: Result<String, env::VarError>,
    pub sudo_user: Result<String, env::VarError>,
    pub server: Server,
}

/// Work out what session makima was started into, inheriting what it needs.
///
/// Lives next to `Environment` because that is what it builds. It used to sit
/// in `udev_monitor`, which has nothing to do with `DBUS_SESSION_BUS_ADDRESS`
/// or with passing a user session through sudo.
pub fn set_environment() -> Environment {
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

    // Compositors listed here get Server::Connected and enable per-app bindings.
    // KDE and Hyprland use event-driven adapters (compositor module).
    // sway and niri use the legacy get_active_window() polling fallback.
    let supported_compositors = vec!["KDE", "Hyprland", "sway", "niri"]
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
                    Currently supported desktops: Plasma/KWin (event-driven), Hyprland (event-driven), Sway (polling), Niri (polling), X11.\n", desktop);
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
