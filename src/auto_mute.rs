//! Mute a Steam game while its window is not focused.
//!
//! Switching from a game to the desktop leaves it playing in the background;
//! with `[gaming_mode] auto_mute = true` makima mutes it instead, and unmutes it
//! when its window is focused again. Only the game is touched — its audio
//! streams are found by walking each stream's process up to the game's `reaper`,
//! the same walk `steam_detector` uses to recognise a game at all.
//!
//! Split the way the rest of makima's output now is: [`Muter`] decides and
//! returns what to do, and the task at the bottom of this file is the only
//! thing that talks to PipeWire. The decisions are therefore testable with no
//! audio server at all.
//!
//! ## Why there is a remembered set of names
//!
//! WirePlumber stores the mute state of every stream **per application name**
//! and restores it the next time that application opens a stream (measured: one
//! `pactl set-sink-input-mute` turns `"mute":false` into `"mute":true` in
//! `~/.local/state/wireplumber/stream-properties` immediately). So a game that
//! exits while makima has it muted would start muted next time, with nothing
//! left that knows why.
//!
//! [`Muter::ours`] is therefore not "streams makima muted" but "application
//! names whose stored mute state is makima's doing". It is written to disk, so
//! it survives makima restarting, and the next time such an application opens a
//! stream makima decides afresh whether it should be muted — and releases it if
//! not.

use crate::config_registry::ConfigRegistry;
use crate::steam_detector::game_reaper;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::watch;

/// One audio stream, as far as muting cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    /// PipeWire's sink-input index — what `set-sink-input-mute` takes.
    pub index: u32,
    /// The game this stream belongs to, by its `reaper` PID; `None` for anything
    /// that is not part of a Steam game.
    pub game: Option<u32>,
    /// `application.name` — the key WirePlumber remembers mute state under.
    pub app: String,
    pub muted: bool,
}

/// Something that happened, in the terms the decision needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// The focused window changed. `game` is the reaper of the newly focused
    /// window, or `None` if it is not a game.
    Focus { game: Option<u32> },
    /// A stream appeared.
    StreamNew(Stream),
    /// A stream went away.
    StreamGone { index: u32 },
    /// The setting changed — a config reload can switch the feature on or off
    /// while a game is muted.
    Enabled(bool),
}

/// What to do to the mixer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mute {
    On(u32),
    Off(u32),
}

/// The decision, with the little state it needs.
///
/// A state machine rather than a function: what to do with a stream depends on
/// which game was last focused and on what makima muted earlier.
#[derive(Debug, Default)]
pub struct Muter {
    enabled: bool,
    /// The game last seen focused, by reaper PID. A window that is not a game
    /// does not clear it — leaving the game is exactly the case that matters.
    game: Option<u32>,
    /// Whether that game's window has focus right now.
    focused: bool,
    /// Streams makima muted that are still open, so they can be released.
    muted_here: HashSet<u32>,
    /// Application names whose *stored* mute state makima caused. See the
    /// module docs; persisted between runs.
    ours: BTreeSet<String>,
}

impl Muter {
    pub fn new(enabled: bool, ours: BTreeSet<String>) -> Self {
        Self { enabled, ours, ..Self::default() }
    }

    /// Names to persist after a step.
    pub fn ours(&self) -> &BTreeSet<String> {
        &self.ours
    }

    /// React to one input. `streams` is every stream open right now; only the
    /// focus and enable changes look at it.
    pub fn step(&mut self, input: Input, streams: &[Stream]) -> Vec<Mute> {
        match input {
            Input::Focus { game } => {
                if let Some(game) = game {
                    self.game = Some(game);
                    self.focused = true;
                    return self.release_all(streams);
                }
                self.focused = false;
                self.mute_background(streams)
            }

            Input::StreamNew(s) => {
                let should_be_muted =
                    self.enabled && !self.focused && self.game.is_some() && s.game == self.game;
                match (should_be_muted, s.muted, self.ours.contains(&s.app)) {
                    // A game that is in the background opened another stream.
                    (true, false, _) => vec![self.mute(&s)],
                    // Restored muted from an earlier mute of ours, and still
                    // should be — keep it, but now it is ours to release again.
                    (true, true, true) => {
                        self.muted_here.insert(s.index);
                        Vec::new()
                    }
                    // Restored muted because of us, but there is no reason for it
                    // any more: the game is focused, a different game, or the
                    // feature is off. Release it.
                    (false, true, true) => vec![self.unmute(&s)],
                    // Anything else is not makima's business — in particular a
                    // stream the user muted themselves.
                    _ => Vec::new(),
                }
            }

            Input::StreamGone { index } => {
                // Its stored state stays as it is; `ours` keeps the name so the
                // next stream under it is looked at again.
                self.muted_here.remove(&index);
                Vec::new()
            }

            Input::Enabled(enabled) => {
                self.enabled = enabled;
                if enabled {
                    // A game already in the background is muted now, not on the
                    // next switch.
                    self.mute_background(streams)
                } else {
                    self.release_all(streams)
                }
            }
        }
    }

    /// Mute the remembered game's open streams, if it is not the focused window
    /// and the feature is on. Streams already muted — by the user or by an
    /// earlier call — are left alone.
    fn mute_background(&mut self, streams: &[Stream]) -> Vec<Mute> {
        if !self.enabled || self.focused {
            return Vec::new();
        }
        let Some(game) = self.game else { return Vec::new() };
        streams
            .iter()
            .filter(|s| s.game == Some(game) && !s.muted)
            .map(|s| self.mute(s))
            .collect()
    }

    fn mute(&mut self, s: &Stream) -> Mute {
        self.muted_here.insert(s.index);
        self.ours.insert(s.app.clone());
        Mute::On(s.index)
    }

    fn unmute(&mut self, s: &Stream) -> Mute {
        self.muted_here.remove(&s.index);
        self.ours.remove(&s.app);
        Mute::Off(s.index)
    }

    /// Unmute every open stream makima muted. Only those: a stream the user
    /// muted by hand was never in `muted_here`.
    fn release_all(&mut self, streams: &[Stream]) -> Vec<Mute> {
        let mine: Vec<&Stream> =
            streams.iter().filter(|s| self.muted_here.contains(&s.index)).collect();
        mine.into_iter().map(|s| self.unmute(s)).collect()
    }
}

// ── The part that talks to PipeWire ─────────────────────────────────────────

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

fn load_ours() -> BTreeSet<String> {
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
fn resolve_game(pid: Option<u32>, app_id: Option<&str>, sandboxes: &HashMap<String, u32>) -> Option<u32> {
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

/// Runs for the life of the process.
///
/// Started once rather than per device generation: `launch_tasks` re-runs on
/// every reinit, including every config reload, and a fresh instance would not
/// know which streams the previous one muted. The setting is read from the
/// registry on each event instead, so a reload still takes effect.
///
/// `pactl subscribe` is only started once the feature is on, so a config that
/// never enables it never spawns anything.
pub async fn auto_mute_task(
    registry: Arc<ConfigRegistry>,
    mut game_focus: watch::Receiver<crate::game_focus::GameFocus>,
) {
    let mut muter = Muter::new(enabled_in(&registry), load_ours());
    let mut events: Option<tokio::io::Lines<BufReader<tokio::process::ChildStdout>>> = None;
    let mut _child: Option<tokio::process::Child> = None;

    loop {
        // Start listening for streams as soon as the feature is on.
        if muter.enabled && events.is_none() {
            match tokio::process::Command::new("pactl")
                .arg("subscribe")
                .stdout(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
            {
                Ok(mut child) => {
                    events = child.stdout.take().map(|o| BufReader::new(o).lines());
                    _child = Some(child);
                }
                Err(e) => eprintln!("deckery: auto_mute: cannot start pactl subscribe: {e}"),
            }
        }

        // One wake-up can carry both a changed setting and a changed focus, so
        // this collects inputs rather than choosing one.
        let mut inputs: Vec<Input> = Vec::new();
        tokio::select! {
            changed = game_focus.changed() => {
                if changed.is_err() {
                    return; // the focus task is gone, and with it the process
                }
                // A focus change is also where a reloaded setting is noticed.
                let now = enabled_in(&registry);
                if now != muter.enabled {
                    inputs.push(Input::Enabled(now));
                }
                // The settled focus: a game only counts once it has kept focus
                // for focus_threshold_ms, so sliding past it does not unmute it.
                let game = game_focus.borrow_and_update().game;
                inputs.push(Input::Focus { game });
            }
            line = async { events.as_mut()?.next_line().await.ok().flatten() }, if events.is_some() => {
                let Some(line) = line else {
                    // pactl went away; start it again on the next turn.
                    events = None;
                    _child = None;
                    continue;
                };
                match parse_event(&line) {
                    Some(("new", index)) => inputs.push(Input::StreamNew(Stream {
                        index, game: None, app: String::new(), muted: false,
                    })),
                    Some(("remove", index)) => inputs.push(Input::StreamGone { index }),
                    _ => continue,
                }
            }
        };

        let before = muter.ours().clone();
        let streams = list_streams().await;
        let mut actions = Vec::new();
        for input in inputs {
            // A new stream is only known by its index from the event; the real
            // details come from the listing just taken.
            let input = match input {
                Input::StreamNew(Stream { index, .. }) => {
                    match streams.iter().find(|s| s.index == index) {
                        Some(s) => Input::StreamNew(s.clone()),
                        None => continue, // already gone again
                    }
                }
                other => other,
            };
            actions.extend(muter.step(input, &streams));
        }
        apply(&actions, &streams).await;
        if *muter.ours() != before {
            save_ours(muter.ours());
        }
    }
}

#[cfg(test)]
#[path = "auto_mute_tests.rs"]
mod tests;
