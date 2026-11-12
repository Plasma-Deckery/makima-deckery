//! Tests for `config_registry`, one file per area of the registry.

mod enabling;
mod files;
mod lookup;
mod merging;
mod resolve;

use super::*;
use crate::config::{Config, DeviceClass, DeviceDeclaration, Event};
use crate::session::Client;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a registry directly from a list of entries (bypasses the filesystem).
///
/// The user root is a throwaway directory rather than an empty path: a test that
/// calls `set_enabled` persists preferences, and an empty root would resolve
/// relative to the process's working directory — i.e. into the source tree.
fn make_registry(entries: Vec<ConfigEntry>) -> Arc<ConfigRegistry> {
    let map = entries.into_iter().map(|e| (e.name.clone(), e)).collect();
    Arc::new(ConfigRegistry {
        roots: ConfigRoots {
            system: PathBuf::new(),
            user:   std::env::temp_dir().join("deckery-test-registry"),
        },
        entries: Mutex::new(map),
        compositor: Mutex::new(None),
        preferences: Mutex::new(crate::preferences::Preferences::default()),
        resolved: Mutex::new(HashMap::new()),
        generation: std::sync::atomic::AtomicU64::new(0),
    })
}

fn wrap(config: Config, enabled: bool) -> ConfigEntry {
    ConfigEntry {
        name: config.name.clone(),
        config: Some(config),
        enabled,
        errors: vec![],
        from_user: false,
    }
}

/// Same, but read from the user's config directory rather than the shipped one.
fn wrap_user(config: Config, enabled: bool) -> ConfigEntry {
    ConfigEntry { from_user: true, ..wrap(config, enabled) }
}

/// A base config declaring a `[device]` section.
fn base(name: &str, device_names: &[&str]) -> Config {
    let mut c = Config::new_empty(name.to_string());
    c.device = Some(DeviceDeclaration {
        class: DeviceClass::HidSteam,
        names: device_names.iter().map(|s| s.to_string()).collect(),
    });
    c
}

/// A module config: no `[device]`, optionally bound to a window class.
fn module(name: &str, window_class: Option<&str>) -> Config {
    let mut c = Config::new_empty(name.to_string());
    c.module.match_window_class = window_class.map(|s| vec![s.to_string()]);
    c
}

/// Give a config one distinguishable binding so merge results can be asserted.
fn with_binding(mut c: Config, trigger: evdev::Key, output: evdev::Key) -> Config {
    c.bindings.remap
        .entry(Event::Key(trigger))
        .or_default()
        .insert(vec![], vec![output]);
    c
}

fn has_binding(c: &Config, trigger: evdev::Key, output: evdev::Key) -> bool {
    c.bindings.remap
        .get(&Event::Key(trigger))
        .and_then(|m| m.get(&vec![]))
        .is_some_and(|keys| keys == &vec![output])
}

/// A ConfigEntry that failed to parse (config: None).
fn broken_entry(name: &str) -> ConfigEntry {
    ConfigEntry {
        name: name.to_string(),
        config: None,
        enabled: true,
        errors: vec![ConfigError { severity: "error", message: "parse failed".into() }],
        from_user: false,
    }
}

fn class(name: &str) -> Client { Client::Class(name.to_string(), String::new(), None) }

/// A registry whose preferences are written to a real (throwaway) user root, so
/// `set_enabled` can persist without touching the running user's config.
fn registry_with_user_root(entries: Vec<ConfigEntry>, user: &Path) -> Arc<ConfigRegistry> {
    let map = entries.into_iter().map(|e| (e.name.clone(), e)).collect();
    Arc::new(ConfigRegistry {
        roots: ConfigRoots { system: PathBuf::new(), user: user.to_path_buf() },
        entries: Mutex::new(map),
        compositor: Mutex::new(None),
        preferences: Mutex::new(Preferences::default()),
        resolved: Mutex::new(HashMap::new()),
        generation: std::sync::atomic::AtomicU64::new(0),
    })
}

fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Roots where only the system dir exists — the shape of a fresh install.
fn roots_at(system: &Path) -> ConfigRoots {
    ConfigRoots { system: system.to_path_buf(), user: system.join("nonexistent-user-root") }
}

const DECK: &str = r#"
[device]
class = "hid-steam"
names = ["Steam Deck"]
"#;

fn warnings_of(entries: &HashMap<String, ConfigEntry>, name: &str) -> Vec<String> {
    entries[name].errors.iter()
        .filter(|e| e.severity == "warning")
        .map(|e| e.message.clone())
        .collect()
}

fn map_of(configs: Vec<Config>) -> HashMap<String, ConfigEntry> {
    configs.into_iter().map(|c| (c.name.clone(), wrap(c, true))).collect()
}
