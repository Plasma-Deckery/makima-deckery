//! The loop that feeds [`Muter`] and carries out what it decides.
//!
//! Everything it touches outside itself — the setting, the stream listing,
//! the mixer, `pactl subscribe`, the remembered names on disk — goes through
//! [`Backend`], so a test can run the loop and count what it would have
//! spawned. [`Pactl`](super::pipewire::Pactl) is the real one.
//!
//! While the feature is off nothing is spawned at all: no `pactl subscribe`,
//! no listing on focus changes, no `/proc` walk. The only exception is turning
//! it off, which lists once to release what makima muted.

use super::restart::Restart;
use super::{Input, Mute, Muter, Stream};
use crate::game_focus::GameFocus;

use std::collections::BTreeSet;
use std::future::Future;
use tokio::io::{AsyncBufRead, Lines};
use tokio::sync::watch;
use tokio::time::Instant;

/// A running stream subscription: its event lines, and whatever has to stay
/// alive for them to keep coming (the `pactl` child, killed on drop).
pub struct Subscription {
    pub lines: Lines<Box<dyn AsyncBufRead + Unpin + Send>>,
    pub _guard: Box<dyn Send>,
}

/// Everything the loop does to the world.
pub trait Backend {
    /// Whether `[gaming_mode] auto_mute` is on right now.
    fn enabled(&self) -> bool;
    /// Start listening for streams appearing and going away.
    fn subscribe(&mut self) -> std::io::Result<Subscription>;
    /// Every open stream.
    fn list_streams(&mut self) -> impl Future<Output = Vec<Stream>> + Send;
    /// Carry out mute changes. Never called with an empty list.
    fn apply(&mut self, actions: &[Mute], streams: &[Stream]) -> impl Future<Output = ()> + Send;
    /// Persist the remembered names.
    fn save_ours(&mut self, ours: &BTreeSet<String>);
}

/// Runs until the focus channel closes.
pub(super) async fn run(
    mut backend: impl Backend,
    ours: BTreeSet<String>,
    mut game_focus: watch::Receiver<GameFocus>,
) {
    let mut muter = Muter::new(backend.enabled(), ours);
    let mut subscription: Option<Subscription> = None;
    let mut restart = Restart::default();

    loop {
        if !muter.enabled {
            // Off: stop listening, and forget any failure streak so turning it
            // on again starts at once.
            subscription = None;
            restart = Restart::default();
        } else if subscription.is_none() && restart.due(Instant::now()) {
            match backend.subscribe() {
                Ok(s) => {
                    subscription = Some(s);
                    restart.started(Instant::now());
                }
                Err(e) => {
                    let wait = restart.failed(Instant::now());
                    eprintln!("deckery: auto_mute: cannot start pactl subscribe: {e}; retrying in {wait:?}");
                }
            }
        }
        let retry_at = restart.not_before().filter(|_| muter.enabled && subscription.is_none());

        // One wake-up can carry both a changed setting and a changed focus, so
        // this collects inputs rather than choosing one.
        let mut inputs: Vec<Input> = Vec::new();
        tokio::select! {
            changed = game_focus.changed() => {
                if changed.is_err() {
                    return; // the focus task is gone, and with it the process
                }
                // A focus change is also where a reloaded setting is noticed.
                let now = backend.enabled();
                if now != muter.enabled {
                    inputs.push(Input::Enabled(now));
                }
                // The settled focus: a game only counts once it has kept focus
                // for focus_threshold_ms, so sliding past it does not unmute it.
                let game = game_focus.borrow_and_update().game;
                inputs.push(Input::Focus { game });
            }
            line = async { subscription.as_mut()?.lines.next_line().await.ok().flatten() }, if subscription.is_some() => {
                let Some(line) = line else {
                    // pactl went away; start it again once the wait is over.
                    subscription = None;
                    let wait = restart.failed(Instant::now());
                    eprintln!("deckery: auto_mute: pactl subscribe ended; restarting in {wait:?}");
                    continue;
                };
                match super::pipewire::parse_event(&line) {
                    Some(("new", index)) => inputs.push(Input::StreamNew(Stream {
                        index, game: None, app: String::new(), muted: false,
                    })),
                    Some(("remove", index)) => inputs.push(Input::StreamGone { index }),
                    _ => continue,
                }
            }
            () = tokio::time::sleep_until(retry_at.unwrap_or_else(Instant::now)), if retry_at.is_some() => {
                continue;
            }
        };

        let before = muter.ours().clone();
        let streams =
            if muter.needs_streams(&inputs) { backend.list_streams().await } else { Vec::new() };
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
        if !actions.is_empty() {
            backend.apply(&actions, &streams).await;
        }
        if *muter.ours() != before {
            backend.save_ours(muter.ours());
        }
    }
}

#[cfg(test)]
#[path = "task_tests.rs"]
mod tests;
