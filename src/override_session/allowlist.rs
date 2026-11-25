//! Which `[commands]` an override may bind.
//!
//! A command binding runs a program, and the control socket is reachable by
//! any process of the user's. So an override may only bind a command line the
//! user's own configs already bind somewhere — the HUD toggle, brightness —
//! and never introduce a new one. The match is on the whole argument vector:
//! `["notify-send", "hello"]` does not allow `["notify-send", "bye"]`.

use crate::config::Config;
use std::collections::HashSet;

/// Every command line bound in `configs`, under any button and combination.
pub fn known_commands<'a>(configs: impl IntoIterator<Item = &'a Config>) -> HashSet<Vec<String>> {
    configs.into_iter()
        .flat_map(|c| c.bindings.commands.values())
        .flat_map(|combos| combos.values())
        .cloned()
        .collect()
}

/// Refuse the first command `own` binds that `known` does not hold.
pub fn check(own: &Config, known: &HashSet<Vec<String>>) -> Result<(), String> {
    let mut commands: Vec<&Vec<String>> = own.bindings.commands.values()
        .flat_map(|combos| combos.values())
        .collect();
    // Sorted so the same document is always refused with the same message.
    commands.sort();
    match commands.into_iter().find(|argv| !known.contains(*argv)) {
        Some(argv) => Err(format!(
            "[commands] {argv:?} is not bound in any loaded config — an override may only \
             bind commands the configs already bind")),
        None => Ok(()),
    }
}

#[cfg(test)]
#[path = "allowlist_tests.rs"]
mod tests;
