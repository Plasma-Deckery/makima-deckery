use crate::udev_monitor::{Client, Environment, Server};
use crate::config::Config;
use serde_json;
use std::process::{Command, Stdio};
use swayipc_async::Connection;
use x11rb::protocol::xproto::{get_input_focus, get_property, Atom, AtomEnum};

/// Returns `Client::Class(class, "")` if `class` matches any loaded config,
/// otherwise `Client::Default`.
fn match_class(class: String, config: &[Config]) -> Client {
    if config.iter().any(|x| match &x.associations.client {
        Client::Class(c, _, _) => c == &class,
        Client::Default => false,
    }) {
        Client::Class(class, String::new(), None)
    } else {
        Client::Default
    }
}

pub async fn get_active_window(environment: &Environment, config: &[Config]) -> Client {
    match &environment.server {
        Server::Connected(server) => {
            let server_str = server.as_str();
            match server_str {
                "Hyprland" => {
                    let query = Command::new("hyprctl")
                        .args(["activewindow", "-j"])
                        .output()
                        .unwrap();
                    if let Ok(reply) = serde_json::from_str::<serde_json::Value>(
                        std::str::from_utf8(query.stdout.as_slice()).unwrap(),
                    ) {
                        let class = reply["class"].to_string().replace("\"", "");
                        match_class(class, config)
                    } else {
                        Client::Default
                    }
                }
                "sway" => {
                    let class = match Connection::new().await.unwrap()
                        .get_tree().await.unwrap()
                        .find_focused(|window| window.focused)
                    {
                        Some(window) => match window.app_id {
                            Some(id) => id,
                            None => window
                                .window_properties
                                .and_then(|p| p.class)
                                .unwrap_or_default(),
                        },
                        None => return Client::Default,
                    };
                    match_class(class, config)
                }
                "niri" => {
                    let query = Command::new("niri")
                        .args(["msg", "-j", "focused-window"])
                        .output()
                        .unwrap();
                    if let Ok(reply) = serde_json::from_str::<serde_json::Value>(
                        std::str::from_utf8(query.stdout.as_slice()).unwrap(),
                    ) {
                        let class = reply["app_id"].to_string().replace("\"", "");
                        match_class(class, config)
                    } else {
                        Client::Default
                    }
                }
                "KDE" => {
                    let (user, running_as_root) =
                        if let Ok(sudo_user) = environment.sudo_user.clone() {
                            (Option::Some(sudo_user), true)
                        } else if let Ok(user) = environment.user.clone() {
                            (Option::Some(user), false)
                        } else {
                            (Option::None, false)
                        };
                    let Some(user) = user else { return Client::Default };
                    let output = if running_as_root {
                        Command::new("runuser")
                            .arg(user)
                            .arg("-c")
                            .arg("kdotool getactivewindow getwindowclassname")
                            .output()
                            .unwrap()
                    } else {
                        Command::new("sh")
                            .arg("-c")
                            .arg(format!("systemd-run --user --scope -M {}@ kdotool getactivewindow getwindowclassname", user))
                            .stderr(Stdio::null())
                            .output()
                            .unwrap()
                    };
                    let class = std::str::from_utf8(output.stdout.as_slice())
                        .unwrap()
                        .trim()
                        .to_string();
                    match_class(class, config)
                }
                "x11" => {
                    let Ok((connection, _)) = x11rb::connect(None) else {
                        return Client::Default;
                    };
                    let focused_window = match get_input_focus(&connection) {
                        Ok(cookie) => match cookie.reply() {
                            Ok(reply) => reply.focus,
                            Err(_) => return Client::Default,
                        },
                        Err(_) => return Client::Default,
                    };
                    let (wm_class, string): (Atom, Atom) =
                        (AtomEnum::WM_CLASS.into(), AtomEnum::STRING.into());
                    let class_reply = match get_property(
                        &connection,
                        false,
                        focused_window,
                        wm_class,
                        string,
                        0,
                        u32::MAX,
                    ) {
                        Ok(cookie) => match cookie.reply() {
                            Ok(reply) => reply,
                            Err(_) => return Client::Default,
                        },
                        Err(_) => return Client::Default,
                    };
                    let bytes = class_reply.value;
                    if let Some(middle) = bytes.iter().position(|&b| b == 0) {
                        let rest = &bytes[middle + 1..];
                        let rest = rest.strip_suffix(&[0]).unwrap_or(rest);
                        let Ok(class_str) = std::str::from_utf8(rest) else {
                            return Client::Default;
                        };
                        match_class(class_str.to_string(), config)
                    } else {
                        Client::Default
                    }
                }
                _ => Client::Default,
            }
        }
        Server::Unsupported => Client::Default,
        Server::Failed => Client::Default,
    }
}
