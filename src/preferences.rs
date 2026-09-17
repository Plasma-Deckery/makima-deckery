// ── Preferences ───────────────────────────────────────────────────────────────
//
// Which modules the user has switched on or off, and which member of each
// exclusive group they picked. This is runtime state, not configuration: the
// module `.toml` files describe what a module *does*, never whether it is
// currently active. Keeping the two apart is what allows the system config root
// to stay read-only while the user's choices still survive a restart.
//
// Lives at `<user config root>/preferences.toml`:
//
//     [exclusive_groups]
//     kde-desktop-layout = "KDE Desktop Layout Horizontal"
//
//     [groups]
//     disabled = ["voice-control-language"]
//
//     [modules]
//     disabled = ["Voice Control"]
//
// Only deviations from the default are recorded. A module absent from
// `disabled` is enabled, which keeps the file small and makes a newly shipped
// module active without the user having to opt in.
//
// A group listed in `groups.disabled` has no active member at all. Its entry in
// `exclusive_groups` is kept while it is off: that is the choice to restore when
// it is switched back on, and dropping it would silently reset the user to the
// alphabetically first member.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "preferences.toml";

#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Preferences {
    #[serde(default)]
    pub exclusive_groups: HashMap<String, String>,
    #[serde(default)]
    pub groups: GroupPreferences,
    #[serde(default)]
    pub modules: ModulePreferences,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct GroupPreferences {
    #[serde(default)]
    pub disabled: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ModulePreferences {
    #[serde(default)]
    pub disabled: Vec<String>,
}

/// Where preferences live for a given user config root.
///
/// Public because `load_entries` has to skip this file: it sits in the same
/// directory as the configs and would otherwise be discovered as a module.
pub fn path_in(user_root: &Path) -> PathBuf {
    user_root.join(FILE_NAME)
}

pub fn is_preferences_file(file_name: &str) -> bool {
    file_name == FILE_NAME
}

/// Copy the shipped preferences template into the user root, once.
///
/// The shipped configs carry a `preferences.toml` holding the exclusive-group
/// defaults. It has to reach the user directory because a group falls back to
/// its alphabetically first member otherwise, and alphabetical order cannot know
/// which member is the intended default.
///
/// Doing it here rather than in `install.sh` is what makes it install-method
/// independent: an RPM never runs that script, and its `%post` runs as root
/// without knowing whose home directory to write to. The first start of the user
/// service knows both.
///
/// Returns without doing anything if the user already has a copy — from that
/// point the file is theirs, and an update must not reset their choices.
pub fn seed_from(system_root: &Path, user_root: &Path) {
    let target = path_in(user_root);
    if target.exists() {
        return;
    }
    let source = path_in(system_root);
    if !source.exists() {
        // Running against a config tree that predates the template, or a bare
        // test root. The group fallback still yields a usable session.
        return;
    }
    if let Err(e) = std::fs::create_dir_all(user_root) {
        eprintln!("deckery: cannot create {user_root:?}: {e}");
        return;
    }
    match std::fs::copy(&source, &target) {
        Ok(_) => eprintln!("deckery: seeded {target:?} from {source:?}"),
        Err(e) => eprintln!("deckery: cannot seed {target:?}: {e}"),
    }
}

impl Preferences {
    /// Read the file, falling back to defaults when it is missing or broken.
    ///
    /// A malformed file must not take the whole input stack down with it, so it
    /// is reported and ignored. The next write replaces it with something valid.
    pub fn load(user_root: &Path) -> Self {
        let path = path_in(user_root);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str(&text) {
            Ok(prefs) => prefs,
            Err(e) => {
                eprintln!("deckery: ignoring malformed {path:?}: {e}");
                Self::default()
            }
        }
    }

    /// Write the file, replacing it atomically.
    ///
    /// Written to a sibling temporary file and renamed into place, the same way
    /// state.json is written. A plain write truncates first, so a crash in that
    /// window would not cost the last toggle but every recorded one — and this is
    /// the one file the user cannot reconstruct from what is on disk.
    pub fn save(&self, user_root: &Path) {
        let path = path_in(user_root);
        if let Err(e) = std::fs::create_dir_all(user_root) {
            eprintln!("deckery: cannot create {user_root:?}: {e}");
            return;
        }
        let text = match toml::to_string_pretty(self) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("deckery: cannot serialise preferences: {e}");
                return;
            }
        };
        // Same directory as the target: rename is only atomic within a filesystem.
        let tmp = path.with_extension("toml.tmp");
        if let Err(e) = std::fs::write(&tmp, text) {
            eprintln!("deckery: cannot write {tmp:?}: {e}");
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, &path) {
            eprintln!("deckery: cannot replace {path:?}: {e}");
            let _ = std::fs::remove_file(&tmp);
        }
    }

    pub fn is_disabled(&self, name: &str) -> bool {
        self.modules.disabled.iter().any(|n| n == name)
    }

    pub fn set_disabled(&mut self, name: &str, disabled: bool) {
        self.modules.disabled.retain(|n| n != name);
        if disabled {
            self.modules.disabled.push(name.to_string());
        }
        self.modules.disabled.sort();
    }

    /// Record the winner of an exclusive group. Siblings are not listed as
    /// disabled — the group entry already says everything, and duplicating it
    /// would let the two halves of the file disagree.
    ///
    /// Picking a member also switches the group on: there is no reading of
    /// "activate this one" that leaves the group off.
    pub fn set_group_choice(&mut self, group: &str, name: &str, siblings: &HashSet<String>) {
        self.exclusive_groups.insert(group.to_string(), name.to_string());
        self.modules.disabled.retain(|n| !siblings.contains(n));
        self.set_group_disabled(group, false);
    }

    pub fn group_choice(&self, group: &str) -> Option<&str> {
        self.exclusive_groups.get(group).map(String::as_str)
    }

    pub fn is_group_disabled(&self, group: &str) -> bool {
        self.groups.disabled.iter().any(|g| g == group)
    }

    /// Switch a whole group off, or back on.
    ///
    /// The recorded member choice is deliberately left alone: switching a group
    /// off is not the same as forgetting which member was picked, and keeping it
    /// is what lets switching back on land where the user left it.
    pub fn set_group_disabled(&mut self, group: &str, disabled: bool) {
        self.groups.disabled.retain(|g| g != group);
        if disabled {
            self.groups.disabled.push(group.to_string());
        }
        self.groups.disabled.sort();
    }
}
