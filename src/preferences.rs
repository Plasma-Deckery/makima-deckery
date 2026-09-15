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
//     [modules]
//     disabled = ["Voice Control"]
//
// Only deviations from the default are recorded. A module absent from
// `disabled` is enabled, which keeps the file small and makes a newly shipped
// module active without the user having to opt in.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "preferences.toml";

#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Preferences {
    #[serde(default)]
    pub exclusive_groups: HashMap<String, String>,
    #[serde(default)]
    pub modules: ModulePreferences,
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

    pub fn save(&self, user_root: &Path) {
        let path = path_in(user_root);
        if let Err(e) = std::fs::create_dir_all(user_root) {
            eprintln!("deckery: cannot create {user_root:?}: {e}");
            return;
        }
        match toml::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    eprintln!("deckery: cannot write {path:?}: {e}");
                }
            }
            Err(e) => eprintln!("deckery: cannot serialise preferences: {e}"),
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
    pub fn set_group_choice(&mut self, group: &str, name: &str, siblings: &HashSet<String>) {
        self.exclusive_groups.insert(group.to_string(), name.to_string());
        self.modules.disabled.retain(|n| !siblings.contains(n));
    }

    pub fn group_choice(&self, group: &str) -> Option<&str> {
        self.exclusive_groups.get(group).map(String::as_str)
    }
}
