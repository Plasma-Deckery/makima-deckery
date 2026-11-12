//! The control socket's commands.
//!
//! `pause`, `resume`, Gaming Mode, the analog export, and switching configs and
//! groups on and off. Everything that reaches the reader from outside the input
//! path — which is also why `paused` has no representation in `machine`: it
//! cannot be produced by an event, only by one of these.
//!
//! What a line *means* is decided by `parse`, which is pure; what is done about
//! it is the loop below. The split is there so the grammar can be pinned by
//! tests: `pause` is how the controller is handed to the auth daemon for PIN
//! entry, and a command that silently stops matching leaves keys held during
//! unlock with nothing to say why.

use super::*;

/// One command the control socket understands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum IpcCommand {
    /// Suppress remaps and let go of every held output key.
    Pause,
    /// Undo `Pause`. Sends nothing.
    Resume,
    /// Ask for Gaming Mode on or off. The bool is exactly what is sent into the
    /// Gaming Mode channel; whoever reads it applies the change.
    GamingMode(bool),
    /// Switch the 60 Hz analog part of the state export on or off.
    AnalogStateExport(bool),
    /// Enable or disable a config group by name.
    ConfigGroup { enabling: bool, name: String },
    /// Enable or disable a single config by name.
    Config { enabling: bool, name: String },
}

/// What a line from the control socket asks for, or `None` if nothing.
///
/// The whole line is trimmed first, and after that matching is exact and case
/// sensitive: `pause` is a command, `Pause` and `pause now` are not, and two
/// spaces inside `gaming_mode  enable` make it a different, unknown line.
/// Anything unknown is `None` and is ignored without a word.
///
/// Config and group names are everything after the prefix, trimmed again, so
/// they may contain spaces. The prefix is removed with `trim_start_matches`,
/// which strips it as many times as it repeats — kept that way because this
/// function must say exactly what the loop used to do; see the tests for what
/// that costs. The group prefixes are tried before the config ones, though no
/// line can match both: `config group enable …` does not start with
/// `config enable `.
pub(super) fn parse(cmd: &str) -> Option<IpcCommand> {
    let cmd = cmd.trim();
    match cmd {
        "pause" => Some(IpcCommand::Pause),
        "resume" => Some(IpcCommand::Resume),
        "gaming_mode enable" => Some(IpcCommand::GamingMode(true)),
        "gaming_mode disable" => Some(IpcCommand::GamingMode(false)),
        "analog-state-export on" => Some(IpcCommand::AnalogStateExport(true)),
        "analog-state-export off" => Some(IpcCommand::AnalogStateExport(false)),
        _ if cmd.starts_with("config group enable ")
            || cmd.starts_with("config group disable ") => {
            let enabling = cmd.starts_with("config group enable ");
            let name = if enabling {
                cmd.trim_start_matches("config group enable ").trim()
            } else {
                cmd.trim_start_matches("config group disable ").trim()
            };
            Some(IpcCommand::ConfigGroup { enabling, name: name.to_string() })
        }
        _ if cmd.starts_with("config enable ") || cmd.starts_with("config disable ") => {
            let enabling = cmd.starts_with("config enable ");
            let name = if enabling {
                cmd.trim_start_matches("config enable ").trim()
            } else {
                cmd.trim_start_matches("config disable ").trim()
            };
            Some(IpcCommand::Config { enabling, name: name.to_string() })
        }
        _ => None,
    }
}

/// The snapshot written straight after a pause or resume.
///
/// The regular snapshot without its analog half: trackpads, sticks and IMU are
/// `null` and the analog export reads as off, whatever it really is. A pause is
/// a discrete event, and what the overlay needs from it is `paused` and the
/// held keys — which a pause keeps (issue #62), so the HUD still shows what the
/// user is holding.
pub(super) fn pause_resume_snapshot(
    config: &Config,
    modifiers: &[Event],
    paused: bool,
    gaming_mode: bool,
    held_keys: &[Event],
    last_action: &Option<LastAction>,
    base_name: &str,
    gaming_mode_config: &GamingModeConfig,
) -> serde_json::Value {
    let stack = crate::state_export::config_stack(base_name, &config.name);
    let mut event_state = crate::state_export::build_state(
        config, modifiers, paused, gaming_mode,
        held_keys, last_action, &stack, gaming_mode_config,
    );
    event_state["trackpads"] = serde_json::Value::Null;
    event_state["sticks"]    = serde_json::Value::Null;
    event_state["imu"]       = serde_json::Value::Null;
    event_state["context"]["analog_state_export"] = serde_json::Value::Bool(false);
    event_state
}


impl EventReader {
    pub(super) async fn ipc_command_loop(&self, mut rx: broadcast::Receiver<String>) {
        loop {
            let cmd = match rx.recv().await {
                Ok(cmd) => cmd,
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            };
            let Some(command) = parse(&cmd) else { continue };
            match command {
                IpcCommand::Pause | IpcCommand::Resume => {
                    let pausing = command == IpcCommand::Pause;
                    *self.paused.lock().await = pausing;
                    // On pause: release all held output keys immediately so
                    // nothing stays stuck while an external process (e.g.
                    // deckery-auth-daemon) takes over the controller. What
                    // the user still holds is kept (issue #62). Resume sends
                    // nothing: no output goes back down, the held state just
                    // carries on, and a custom modifier still held is marked
                    // used so its release does not tap.
                    if pausing {
                        self.pause_release_outputs().await;
                    } else {
                        self.resume_mark_held_modifiers().await;
                    }
                    // Write state for pause/resume immediately.
                    let is_paused = *self.paused.lock().await;
                    let is_gaming = *self.gaming_mode.lock().await;
                    const TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);
                    let config = match tokio::time::timeout(TIMEOUT, self.current_config.lock()).await {
                        Ok(g) => g.clone(), Err(_) => continue,
                    };
                    let mods = match tokio::time::timeout(TIMEOUT, self.modifiers.lock()).await {
                        Ok(g) => g.clone(), Err(_) => continue,
                    };
                    let la = match tokio::time::timeout(TIMEOUT, self.last_action.lock()).await {
                        Ok(g) => g.clone(), Err(_) => continue,
                    };
                    let hk = match tokio::time::timeout(TIMEOUT, self.held_keys.lock()).await {
                        Ok(g) => g.clone(), Err(_) => continue,
                    };
                    let event_state = pause_resume_snapshot(
                        &config, &mods, is_paused, is_gaming,
                        &hk, &la, &self.base_name, &self.gaming_mode_config,
                    );
                    // pause/resume is a discrete event — must not be dropped.
                    let _ = self.state_tx.send(StateCommand::SetEventState(Some(event_state))).await;
                }
                IpcCommand::GamingMode(new_state) => {
                    // Route through the unified Gaming Mode channel.
                    // gaming_mode_set_loop applies the change,
                    // fires haptics, and writes state.
                    println!("deckery: Gaming Mode trigger — IPC command: {}", cmd.trim());
                    let _ = self.gaming_mode_tx.send(new_state).await;
                }
                IpcCommand::AnalogStateExport(on) => {
                    *self.analog_state_export.lock().await = on;
                }
                IpcCommand::ConfigGroup { enabling, name: group } => {
                    if !self.registry.set_group_enabled(&group, enabling) {
                        eprintln!("deckery: IPC config group enable/disable: no group named {:?}", group);
                        continue;
                    }
                    eprintln!("deckery: IPC config group {}: {:?}",
                        if enabling { "enable" } else { "disable" }, group);
                    let _ = self.state_tx.send(
                        StateCommand::SetLoadedConfigs(self.registry.snapshot())
                    ).await;
                }
                IpcCommand::Config { enabling, name } => {
                    // set_enabled and snapshot are synchronous (std::sync::Mutex) — no .await.
                    if !self.registry.set_enabled(&name, enabling) {
                        eprintln!("deckery: IPC config enable/disable: no config named {:?}", name);
                        continue;
                    }
                    eprintln!("deckery: IPC config {}: {:?}",
                        if enabling { "enable" } else { "disable" }, name);
                    let _ = self.state_tx.send(
                        StateCommand::SetLoadedConfigs(self.registry.snapshot())
                    ).await;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "ipc_tests.rs"]
mod tests;
