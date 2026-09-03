//! Compositor adapter abstraction.
//!
//! Each adapter listens for window-focus changes on the compositor's native
//! IPC mechanism and pushes `Client` updates + fires `Notify` so EventReader
//! tasks react immediately without polling.
//!
//! ## Architecture
//!
//! ```text
//! [compositor] → IPC/D-Bus → [adapter] → Arc<Mutex<Client>> + Notify → [EventReader]
//! ```
//!
//! Compositors with an event-driven adapter (KDE, Hyprland):
//!   `EventReader` reads directly from the shared `Arc<Mutex<Client>>`.
//!
//! Compositors without an adapter yet (sway, niri, x11):
//!   The Fallback adapter is a no-op; `EventReader` falls back to calling
//!   `active_client::get_active_window()` on demand (polling).
//!
//! ## Adding a new compositor
//!
//! 1. Add a `mod <name>;` entry below and a new `CompositorKind` variant.
//! 2. Implement `pub async fn run_focus_watcher(active_client, notify)` in the new module.
//!    Call `super::notify_focus_change()` on each focus-change event.
//! 3. Update `detect()` and `CompositorKind::is_event_driven()`.
//! 4. Add the compositor name to `supported_compositors` in `udev_monitor.rs`.

pub mod fallback;
pub mod hyprland;
pub mod kde;

use crate::udev_monitor::Client;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

// ── Shared helper ─────────────────────────────────────────────────────────────

/// Push a focus change into the shared client state and wake all EventReader tasks.
/// All compositor adapters call this on every window-activation event.
pub(crate) async fn notify_focus_change(
    active_client: &Mutex<Client>,
    notify: &Notify,
    class: String,
    title: String,
    pid: Option<u32>,
) {
    *active_client.lock().await = if class.is_empty() {
        Client::Default
    } else {
        Client::Class(class, title, pid)
    };
    notify.notify_waiters();
}

// ── CompositorKind ────────────────────────────────────────────────────────────

/// Which compositor is active. Detected once at startup from environment variables.
/// Controls which focus-watcher adapter is spawned.
#[derive(Debug, Clone, PartialEq)]
pub enum CompositorKind {
    /// KDE Plasma / KWin. Event-driven via KWin scripting D-Bus.
    Kde,
    /// Hyprland. Event-driven via native IPC socket (`.socket2.sock`).
    Hyprland,
    /// Any other compositor or unknown. Per-app bindings fall back to the
    /// legacy `active_client::get_active_window()` polling approach.
    Fallback,
}

impl CompositorKind {
    /// Whether this adapter pushes focus events into `active_client`.
    ///
    /// `EventReader` reads from the `Arc<Mutex<Client>>` directly for
    /// event-driven compositors; for others it calls `get_active_window()`.
    pub fn is_event_driven(&self) -> bool {
        matches!(self, Self::Kde | Self::Hyprland)
    }

    /// Human-readable name used in log output.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Kde => "KDE/KWin",
            Self::Hyprland => "Hyprland",
            Self::Fallback => "fallback (polling)",
        }
    }

    /// Start the focus-watcher task for this compositor. Runs indefinitely.
    /// Meant to be spawned once with `tokio::spawn`.
    pub async fn run_focus_watcher(
        self,
        active_client: Arc<Mutex<Client>>,
        notify: Arc<Notify>,
    ) {
        match self {
            Self::Kde      => kde::run_focus_watcher(active_client, notify).await,
            Self::Hyprland => hyprland::run_focus_watcher(active_client, notify).await,
            Self::Fallback => fallback::run_focus_watcher(active_client, notify).await,
        }
    }
}

// ── Detection ─────────────────────────────────────────────────────────────────

/// Detect the active compositor from the `XDG_CURRENT_DESKTOP` value already
/// resolved by `udev_monitor::set_environment()`.
pub fn detect(desktop: &str) -> CompositorKind {
    match desktop {
        "KDE"      => CompositorKind::Kde,
        "Hyprland" => CompositorKind::Hyprland,
        _          => CompositorKind::Fallback,
    }
}
