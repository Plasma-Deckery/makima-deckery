//! Fallback compositor adapter — no-op.
//!
//! Used for compositors without an event-driven adapter yet (sway, niri, X11).
//! Per-app bindings for these continue to use the legacy polling approach:
//! `EventReader` calls `active_client::get_active_window()` on demand whenever
//! the active config needs to be checked.
//!
//! This adapter simply parks the task. It never updates `active_client` or
//! fires `notify` — that remains the job of the polling path in EventReader.

use crate::udev_monitor::Client;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

/// No-op focus watcher. Parks the task indefinitely.
pub async fn run_focus_watcher(
    _active_client: Arc<Mutex<Client>>,
    _notify: Arc<Notify>,
) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}
