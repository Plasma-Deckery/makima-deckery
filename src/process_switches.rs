//! The switches the control socket sets, which belong to the process rather
//! than to a device generation.
//!
//! Every rebuild of the device tasks — a config reload, a device change, an
//! override session's trackpad `set` — aborts the readers and starts new ones.
//! Commands used to be broadcast to the readers, and a line sent while the old
//! ones were gone and the new ones not yet listening reached nobody. The
//! onboarding tour rebuilds up to several times a second, so that window was
//! hit. And a reader's own `paused` started at false, so every rebuild while
//! the HUD was open quietly switched remapping back on.
//!
//! So the control socket now carries every command out itself, on these:
//!
//! * `analog` — the analog export (`crate::analog_export`);
//! * `pause` — whether remapping is paused. A watch: a reader takes the value
//!   when it is built, so one started while paused starts paused, and reacts
//!   to every change after that with what a pause does to the input path
//!   (issue #62: outputs released, what the user holds kept);
//! * `gaming` — Gaming Mode requests, queued until a reader takes them. One
//!   reader of each generation consumes the queue, so a request sent during a
//!   rebuild is applied by the next one, with its haptic and announcement.
//!
//! Every change is logged with where it came from.

use crate::analog_export::AnalogExport;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};

#[derive(Clone)]
pub struct Switches {
    pub analog: AnalogExport,
    pub pause: Pause,
    pub gaming: GamingRequests,
}

impl Default for Switches {
    fn default() -> Self {
        Self { analog: AnalogExport::default(), pause: Pause::default(), gaming: GamingRequests::new() }
    }
}

/// Whether remapping is paused.
#[derive(Clone)]
pub struct Pause(Arc<watch::Sender<bool>>);

impl Default for Pause {
    fn default() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }
}

impl Pause {
    #[cfg(test)]
    pub fn is_paused(&self) -> bool {
        *self.0.borrow()
    }

    /// Pause or resume, and log a change with `source`. Every reader is told
    /// through `subscribe`; one that is not running yet reads it when built.
    pub fn set(&self, paused: bool, source: &str) {
        let changed = self.0.send_if_modified(|p| std::mem::replace(p, paused) != paused);
        if changed {
            println!("deckery: remapping {} ({source})", if paused { "paused" } else { "resumed" });
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.0.subscribe()
    }
}

/// Gaming Mode requests from the control socket, waiting for a reader.
#[derive(Clone)]
pub struct GamingRequests {
    tx: mpsc::Sender<bool>,
    rx: Arc<Mutex<mpsc::Receiver<bool>>>,
}

impl GamingRequests {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel(32);
        Self { tx, rx: Arc::new(Mutex::new(rx)) }
    }

    /// Ask for Gaming Mode on or off. Logged here; whether it changes
    /// anything — it is refused during an override session, and a request
    /// for what is already the case does nothing — the reader decides.
    pub fn request(&self, on: bool, source: &str) {
        println!("deckery: Gaming Mode {} requested ({source})", if on { "on" } else { "off" });
        if self.tx.try_send(on).is_err() {
            eprintln!("deckery: Gaming Mode request dropped — {} are already waiting", 32);
        }
    }

    /// The queue, for the one reader of a generation that applies it.
    pub fn queue(&self) -> Arc<Mutex<mpsc::Receiver<bool>>> {
        self.rx.clone()
    }
}

#[cfg(test)]
#[path = "process_switches_tests.rs"]
mod tests;
