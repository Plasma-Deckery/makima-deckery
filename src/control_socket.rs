//! The control socket: one line in per connection, carried out right here.
//!
//! Moved out of `main` when the override session needed replies. Every command
//! but those is fire-and-forget, as it always was: the auth daemon, the tray,
//! the HUD and the Gaming Mode toggle write a line and close. An override
//! command is answered with one line on the same connection before it closes.
//!
//! Nothing is broadcast to the readers any more. Every command acts on
//! something that belongs to the process — the override session, the registry,
//! the switches in `crate::process_switches` — so a rebuild of the device tasks,
//! which has no readers for a moment, cannot swallow it. A reader learns of a
//! change through those switches, including one made while it did not exist.

use crate::config_registry::ConfigRegistry;
use crate::event_loop::{parse_ipc_line, IpcCommand};
use crate::override_session::OverrideSession;
use crate::process_switches::Switches;
use crate::state_writer::{StateCommand, StateWriterHandle};
use std::sync::Arc;
use tokio::net::UnixListener;

/// How long a client gets to send its line, and to take the reply.
const CLIENT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// Everything a command acts on.
#[derive(Clone)]
pub struct Control {
    pub session: Arc<OverrideSession>,
    pub switches: Switches,
    pub registry: Arc<ConfigRegistry>,
    pub state_tx: StateWriterHandle,
}

/// Bind the socket and serve it for the life of the process.
pub async fn serve(control: Control) {
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
        if let Some(reply) = control.handle(&cmd, &sender).await {
            let mut stream = reader.into_inner();
            let _ = tokio::time::timeout(
                CLIENT_TIMEOUT, stream.write_all(format!("{reply}\n").as_bytes()),
            ).await;
        }
    }
}

impl Control {
    /// Carry out one line, and say what to answer if it is answered.
    /// Parsed by `event_loop::parse`, which keeps the grammar in one place.
    /// `sender` names who sent it, for the journal. An unknown line is
    /// ignored without a word, as it always was.
    pub async fn handle(&self, line: &str, sender: &str) -> Option<String> {
        let source = format!("control socket, {sender}");
        match parse_ipc_line(line)? {
            IpcCommand::Override(command) => return Some(self.session.handle(&command).to_string()),
            IpcCommand::AnalogStateExport(on) => self.switches.analog.set(on, &source),
            IpcCommand::Pause => self.switches.pause.set(true, &source),
            IpcCommand::Resume => self.switches.pause.set(false, &source),
            IpcCommand::GamingMode(on) => self.switches.gaming.request(on, &source),
            IpcCommand::Config { enabling, name } => {
                if !self.registry.set_enabled(&name, enabling) {
                    eprintln!("deckery: IPC config enable/disable: no config named {name:?} ({source})");
                    return None;
                }
                eprintln!("deckery: IPC config {}: {name:?} ({source})",
                    if enabling { "enable" } else { "disable" });
                let _ = self.state_tx.send(StateCommand::SetLoadedConfigs(self.registry.snapshot())).await;
            }
            IpcCommand::ConfigGroup { enabling, name } => {
                if !self.registry.set_group_enabled(&name, enabling) {
                    eprintln!("deckery: IPC config group enable/disable: no group named {name:?} ({source})");
                    return None;
                }
                eprintln!("deckery: IPC config group {}: {name:?} ({source})",
                    if enabling { "enable" } else { "disable" });
                let _ = self.state_tx.send(StateCommand::SetLoadedConfigs(self.registry.snapshot())).await;
            }
        }
        None
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
