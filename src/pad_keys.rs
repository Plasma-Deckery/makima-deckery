//! The keys of a `[trackpad.left]`/`[trackpad.right]` table that some pad
//! mode reads.
//!
//! A pad table outlives its mode. Switching `mode` — in a file by hand, or
//! through an override session, which merges `trackpad.right.mode` into the
//! table underneath — leaves the other mode's keys in it: a trackball's
//! `sensitivity` and `haptic.on_roll` next to `mode = "mt-trackpad"`. Those
//! keys are inactive, not typos, and the table is meant to keep them so
//! switching back finds them again. So a handler warns only about a key no
//! mode reads; that is still every typo, which is what the warning is for.
//!
//! Each handler owns its own list; this is only their union, plus what Core
//! reads from every pad table.

/// Keys Core reads from every pad table, or that hold sub-tables.
const CORE_KEYS: &[&str] = &["mode", "click_pressure", "click", "kde", "haptic"];

/// Whether some pad mode reads `key` at the top of a pad table.
pub fn is_pad_key(key: &str) -> bool {
    [CORE_KEYS, crate::mt_trackpad::OWN_KEYS, crate::trackball::OWN_KEYS]
        .iter()
        .any(|keys| keys.contains(&key))
}

/// Whether some pad mode reads `key` in a pad table's `[haptic]`.
pub fn is_haptic_key(key: &str) -> bool {
    [crate::mt_trackpad::HAPTIC_KEYS, crate::trackball::HAPTIC_KEYS]
        .iter()
        .any(|keys| keys.contains(&key))
}

#[cfg(test)]
#[path = "pad_keys_tests.rs"]
mod tests;
