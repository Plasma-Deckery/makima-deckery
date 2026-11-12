//! Mute a Steam game while its window is not focused.
//!
//! Switching from a game to the desktop leaves it playing in the background;
//! with `[gaming_mode] auto_mute = true` makima mutes it instead, and unmutes it
//! when its window is focused again. Only the game is touched — its audio
//! streams are found by walking each stream's process up to the game's `reaper`,
//! the same walk `steam_detector` uses to recognise a game at all.
//!
//! Split the way the rest of makima's output now is: [`Muter`] decides and
//! returns what to do, [`task`] runs the loop around it, and [`pipewire`] is
//! the only thing that talks to PipeWire. The decisions are therefore testable
//! with no audio server at all, and the loop with a fake one.
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

mod pipewire;
mod restart;
mod task;

use crate::config_registry::ConfigRegistry;

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
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

    /// Whether stepping through `inputs` needs the open streams.
    ///
    /// Off, the decision never acts on a stream — except to release what makima
    /// muted earlier, and to mute a background game the moment it is turned
    /// on. Listing streams spawns `pactl` and walks `/proc`, so a config that
    /// keeps the feature off must not do it on every focus change.
    pub fn needs_streams(&self, inputs: &[Input]) -> bool {
        self.enabled || !self.muted_here.is_empty() || inputs.contains(&Input::Enabled(true))
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

/// Runs for the life of the process.
///
/// Started once rather than per device generation: `launch_tasks` re-runs on
/// every reinit, including every config reload, and a fresh instance would not
/// know which streams the previous one muted. The setting is read from the
/// registry on each event instead, so a reload still takes effect.
///
/// Nothing is spawned while the feature is off — see [`task`].
pub async fn auto_mute_task(
    registry: Arc<ConfigRegistry>,
    game_focus: watch::Receiver<crate::game_focus::GameFocus>,
) {
    task::run(pipewire::Pactl { registry }, pipewire::load_ours(), game_focus).await
}

#[cfg(test)]
#[path = "auto_mute_tests.rs"]
mod tests;
