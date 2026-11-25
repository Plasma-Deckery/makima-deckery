//! The control socket: one line in per connection, broadcast to the readers —
//! or, for an override command, answered with one line back.
//!
//! Moved out of `main` when the override session needed replies. Every other
//! command is fire-and-forget, as it always was: the auth daemon, the tray and
//! the Gaming Mode toggle write a line and close. An override command is
//! carried out right here, against the one shared session, and the reply goes
//! back on the same connection before it is closed. It is not broadcast: there
//! is one session, not one per reader, and a reply needs one answerer.

use crate::event_loop::{parse_ipc_line, IpcCommand};
use crate::override_session::OverrideSession;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::sync::broadcast;

/// How long a client gets to send its line, and to take the reply.
const CLIENT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// Bind the socket and serve it for the life of the process.
pub async fn serve(ipc_tx: broadcast::Sender<String>, session: Arc<OverrideSession>) {
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
        let mut reader = tokio::io::BufReader::new(stream);
        let mut line = String::new();
        let read = tokio::time::timeout(CLIENT_TIMEOUT, reader.read_line(&mut line)).await;
        if read.is_err() || read.unwrap().is_err() { continue; }
        let cmd = line.trim().to_string();
        if cmd.is_empty() { continue; }
        match answer(&cmd, &session) {
            Some(reply) => {
                let mut stream = reader.into_inner();
                let _ = tokio::time::timeout(
                    CLIENT_TIMEOUT, stream.write_all(format!("{reply}\n").as_bytes()),
                ).await;
            }
            None => { let _ = ipc_tx.send(cmd); }
        }
    }
}

/// The reply to `line` if it is answered here, or `None` if it goes to the
/// readers. Parsed by the same `parse` the readers use, so the grammar is one.
pub fn answer(line: &str, session: &OverrideSession) -> Option<String> {
    match parse_ipc_line(line)? {
        IpcCommand::Override(command) => Some(session.handle(&command).to_string()),
        _ => None,
    }
}
