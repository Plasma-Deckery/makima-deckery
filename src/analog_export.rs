//! Whether the 60 Hz analog part of the state export is on.
//!
//! One switch per process, created in `main` and shared by the control socket
//! and every reader. The HUD sends `analog-state-export on|off` only when it
//! changes its mind, so the switch must never be lost or reset behind its back:
//!
//! * It outlives every device generation. A reader that kept its own and
//!   started at off after each rebuild stopped the export until the HUD
//!   toggled again.
//! * It is set by the control socket itself, never broadcast to the readers.
//!   A rebuild has no readers for a moment — the old ones aborted, the new
//!   ones not subscribed yet — and a broadcast in that window reaches nobody.
//!   The onboarding tour rebuilds at every stage change, up to several times
//!   a second, at the very moment the HUD shows or hides its pads.
//!
//! Every change is logged with where it came from, so the journal says who
//! switched it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub struct AnalogExport(Arc<AtomicBool>);

impl AnalogExport {
    pub fn is_on(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Switch it, and log the change and `source`. Setting it to what it
    /// already is says nothing.
    pub fn set(&self, on: bool, source: &str) {
        if self.0.swap(on, Ordering::Relaxed) != on {
            println!("deckery: analog state export {} ({source})", if on { "on" } else { "off" });
        }
    }
}

#[cfg(test)]
#[path = "analog_export_tests.rs"]
mod tests;
