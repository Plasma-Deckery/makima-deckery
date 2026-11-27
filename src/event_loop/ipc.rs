//! The control socket's commands.
//!
//! `pause`, `resume`, Gaming Mode, the analog export, and switching configs and
//! groups on and off. Everything that reaches the reader from outside the input
//! path — which is also why `paused` has no representation in `machine`: it
//! cannot be produced by an event, only by one of these.
//!
//! What a line *means* is decided by `parse`, which is pure; the control socket
//! carries it out (`crate::control_socket`), and what the reader does about a
//! pause is below: it follows the process's pause switch, so a rebuild can
//! neither miss a pause nor undo one. The split is there so the grammar can be pinned by
//! tests: `pause` is how the controller is handed to the auth daemon for PIN
//! entry, and a command that silently stops matching leaves keys held during
//! unlock with nothing to say why.

use super::*;

/// One command the control socket understands. All of them are carried out
/// by the socket itself — see `crate::control_socket`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IpcCommand {
    /// Suppress remaps and let go of every held output key.
    Pause,
    /// Undo `Pause`. Sends nothing.
    Resume,
    /// Ask for Gaming Mode on or off. The bool is exactly what is sent into the
    /// Gaming Mode channel; whoever reads it applies the change.
    GamingMode(bool),
    /// Switch the 60 Hz analog part of the state export on or off. Carried
    /// out by the control socket itself, like `Override`.
    AnalogStateExport(bool),
    /// Enable or disable a config group by name.
    ConfigGroup { enabling: bool, name: String },
    /// Enable or disable a single config by name.
    Config { enabling: bool, name: String },
    /// One of the override session's commands. Answered by the control socket
    /// itself, which is why it never reaches a reader — see
    /// `crate::control_socket`.
    Override(crate::override_session::OverrideCommand),
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
///
/// Every line starting with the word `override` is an override command, a
/// malformed one included: that one is answered with an error, because the
/// sender waits for a reply.
pub(crate) fn parse(cmd: &str) -> Option<IpcCommand> {
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
        _ if cmd == "override" || cmd.starts_with("override ") => Some(IpcCommand::Override(
            crate::override_session::command::parse(&cmd["override".len()..]),
        )),
        _ => None,
    }
}

/// The snapshot written straight after a pause or resume.
///
/// The regular snapshot without its analog half: trackpads, sticks and IMU are
/// `null` and the analog export reads as off — the IPC loop lays the real
/// switch over that before the snapshot goes out. A pause is
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
    /// Follow the process's pause switch.
    ///
    /// The control socket sets it; this reader does what a pause or a resume
    /// does to its input path when the switch changes. A reader built while
    /// paused started paused (see `assemble`), so a rebuild never resumes.
    pub(super) async fn pause_loop(&self) {
        // Built at the switch's value already; this only catches a change
        // that landed between the build and this loop's first poll.
        self.sync_pause().await;
        loop {
            let changed = self.pause_rx.lock().await.changed().await;
            if changed.is_err() { return; }
            self.sync_pause().await;
        }
    }

    /// Bring this reader to the pause switch's value, if it is not there.
    pub(super) async fn sync_pause(&self) {
        let want = *self.pause_rx.lock().await.borrow_and_update();
        if want != *self.paused.lock().await {
            self.apply_pause(want).await;
        }
    }

    /// Apply the control socket's Gaming Mode requests for as long as this
    /// reader runs. Only the reader that also takes the Gaming Mode channel
    /// runs this; a request sent while no reader runs waits in the queue.
    pub(super) async fn gaming_requests_loop(&self) {
        let queue = self.switches.gaming.queue();
        let mut queue = queue.lock().await;
        while let Some(on) = queue.recv().await {
            self.apply_gaming_request(on).await;
        }
    }

    /// What a pause or a resume does to the input path, and the snapshot it
    /// writes at once.
    pub(super) async fn apply_pause(&self, pausing: bool) {
        *self.paused.lock().await = pausing;
        // On pause: release all held output keys immediately so nothing stays
        // stuck while an external process (e.g. deckery-auth-daemon) takes
        // over the controller. What the user still holds is kept (issue #62).
        // Resume sends nothing: no output goes back down, the held state just
        // carries on, and a custom modifier still held is marked used so its
        // release does not tap.
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
            Ok(g) => g.clone(), Err(_) => return,
        };
        let mods = match tokio::time::timeout(TIMEOUT, self.modifiers.lock()).await {
            Ok(g) => g.clone(), Err(_) => return,
        };
        let la = match tokio::time::timeout(TIMEOUT, self.last_action.lock()).await {
            Ok(g) => g.clone(), Err(_) => return,
        };
        let hk = match tokio::time::timeout(TIMEOUT, self.held_keys.lock()).await {
            Ok(g) => g.clone(), Err(_) => return,
        };
        let mut event_state = pause_resume_snapshot(
            &config, &mods, is_paused, is_gaming,
            &hk, &la, &self.base_name, &self.gaming_mode_config,
        );
        self.mark_override(&mut event_state);
        // The analog half is left out, but the switch is reported as it is: a
        // snapshot saying "off" while it is on read as a lost
        // `analog-state-export on` in every diagnosis.
        event_state["context"]["analog_state_export"] =
            serde_json::Value::Bool(self.switches.analog.is_on());
        // pause/resume is a discrete event — must not be dropped.
        let _ = self.state_tx.send(StateCommand::SetEventState(Some(event_state))).await;
    }
}

#[cfg(test)]
#[path = "ipc_tests.rs"]
mod tests;
