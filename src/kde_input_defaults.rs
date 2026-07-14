//! Writes KDE/KWin libinput defaults for Deckery virtual trackpad devices into
//! `~/.config/kcminputrc` before those uinput nodes are created.
//!
//! KWin reads kcminputrc when a new input device appears. If a matching
//! `[Libinput][vendor][product][name]` section already exists it applies those
//! settings; otherwise it uses KDE defaults — which include tap-to-click enabled
//! and flat acceleration, neither of which is right for a Steam Deck pad.
//!
//! `ensure_kde_input_defaults()` is idempotent: it only appends sections that
//! are entirely absent, so existing user customisations in kcminputrc are never
//! touched. Call it once at startup, before `VirtualDevices::enable_trackpads`
//! and `enable_gesture_pad`.

use crate::virtual_devices::{DECKERY_PRODUCT, DECKERY_VENDOR};
use std::fs;
use std::io::Write;

struct PadDefault {
    name: &'static str,
    settings: &'static str,
}

fn pad_defaults() -> [PadDefault; 3] {
    [
        PadDefault {
            name: "Deckery Left Trackpad",
            settings: "DisableWhileTyping=false\nPointerAcceleration=0.200\nPointerAccelerationProfile=2\nTapToClick=false\n",
        },
        PadDefault {
            name: "Deckery Right Trackpad",
            settings: "DisableWhileTyping=false\nEnabled=true\nPointerAcceleration=0.200\nPointerAccelerationProfile=2\nTapToClick=false\n",
        },
        PadDefault {
            name: "Deckery Combined Trackpad",
            settings: "DisableWhileTyping=false\nNaturalScroll=true\nPointerAccelerationProfile=2\nScrollFactor=0.5\nTapToClick=false\n",
        },
    ]
}

fn kcminputrc_path() -> Option<std::path::PathBuf> {
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{}/.config", h)))?;
    Some(std::path::PathBuf::from(config_dir).join("kcminputrc"))
}

/// Ensures sensible libinput defaults for Deckery virtual trackpads are
/// present in kcminputrc. Sections that already exist are left untouched.
pub fn ensure_kde_input_defaults() {
    let path = match kcminputrc_path() {
        Some(p) => p,
        None => {
            eprintln!("[makima] Warning: XDG_CONFIG_HOME and HOME not set; skipping kcminputrc defaults.");
            return;
        }
    };

    let content = fs::read_to_string(&path).unwrap_or_default();

    let mut to_append = String::new();
    for pad in pad_defaults() {
        let header = format!(
            "[Libinput][{}][{}][{}]",
            DECKERY_VENDOR, DECKERY_PRODUCT, pad.name
        );
        if !content.contains(&header) {
            to_append.push('\n');
            to_append.push_str(&header);
            to_append.push('\n');
            to_append.push_str(pad.settings);
        }
    }

    if to_append.is_empty() {
        return;
    }

    let mut file = match fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[makima] Warning: could not open {:?} for writing ({}); libinput defaults not applied.", path, e);
            return;
        }
    };

    if let Err(e) = file.write_all(to_append.as_bytes()) {
        eprintln!("[makima] Warning: failed to write kcminputrc defaults ({}); libinput defaults may not apply.", e);
    }
}
