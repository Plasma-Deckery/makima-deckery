//! Running a command binding, and saying so when it does not run.
//!
//! This is the `[commands]` half of a config: `run = ["…"]` on a button. It
//! sat in `event_reader` because that is where the button press arrives, but
//! nothing here is about reading events — it is process spawning and failure
//! reporting, which issue #32 names as its own extraction (`subprocess.rs` in
//! that issue's table). Keeping it here is what lets the event reader shrink
//! rather than grow every time a command gains a behaviour.
//!
//! Two paths, because makima can be started either way:
//!
//! * **as the user** — `systemd-run` into the user's session, waited on, exit
//!   status and stderr read back.
//! * **as root via sudo** — double-`fork` + `setsid` + `runuser`, fire and
//!   forget. The double fork is what detaches the child from makima, so there
//!   is nothing left to wait on and no status to report.

use crate::session::Environment;
use crate::state_export::LastAction;
use crate::trackpad_router::StateWrite;

use fork::{fork, setsid, Fork};
use std::process::{Command, Stdio};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

/// Run every command of one binding.
///
/// `label` is the binding's own label and is only used to name it if the
/// command fails — the user pressed a button called "Screenshot to Clipboard",
/// not a shell line.
pub async fn run_command_binding(
    environment: &Environment,
    command_list: &[String],
    label: Option<String>,
    last_action: Arc<Mutex<Option<LastAction>>>,
    state_write_tx: mpsc::Sender<StateWrite>,
) {
    let (user, running_as_root) = if let Ok(sudo_user) = &environment.sudo_user {
        (Some(sudo_user), true)
    } else if let Ok(user) = &environment.user {
        (Some(user), false)
    } else {
        (None, false)
    };
    let Some(user) = user else { return };

    for command in command_list {
        if running_as_root {
            spawn_detached_as_user(user, command);
        } else {
            let child = spawn_in_user_session(user, command);

            let label = label.clone();
            let shown = command.clone();
            let last_action = last_action.clone();
            let state_write_tx = state_write_tx.clone();
            // Detached: the command outlives the key press, and the input path
            // must not wait three seconds for a screenshot. The failure toast
            // therefore arrives when the command ends, not when it started.
            tokio::spawn(async move {
                report_command_failure(child, label, shown, last_action, state_write_tx).await;
            });
        }
    }
}

/// Start `command` in the user's own systemd session and keep a handle on it.
///
/// The command is one argument, handed to a shell of its own. It used to be
/// interpolated into this line, which an outer `sh -c` then parsed — so a `;`,
/// `&&` or `|` in a binding bound at the wrong level and tore the command
/// apart before it ran. Nothing reported that, because nothing looked at the
/// result.
fn spawn_in_user_session(user: &str, command: &str) -> std::io::Result<tokio::process::Child> {
    tokio::process::Command::new("systemd-run")
        .args([
            "--wait", "--pipe", "--user", "--machine", &format!("{user}@"),
            "--", "systemd-run", "--user", "--scope",
            "sh", "-c", command,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
}

/// Drop from root to `user` and detach completely.
///
/// Nothing is waited on here, so a failure on this path stays invisible — the
/// double fork is exactly what removes the child makima could have waited for.
/// Reporting it would mean keeping the intermediate process around, which is
/// what the fork exists to avoid.
fn spawn_detached_as_user(user: &str, command: &str) {
    match fork() {
        Ok(Fork::Child) => match fork() {
            Ok(Fork::Child) => {
                setsid().unwrap();
                Command::new("runuser")
                    .args([user, "-c", command])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                std::process::exit(0);
            }
            Ok(Fork::Parent(_)) => std::process::exit(0),
            Err(_) => std::process::exit(1),
        },
        Ok(Fork::Parent(_)) => (),
        Err(_) => std::process::exit(1),
    }
}

/// Wait for a spawned command and, if it failed, make that visible.
///
/// A binding whose command does not run used to be indistinguishable from a
/// binding that never fired: both streams went to /dev/null and the exit
/// status was never read. The journal gets the detail; `last_action` carries a
/// short version to the overlay, which already draws it as a toast.
async fn report_command_failure(
    child: std::io::Result<tokio::process::Child>,
    label: Option<String>,
    command: String,
    last_action: Arc<Mutex<Option<LastAction>>>,
    state_write_tx: mpsc::Sender<StateWrite>,
) {
    let name = label.unwrap_or_else(|| command.clone());

    let detail = match child {
        // systemd-run itself could not be started. Nothing ran.
        Err(e) => format!("could not start: {e}"),
        Ok(child) => match child.wait_with_output().await {
            Err(e) => format!("could not be waited for: {e}"),
            Ok(out) if out.status.success() => return,
            Ok(out) => describe_failure(out.status.code(), &out.stderr),
        },
    };

    eprintln!("deckery: command binding {name:?} failed — {detail}\n  {command}");
    *last_action.lock().await = Some(LastAction {
        r#type: "error".to_string(),
        value: serde_json::json!(detail),
        label: Some(name),
        ts: crate::state_export::now_ts(),
        silent: false,
    });
    let _ = state_write_tx.send(StateWrite::Immediate).await;
}

/// Turn an exit status and stderr into one line worth showing.
///
/// systemd-run narrates every run on stderr, so its own chatter has to go —
/// what is left is either the command's real complaint or nothing at all.
fn describe_failure(code: Option<i32>, stderr: &[u8]) -> String {
    const NARRATION: [&str; 6] = [
        "Running as unit",
        "Finished with result",
        "Main processes terminated",
        "Service runtime",
        "CPU time consumed",
        "Memory peak",
    ];
    let stderr = String::from_utf8_lossy(stderr);
    let first = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !NARRATION.iter().any(|n| l.starts_with(n)));

    match (code, first) {
        (_, Some(msg)) => msg.to_string(),
        (Some(code), None) => format!("exited with {code}"),
        (None, None) => "killed by a signal".to_string(),
    }
}

#[cfg(test)]
#[path = "subprocess_tests.rs"]
mod tests;
