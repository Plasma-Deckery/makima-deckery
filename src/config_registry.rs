// ── Config Registry ───────────────────────────────────────────────────────────
//
// Central, authoritative store for all loaded configuration files.
// Created once at startup; updated in-place on reload (SIGHUP / file-watcher).
// Both udev_monitor and EventReader read from the same Arc<ConfigRegistry>.
//
// Configs are stored UNMERGED — exactly as they appear on disk, with
// associations (device name, window class, layout) parsed from the filename.
// Runtime merging (base + app overrides) happens in resolve() at the point of
// use, so enabling/disabling individual configs always takes effect immediately
// without requiring a reload.
//
// Error handling: files that fail to parse are stored with config: None and a
// descriptive error entry.  They are visible in state.json so the tray can
// highlight broken configs, but they never become the active config and cannot
// be activated even if enabled = true.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use crate::config::{Associations, Config};
use crate::udev_monitor::Client;

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ConfigError {
    pub severity: &'static str,  // "error" | "warning"
    pub message:  String,
}

/// One entry in the registry — one config file on disk.
#[derive(Debug, Clone)]
pub struct ConfigEntry {
    /// The config name (= file base name, e.g. "Steam Deck::Firefox").
    pub name:    String,
    /// Parsed config; None when the file could not be read or parsed.
    pub config:  Option<Config>,
    /// Runtime toggle — can be flipped via IPC without touching the file.
    /// A disabled entry (or one with config: None) is never returned by resolve().
    pub enabled: bool,
    /// Validation errors and warnings collected at load time.
    pub errors:  Vec<ConfigError>,
}

// ── Registry ──────────────────────────────────────────────────────────────────

pub struct ConfigRegistry {
    /// Keyed by config name (= file base name).
    entries: Mutex<HashMap<String, ConfigEntry>>,
}

impl ConfigRegistry {
    // ── Constructors ──────────────────────────────────────────────────────────

    /// Load all `.toml` files from `config_dir`.
    /// Associations are parsed from filenames; content is parsed into Config.
    /// Never panics — parse errors are stored as ConfigEntry with config: None.
    pub fn load(config_dir: &str) -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(Self::load_entries(config_dir)),
        })
    }

    /// Empty registry — used in tests and as a fallback when the config dir
    /// does not exist yet.
    pub fn empty() -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
        })
    }

    // ── Reload ────────────────────────────────────────────────────────────────

    /// Replace all entries with freshly loaded ones.
    /// Called on SIGHUP / file-watcher event — in-place so all Arc holders
    /// (udev_monitor, EventReader) see the update automatically.
    /// Preserves runtime-toggled `enabled` flags: if an entry existed before
    /// and was disabled via IPC, it stays disabled after reload.
    pub fn reload(&self, config_dir: &str) {
        let mut new_entries = Self::load_entries(config_dir);
        let mut entries = self.entries.lock().unwrap();
        for (name, old) in entries.iter() {
            if let Some(new) = new_entries.get_mut(name) {
                new.enabled = old.enabled;
            }
        }
        *entries = new_entries;
    }

    // ── Query API (udev_monitor) ──────────────────────────────────────────────

    /// True if at least one config exists whose device-name prefix matches.
    /// Used by udev_monitor to decide whether to open a given evdev device.
    pub fn device_has_configs(&self, device_name: &str) -> bool {
        self.entries.lock().unwrap()
            .keys()
            .any(|n| n.split("::").next() == Some(device_name))
    }

    /// The unmerged base config for a device — the entry whose name is exactly
    /// the device name (no "::" suffix) and whose associations are default.
    /// Returns None if not found, parse-failed, or disabled.
    pub fn base_config(&self, device_name: &str) -> Option<Config> {
        self.entries.lock().unwrap()
            .get(device_name)
            .filter(|e| e.enabled)
            .and_then(|e| e.config.clone())
    }

    // ── Query API (EventReader) ───────────────────────────────────────────────

    /// Resolve the active, fully-merged config for the given runtime context.
    ///
    /// Steps:
    ///   1. Find the enabled, valid base config for `device_name`.
    ///   2. Find the best matching app config (layout + client).
    ///      Priority: exact (layout + client class) > layout-only (client = Default).
    ///   3. Merge app into base and return.  If no app matches, return base alone.
    ///
    /// Returns None only when no valid base exists for the device.
    pub fn resolve(&self, device_name: &str, client: &Client, layout: u16) -> Option<Config> {
        let entries = self.entries.lock().unwrap();

        // Step 1 — base config.
        let base = entries.get(device_name)
            .filter(|e| e.enabled && e.config.is_some())
            .and_then(|e| e.config.as_ref())?;

        let prefix = format!("{}::", device_name);

        // Step 2a — exact match: layout + client class.
        let exact = entries.values()
            .filter(|e| e.enabled && e.config.is_some())
            .filter_map(|e| e.config.as_ref())
            .filter(|c| c.name.starts_with(&prefix))
            .find(|c| {
                c.associations.layout == layout
                    && clients_match(&c.associations.client, client)
                    && !matches!(c.associations.client, Client::Default)
            });

        // Step 2b — layout-only fallback: any config matching the layout but
        // with Client::Default associations (e.g. "Device::1").
        let layout_only = if exact.is_none() {
            entries.values()
                .filter(|e| e.enabled && e.config.is_some())
                .filter_map(|e| e.config.as_ref())
                .filter(|c| c.name.starts_with(&prefix))
                .find(|c| {
                    c.associations.layout == layout
                        && matches!(c.associations.client, Client::Default)
                        && c.associations != Associations::default() // exclude base itself
                })
        } else {
            None
        };

        let app = exact.or(layout_only);

        // Step 3 — merge.
        Some(match app {
            Some(app_cfg) => app_cfg.merged_with_base(base),
            None          => base.clone(),
        })
    }

    /// All enabled, valid app configs for a device (non-base entries).
    /// Passed to `get_active_window()` so it can validate the window class
    /// against known configs before returning a Client::Class.
    pub fn enabled_app_configs(&self, device_name: &str) -> Vec<Config> {
        let prefix = format!("{}::", device_name);
        self.entries.lock().unwrap()
            .values()
            .filter(|e| e.enabled && e.config.is_some())
            .filter_map(|e| e.config.as_ref())
            .filter(|c| c.name.starts_with(&prefix))
            .cloned()
            .collect()
    }

    // ── IPC API ───────────────────────────────────────────────────────────────

    /// Set the enabled flag for one config entry.
    /// Returns true if the change was applied.
    /// Returns false if the name does not exist, or if `enabled = true` is
    /// requested for an entry that failed to parse (`config: None`) — a
    /// broken config cannot be activated regardless of the flag.
    pub fn set_enabled(&self, name: &str, enabled: bool) -> bool {
        if let Some(entry) = self.entries.lock().unwrap().get_mut(name) {
            if enabled && entry.config.is_none() {
                return false;
            }
            entry.enabled = enabled;
            true
        } else {
            false
        }
    }

    // ── State export ──────────────────────────────────────────────────────────

    /// Snapshot of all entries for state.json serialisation.
    /// Order is unspecified (HashMap); consumers should sort if needed.
    pub fn snapshot(&self) -> Vec<ConfigEntry> {
        self.entries.lock().unwrap()
            .values()
            .cloned()
            .collect()
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn load_entries(config_dir: &str) -> HashMap<String, ConfigEntry> {
        let mut map = HashMap::new();
        let dir = match std::fs::read_dir(config_dir) {
            Ok(d)  => d,
            Err(e) => {
                eprintln!("deckery: config_registry: cannot read {:?}: {}", config_dir, e);
                return map;
            }
        };

        for file in dir.flatten() {
            let filename = file.file_name().into_string().unwrap_or_default();
            if !filename.ends_with(".toml") || filename.starts_with('.') {
                continue;
            }

            let name = filename.trim_end_matches(".toml").to_string();
            let path = file.path();
            let path_str = path.to_str().unwrap_or("");

            let (associations, assoc_errors) = parse_associations(&name);

            let (config_opt, parse_errors) = match Config::try_from_file(path_str, name.clone()) {
                Ok(mut c) => {
                    c.associations = associations;
                    (Some(c), vec![])
                }
                Err(msg) => (None, vec![ConfigError { severity: "error", message: msg }]),
            };

            let mut errors = assoc_errors;
            errors.extend(parse_errors);

            // Log every error/warning to stderr so they appear in the journal
            // even without the tray open.
            for e in &errors {
                eprintln!("deckery: config {:?}: [{}] {}", name, e.severity, e.message);
            }

            // Configs that failed to parse start as disabled — they cannot be
            // used and should not appear as active in the tray.
            let enabled = config_opt.is_some();
            map.insert(name.clone(), ConfigEntry {
                name,
                config: config_opt,
                enabled,
                errors,
            });
        }

        map
    }
}

// ── Free helpers ──────────────────────────────────────────────────────────────

/// Parse associations (device, window class, layout) from a config file name.
///
/// Naming conventions (all prefixed with "DeviceName"):
///   "DeviceName"                     → base, layout 0, all windows
///   "DeviceName::WindowClass"        → layout 0, specific window class
///   "DeviceName::N"                  → layout N, all windows
///   "DeviceName::N::WindowClass"     → layout N, specific window class
///   "DeviceName::WindowClass::N"     → layout N, specific window class (alt order)
fn parse_associations(name: &str) -> (Associations, Vec<ConfigError>) {
    let parts: Vec<&str> = name.split("::").collect();
    let mut warnings = vec![];

    let (client, layout) = match parts.len() {
        1 => (Client::Default, 0),
        2 => {
            if let Ok(n) = parts[1].parse::<u16>() {
                (Client::Default, n)
            } else {
                (Client::Class(parts[1].to_string(), String::new(), None), 0)
            }
        }
        3 => {
            if let Ok(n) = parts[1].parse::<u16>() {
                (Client::Class(parts[2].to_string(), String::new(), None), n)
            } else if let Ok(n) = parts[2].parse::<u16>() {
                (Client::Class(parts[1].to_string(), String::new(), None), n)
            } else {
                warnings.push(ConfigError {
                    severity: "warning",
                    message: format!(
                        "Cannot parse layout number in {:?} — treating as layout 0",
                        name
                    ),
                });
                (Client::Default, 0)
            }
        }
        _ => {
            warnings.push(ConfigError {
                severity: "warning",
                message: format!(
                    "Too many '::' segments in {:?} — treating as layout 0",
                    name
                ),
            });
            (Client::Default, 0)
        }
    };

    (Associations { client, layout }, warnings)
}

/// True if the config's stored client matches the runtime client.
/// Client::Default in the config matches any runtime client (wildcard).
fn clients_match(stored: &Client, runtime: &Client) -> bool {
    match (stored, runtime) {
        (Client::Default, _)                                     => true,
        (Client::Class(a, _, _), Client::Class(b, _, _)) => a == b,
        _                                                        => false,
    }
}

#[cfg(test)]
#[path = "config_registry_tests.rs"]
mod tests;
