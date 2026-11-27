//! The control socket: one line in per connection, broadcast to the readers —
//! or, for an override command, answered with one line back.
//!
//! Moved out of `main` when the override session needed replies. Every other
//! command is fire-and-forget, as it always was: the auth daemon, the tray and
//! the Gaming Mode toggle write a line and close.
//!
//! Two kinds of command are carried out right here instead of being
//! broadcast, because they belong to the process rather than to a reader:
//!
//! * an override command, against the one shared session, with its reply
//!   written back on the same connection before it is closed;
//! * `analog-state-export on|off`, on the one shared switch (see
//!   `crate::analog_export`). A broadcast during a rebuild of the device
//!   tasks reaches no reader at all, and was lost.

use crate::event_loop::{parse_ipc_line, IpcCommand};
use crate::analog_export::AnalogExport;
use crate::override_session::OverrideSession;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::sync::broadcast;

/// How long a client gets to send its line, and to take the reply.
const CLIENT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// Bind the socket and serve it for the life of the process.
pub async fn serve(ipc_tx: broadcast::Sender<String>, session: Arc<OverrideSession>, analog: AnalogExport) {
    // Bind in $XDG_RUNTIME_DIR (= /run/user/<uid>), not /tmp.
    //
    // /tmp is mode 1777, so any local process can create the socket
    // name before we bind it. This socket accepts "pause", i.e. it can
    // switch off input remapping — not something to leave squattable.
    // The runtime dir is mode 0700 and per-user, which additionally
    // lets deckery-auth-daemon address each user's instance separately
    // (it scans /run/user/*/makima-control.sock); a single /tmp path
    // cannot express that. It is also why the daemon can reach us at
    // all: it runs with PrivateTmp=yes and sees an empty /tmp.
    //
    // No /tmp fallback on purpose — it would silently downgrade to the
    // squattable path in exactly the situation where something is
    // already unusual.
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
    let socket_path = format!("{runtime_dir}/makima-control.sock");
    let _ = std::fs::remove_file(&socket_path);
    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => { eprintln!("deckery: IPC socket bind failed: {}", e); return; }
    };
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let sender = sender_name(&stream);
        let mut reader = tokio::io::BufReader::new(stream);
        let mut line = String::new();
        let read = tokio::time::timeout(CLIENT_TIMEOUT, reader.read_line(&mut line)).await;
        if read.is_err() || read.unwrap().is_err() { continue; }
        let cmd = line.trim().to_string();
        if cmd.is_empty() { continue; }
        match handle(&cmd, &session, &analog, &sender) {
            Handled::Reply(reply) => {
                let mut stream = reader.into_inner();
                let _ = tokio::time::timeout(
                    CLIENT_TIMEOUT, stream.write_all(format!("{reply}\n").as_bytes()),
                ).await;
            }
            Handled::Done => {}
            Handled::Broadcast => { let _ = ipc_tx.send(cmd); }
        }
    }
}

/// What became of one line.
#[derive(Debug, PartialEq, Eq)]
pub enum Handled {
    /// Carried out here; this is the one line to send back.
    Reply(String),
    /// Carried out here; nothing is sent back.
    Done,
    /// For the readers.
    Broadcast,
}

/// Carry out `line` if it belongs here, or say it goes to the readers.
/// Parsed by the same `parse` the readers use, so the grammar is one.
/// `sender` names who sent it, for the journal.
pub fn handle(line: &str, session: &OverrideSession, analog: &AnalogExport, sender: &str) -> Handled {
    match parse_ipc_line(line) {
        Some(IpcCommand::Override(command)) => Handled::Reply(session.handle(&command).to_string()),
        Some(IpcCommand::AnalogStateExport(on)) => {
            analog.set(on, &format!("control socket, {sender}"));
            Handled::Done
        }
        _ => Handled::Broadcast,
    }
}

/// The reply to an override line, `None` for any other line. What the
/// harness drives the override session through.
#[cfg(test)]
pub fn answer(line: &str, session: &OverrideSession) -> Option<String> {
    match handle(line, session, &AnalogExport::default(), "test") {
        Handled::Reply(reply) => Some(reply),
        _ => None,
    }
}

/// The process on the other end of `stream`, as `pid <n> (<comm>)`, or
/// `unknown sender` when the kernel will not say.
fn sender_name(stream: &tokio::net::UnixStream) -> String {
    let Some(pid) = stream.peer_cred().ok().and_then(|c| c.pid()) else {
        return "unknown sender".to_string();
    };
    // The command line rather than `comm`: the HUD and the tour are Python,
    // and "python3" would not tell them apart.
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let cmdline: String = String::from_utf8_lossy(&cmdline)
        .split('\0').filter(|a| !a.is_empty()).collect::<Vec<_>>().join(" ")
        .chars().take(100).collect();
    format!("pid {pid}: {cmdline}")
}
