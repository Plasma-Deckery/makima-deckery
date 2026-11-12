//! The part of auto-mute that talks to PipeWire and `/proc`: listing streams,
//! working out which game each belongs to, setting mute, and `pactl subscribe`.
//!
//! [`Pactl`] is the production [`Backend`](super::task::Backend); nothing else
//! in this module is called from outside it except the parsers, which the
//! tests cover.

use super::task::{Backend, Subscription};
use super::{Mute, Stream};
use crate::config_registry::ConfigRegistry;
use crate::steam_detector::game_reaper;

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};

/// The real thing: `pactl` for the mixer, the config registry for the setting,
/// the state directory for the remembered names.
pub(super) struct Pactl {
    pub registry: Arc<ConfigRegistry>,
}

impl Backend for Pactl {
    fn enabled(&self) -> bool {
        enabled_in(&self.registry)
    }

    fn subscribe(&mut self) -> std::io::Result<Subscription> {
        let mut child = tokio::process::Command::new("pactl")
            .arg("subscribe")
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("pactl subscribe has no stdout"))?;
        Ok(Subscription {
            lines: (Box::new(BufReader::new(stdout)) as Box<dyn tokio::io::AsyncBufRead + Unpin + Send>).lines(),
            _guard: Box::new(child),
        })
    }

    async fn list_streams(&mut self) -> Vec<Stream> {
        list_streams().await
    }

    async fn apply(&mut self, actions: &[Mute], streams: &[Stream]) {
        apply(actions, streams).await
    }

    fn save_ours(&mut self, ours: &BTreeSet<String>) {
        save_ours(ours)
    }
}

/// Where the remembered names live. Not the runtime directory: the state they
/// describe is WirePlumber's, which survives a reboot, so this has to as well.
fn ours_path() -> PathBuf {
    let base = std::env::var("XDG_STATE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{}/.local/state", crate::config_registry::user_home())));
    base.join("deckery").join("auto-mute-names")
}

pub(super) fn load_ours() -> BTreeSet<String> {
    std::fs::read_to_string(ours_path())
        .map(|t| t.lines().filter(|l| !l.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

fn save_ours(ours: &BTreeSet<String>) {
    let path = ours_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text: String = ours.iter().map(|n| format!("{n}\n")).collect();
    if let Err(e) = std::fs::write(&path, text) {
        eprintln!("deckery: auto_mute: cannot write {}: {e}", path.display());
    }
}

/// Every open stream, from `pactl --format=json list sink-inputs`.
async fn list_streams() -> Vec<Stream> {
    let out = match tokio::process::Command::new("pactl")
        .args(["--format=json", "list", "sink-inputs"])
        .output()
        .await
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    let sandboxes = sandboxed_games();
    parse_streams(&out, |pid, app_id| resolve_game(pid, app_id, &sandboxes))
}

/// Which game a stream belongs to.
///
/// A sandboxed application — every Flatpak, which with EmuDeck means nearly
/// every emulator — reports a PID from *inside* its sandbox. Dolphin's stream
/// says `application.process.id = 13`, which on the host is some unrelated
/// process, so walking up from it can only ever find the wrong thing or
/// nothing. For those the PID is ignored entirely, and the stream is matched by
/// `pipewire.access.portal.app_id` instead — a value PipeWire sets itself and
/// the sandbox cannot forge.
pub(super) fn resolve_game(pid: Option<u32>, app_id: Option<&str>, sandboxes: &HashMap<String, u32>) -> Option<u32> {
    match app_id {
        Some(id) => sandboxes.get(id).copied(),
        None => pid.and_then(game_reaper),
    }
}

/// Every Flatpak sandbox running as part of a Steam game, by app id → reaper.
///
/// Walks `/proc` once: finds each `reaper` whose parent is `steam`, collects
/// everything below it, and reads `.flatpak-info` through `/proc/<pid>/root`
/// for any of those processes that live in a sandbox. Dolphin launched from
/// Steam sits like this:
///
/// ```text
/// reaper → bwrap → bwrap → dolphin-emu-wrapper → dolphin-emu
/// ```
///
/// Reading another process's root needs the same user, which makima has — it
/// runs as the user, like the games.
fn sandboxed_games() -> HashMap<String, u32> {
    let mut parent: HashMap<u32, u32> = HashMap::new();
    let mut name: HashMap<u32, String> = HashMap::new();
    let Ok(dir) = std::fs::read_dir("/proc") else { return HashMap::new() };
    for entry in dir.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if let Some((n, ppid)) = crate::steam_detector::read_proc_status(pid) {
            parent.insert(pid, ppid);
            name.insert(pid, n);
        }
    }

    let is_reaper = |pid: &u32| {
        name.get(pid).map(String::as_str) == Some("reaper")
            && parent.get(pid).and_then(|pp| name.get(pp)).map(String::as_str) == Some("steam")
    };

    let mut games = HashMap::new();
    for (&pid, _) in &parent {
        // Walk up to a reaper; anything not under one is not part of a game.
        let mut cur = pid;
        let reaper = loop {
            if is_reaper(&cur) {
                break Some(cur);
            }
            match parent.get(&cur) {
                Some(&pp) if pp > 1 => cur = pp,
                _ => break None,
            }
        };
        let Some(reaper) = reaper else { continue };
        if let Some(app_id) = flatpak_app_id(pid) {
            games.insert(app_id, reaper);
        }
    }
    games
}

/// The Flatpak app id a process runs as, if it runs in a sandbox.
fn flatpak_app_id(pid: u32) -> Option<String> {
    let info = std::fs::read_to_string(format!("/proc/{pid}/root/.flatpak-info")).ok()?;
    parse_flatpak_info(&info)
}

/// `name=` under `[Application]`. Split out for a test.
pub fn parse_flatpak_info(info: &str) -> Option<String> {
    let mut in_app = false;
    for line in info.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_app = line == "[Application]";
        } else if in_app {
            if let Some(v) = line.strip_prefix("name=") {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// Split out so the JSON shape is covered by a test. `resolve` decides which
/// game a stream belongs to from its reported PID and its sandbox app id.
pub fn parse_streams(
    json: &[u8],
    resolve: impl Fn(Option<u32>, Option<&str>) -> Option<u32>,
) -> Vec<Stream> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_slice(json) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|s| {
            let props = s.get("properties")?;
            let pid = props
                .get("application.process.id")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse().ok());
            let app_id = props.get("pipewire.access.portal.app_id").and_then(|v| v.as_str());
            Some(Stream {
                index: s.get("index")?.as_u64()? as u32,
                game: resolve(pid, app_id),
                app: props.get("application.name")?.as_str()?.to_string(),
                muted: s.get("mute").and_then(|v| v.as_bool()).unwrap_or(false),
            })
        })
        .collect()
}

async fn apply(actions: &[Mute], streams: &[Stream]) {
    for action in actions {
        let (index, flag, verb) = match action {
            Mute::On(i) => (i, "1", "muted"),
            Mute::Off(i) => (i, "0", "unmuted"),
        };
        // One line per change, so a game that starts silent can be traced back
        // to whether Deckery did it.
        let app = streams.iter().find(|s| s.index == *index).map(|s| s.app.as_str()).unwrap_or("?");
        println!("deckery: auto_mute: {verb} {app:?} (stream #{index})");
        let ok = tokio::process::Command::new("pactl")
            .args(["set-sink-input-mute", &index.to_string(), flag])
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("deckery: auto_mute: could not set mute {flag} on stream #{index}");
        }
    }
}

/// `Event 'new' on sink-input #369` → `Some(("new", 369))`.
pub fn parse_event(line: &str) -> Option<(&str, u32)> {
    let rest = line.strip_prefix("Event '")?;
    let (kind, rest) = rest.split_once('\'')?;
    let index = rest.strip_prefix(" on sink-input #")?.trim().parse().ok()?;
    Some((kind, index))
}

fn enabled_in(registry: &ConfigRegistry) -> bool {
    registry
        .base_configs()
        .first()
        .map(|c| c.gaming_mode_config.auto_mute)
        .unwrap_or(false)
}
