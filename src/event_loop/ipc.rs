//! The control socket's commands.
//!
//! `pause`, `resume`, Gaming Mode, the analog export, and switching configs and
//! groups on and off. Everything that reaches the reader from outside the input
//! path — which is also why `paused` has no representation in `machine`: it
//! cannot be produced by an event, only by one of these.

use super::*;

impl EventReader {
    pub(super) async fn ipc_command_loop(&self, mut rx: broadcast::Receiver<String>) {
        loop {
            let cmd = match rx.recv().await {
                Ok(cmd) => cmd,
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            };
            let cmd = cmd.trim().to_string();
            let cmd = cmd.as_str();
            match cmd {
                "pause" | "resume" | "gaming_mode enable" | "gaming_mode disable" => {
                    if cmd == "pause" || cmd == "resume" {
                        *self.paused.lock().await = cmd == "pause";
                        // On pause: release all held output keys immediately so
                        // nothing stays stuck while an external process (e.g.
                        // deckery-auth-daemon) takes over the controller.
                        if cmd == "pause" {
                            self.release_all_held().await;
                        }
                    } else {
                        // Route through the unified Gaming Mode channel.
                        // gaming_mode_set_loop applies the change,
                        // fires haptics, and writes state.
                        let new_state = cmd == "gaming_mode enable";
                        println!("deckery: Gaming Mode trigger — IPC command: {}", cmd);
                        let _ = self.gaming_mode_tx.send(new_state).await;
                        continue; // state write handled by gaming_mode_set_loop
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
                    let base_name = self.base_name.clone();
                    let stack = if config.name == base_name {
                        vec![base_name]
                    } else {
                        vec![base_name, config.name.clone()]
                    };
                    let mut event_state = crate::state_export::build_state(
                        &config, &mods, is_paused, is_gaming,
                        &hk, &la, &stack, &self.gaming_mode_config,
                    );
                    event_state["trackpads"] = serde_json::Value::Null;
                    event_state["sticks"]    = serde_json::Value::Null;
                    event_state["imu"]       = serde_json::Value::Null;
                    event_state["context"]["analog_state_export"] = serde_json::Value::Bool(false);
                    // pause/resume is a discrete event — must not be dropped.
                    let _ = self.state_tx.send(StateCommand::SetEventState(Some(event_state))).await;
                }
                "analog-state-export on" | "analog-state-export off" => {
                    *self.analog_state_export.lock().await = cmd == "analog-state-export on";
                }
                _ if cmd.starts_with("config group enable ")
                    || cmd.starts_with("config group disable ") => {
                    let enabling = cmd.starts_with("config group enable ");
                    let group = if enabling {
                        cmd.trim_start_matches("config group enable ").trim()
                    } else {
                        cmd.trim_start_matches("config group disable ").trim()
                    };
                    if !self.registry.set_group_enabled(group, enabling) {
                        eprintln!("deckery: IPC config group enable/disable: no group named {:?}", group);
                        continue;
                    }
                    eprintln!("deckery: IPC config group {}: {:?}",
                        if enabling { "enable" } else { "disable" }, group);
                    let _ = self.state_tx.send(
                        StateCommand::SetLoadedConfigs(self.registry.snapshot())
                    ).await;
                }
                _ if cmd.starts_with("config enable ") || cmd.starts_with("config disable ") => {
                    let enabling = cmd.starts_with("config enable ");
                    let name = if enabling {
                        cmd.trim_start_matches("config enable ").trim()
                    } else {
                        cmd.trim_start_matches("config disable ").trim()
                    };
                    // set_enabled and snapshot are synchronous (std::sync::Mutex) — no .await.
                    if !self.registry.set_enabled(name, enabling) {
                        eprintln!("deckery: IPC config enable/disable: no config named {:?}", name);
                        continue;
                    }
                    eprintln!("deckery: IPC config {}: {:?}",
                        if enabling { "enable" } else { "disable" }, name);
                    let _ = self.state_tx.send(
                        StateCommand::SetLoadedConfigs(self.registry.snapshot())
                    ).await;
                }
                _ => {}
            }
        }
    }
}
