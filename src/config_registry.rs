// ── Config Registry ───────────────────────────────────────────────────────────
//
// Central, authoritative store for all loaded configuration files.
// Created once at startup; updated in-place on reload (SIGHUP / file-watcher).
// Both udev_monitor and EventReader read from the same Arc<ConfigRegistry>.
//
// Configs are stored UNMERGED — exactly as they appear on disk. A config
// declares what it is through its *content*, never through its filename:
//
//   [device]  → base config; names the physical device it drives
//   [module]  → activation conditions (window class, layout, compositor)
//   [modules] → include list of plain modules merged into a base config
//
// Runtime merging happens in resolve() at the point of use, so enabling or
// disabling individual configs always takes effect immediately without a reload.
//
// Error handling: files that fail to parse are stored with config: None and a
// descriptive error entry.  They are visible in state.json so the tray can
// highlight broken configs, but they never become the active config and cannot
// be activated even if enabled = true.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use crate::config::Config;
use crate::udev_monitor::Client;

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigError {
    pub severity: &'static str,  // "error" | "warning"
    pub message:  String,
}

/// One entry in the registry — one config file on disk.
#[derive(Debug, Clone)]
pub struct ConfigEntry {
    /// The config name (= file base name, e.g. "Steam Deck" or "konsole").
    pub name:    String,
    /// Parsed config; None when the file could not be read or parsed.
    pub config:  Option<Config>,
    /// Runtime toggle — can be flipped via IPC without touching the file.
    /// A disabled entry (or one with config: None) is never returned by resolve().
    pub enabled: bool,
    /// Validation errors and warnings collected at load time.
    pub errors:  Vec<ConfigError>,
}

/// One config as the tray displays it: what it is and where it belongs.
/// Derived from the parsed content, never from the file's location on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigSummary {
    pub name:    String,
    /// "base" (declares a device), "app" (matches a window class), "module"
    /// (neither — only reachable through an include), or "unknown" (unparsed).
    pub kind:    &'static str,
    /// For modules: the config whose `[modules] include` pulls this one in.
    pub parent:  Option<String>,
    pub enabled: bool,
    pub errors:  Vec<ConfigError>,
}

fn entry_kind(entry: &ConfigEntry) -> &'static str {
    match &entry.config {
        None => "unknown",
        Some(c) if c.device.is_some()                    => "base",
        Some(c) if c.module.match_window_class.is_some() => "app",
        Some(_)                                          => "module",
    }
}

/// The config that includes `name`, if any. A module included by more than one
/// base reports the first match; nesting a module under two bases at once is
/// not something the tray can draw, and not something any config does today.
fn parent_of(entries: &HashMap<String, ConfigEntry>, name: &str) -> Option<String> {
    entries.values()
        .filter_map(|e| e.config.as_ref())
        .find(|c| c.module_includes.iter().any(|i| i == name))
        .map(|c| c.name.clone())
}

// ── Registry ──────────────────────────────────────────────────────────────────

pub struct ConfigRegistry {
    /// Keyed by config name (= file base name).
    entries: Mutex<HashMap<String, ConfigEntry>>,
    /// Name of the running compositor, as reported by XDG_CURRENT_DESKTOP
    /// (or "x11"). Gates modules that declare `[module] requires_compositor`.
    /// Set once by `set_compositor()` after the session environment is resolved;
    /// until then no compositor-specific module is considered usable.
    compositor: Mutex<Option<String>>,
}

impl ConfigRegistry {
    // ── Constructors ──────────────────────────────────────────────────────────

    /// Load all `.toml` files from `config_dir` and its `apps/` subdirectory.
    /// Never panics — parse errors are stored as ConfigEntry with config: None.
    pub fn load(config_dir: &str) -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(Self::load_entries(config_dir)),
            compositor: Mutex::new(None),
        })
    }

    /// Empty registry.
    #[cfg(test)]
    pub(crate) fn empty() -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
            compositor: Mutex::new(None),
        })
    }

    /// Build a registry directly from a list of entries — for use in tests
    /// outside the `config_registry` module where `entries` is private.
    #[cfg(test)]
    pub(crate) fn from_entries(entries: Vec<ConfigEntry>) -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(entries.into_iter().map(|e| (e.name.clone(), e)).collect()),
            compositor: Mutex::new(None),
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
                // Only carry over the runtime-toggled enabled flag when both
                // the old AND new entry are valid (config: Some).
                // - old broken → new fixed:   keep new default (enabled=true)
                // - old valid  → new broken:  keep new default (enabled=false)
                // - old valid  → new valid:   preserve user's IPC toggle
                if old.config.is_some() && new.config.is_some() {
                    new.enabled = old.enabled;
                }
            }
        }
        *entries = new_entries;
    }

    // ── Environment ───────────────────────────────────────────────────────────

    /// Record which compositor the session is running under. Modules declaring
    /// a different `[module] requires_compositor` are excluded from every query
    /// from this point on. Called once, after the environment is resolved.
    pub fn set_compositor(&self, name: Option<String>) {
        *self.compositor.lock().unwrap() = name;
    }

    // ── Query API (udev_monitor) ─────────────────────────────────────────────

    /// True if any usable base config's `[device]` declaration matches this evdev name.
    pub fn any_device_matches(&self, evdev_name: &str) -> bool {
        let compositor = self.compositor();
        self.entries.lock().unwrap()
            .values()
            .filter_map(|e| usable(e, compositor.as_deref()))
            .filter_map(|c| c.device.as_ref())
            .any(|d| d.matches_evdev_name(evdev_name))
    }

    /// All usable base configs (those with a `[device]` section), each already
    /// merged with the plain modules it pulls in via `[modules] include`.
    /// `launch_tasks()` iterates these declared targets to find physical devices.
    pub fn base_configs(&self) -> Vec<Config> {
        let compositor = self.compositor();
        let entries = self.entries.lock().unwrap();
        entries.values()
            .filter_map(|e| usable(e, compositor.as_deref()))
            .filter(|c| c.device.is_some())
            .map(|c| with_includes(&entries, c, compositor.as_deref()))
            .collect()
    }

    /// All usable modules that declare a `match_window_class`. `active_client`
    /// uses this to decide whether a focused window is one we have a module for.
    pub fn window_class_modules(&self) -> Vec<Config> {
        let compositor = self.compositor();
        self.entries.lock().unwrap()
            .values()
            .filter_map(|e| usable(e, compositor.as_deref()))
            .filter(|c| c.module.match_window_class.is_some())
            .cloned()
            .collect()
    }

    // ── Query API (EventReader) ───────────────────────────────────────────────

    /// Resolve the active, fully-merged config for the given runtime context.
    ///
    /// `base_name` identifies the base config by name — the one `launch_tasks()`
    /// already bound to a physical device. Device matching happens once, at
    /// discovery; re-deriving it here would make the outcome depend on whether
    /// the filename happens to resemble the `[device] names` entries.
    ///
    /// Merge order, lowest priority first:
    ///   1. plain modules named in the base config's `[modules] include`
    ///   2. the base config itself
    ///   3. the conditional module matching the current window class and layout
    ///
    /// Returns None when `base_name` names no usable base config, or when a
    /// layout other than 0 is requested and no module covers it — the latter is
    /// what lets `change_active_layout()` skip unpopulated layout slots.
    pub fn resolve(&self, base_name: &str, client: &Client, layout: u16) -> Option<Config> {
        let compositor = self.compositor();
        let compositor = compositor.as_deref();
        let entries = self.entries.lock().unwrap();

        let base_raw = usable(entries.get(base_name)?, compositor)
            .filter(|c| c.device.is_some())?;
        let base = with_includes(&entries, base_raw, compositor);

        let client_class = match client {
            Client::Class(class, _, _) => Some(class.as_str()),
            Client::Default => None,
        };

        // Most specific first: a module bound to both window class and layout
        // beats one bound to the layout alone.
        let candidates = || entries.values()
            .filter_map(|e| usable(e, compositor))
            .filter(|c| c.module.layout == layout);

        let by_class = client_class.and_then(|class| {
            candidates().find(|c| c.module.match_window_class.as_deref() == Some(class))
        });
        let by_layout = candidates()
            .find(|c| c.module.match_window_class.is_none() && c.module.layout != 0);

        let mut resolved = match by_class.or(by_layout) {
            Some(module) => module.merged_with_base(&base),
            // Layout 0 is always valid — it is the base config with no module.
            None if layout == 0 => base,
            None => return None,
        };

        // Hints resolve here and nowhere else: this is the single funnel every
        // merged config passes through, so an output-space hint sees the final
        // remap table including module and app overrides. Warnings are dropped
        // on the hot path — they are reported once at load by `hint_warnings()`.
        let _ = resolved.resolve_hints();
        Some(resolved)
    }

    /// Hint problems across all loaded configs, resolved against each base.
    /// Called once after loading so dead or ambiguous hints are reported in the
    /// journal instead of silently never appearing in the HUD.
    pub fn hint_warnings(&self) -> Vec<String> {
        let compositor = self.compositor();
        let compositor = compositor.as_deref();
        let entries = self.entries.lock().unwrap();
        let mut out = Vec::new();
        for base_entry in entries.values() {
            let Some(base_raw) = usable(base_entry, compositor).filter(|c| c.device.is_some())
            else { continue };
            let base = with_includes(&entries, base_raw, compositor);
            // The base alone, plus each module merged onto it — an app override
            // can move a key, so a hint may be fine in one stack and dead in another.
            let mut stacks: Vec<Config> = vec![base.clone()];
            stacks.extend(
                entries.values()
                    .filter_map(|e| usable(e, compositor))
                    .filter(|c| c.device.is_none())
                    .map(|m| m.merged_with_base(&base)),
            );
            for mut stack in stacks {
                let name = stack.name.clone();
                for warning in stack.resolve_hints() {
                    out.push(format!("{name}: {warning}"));
                }
            }
        }
        out.sort();
        out.dedup();
        out
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
    ///
    /// Modules gated behind a compositor this session does not run are left out:
    /// they can never contribute a binding here, and a desktop environment is
    /// not something one switches between mid-session.
    pub fn snapshot(&self) -> Vec<ConfigSummary> {
        let entries = self.entries.lock().unwrap();
        let compositor = self.compositor.lock().unwrap().clone();
        entries.values()
            .filter(|e| match &e.config {
                Some(c) => match c.module.requires_compositor.as_deref() {
                    Some(required) => compositor.as_deref() == Some(required),
                    None => true,
                },
                // Parsing failed, so we cannot know whether a gate applies.
                // Showing it is the point — its error is what needs fixing.
                None => true,
            })
            .map(|e| ConfigSummary {
                name:    e.name.clone(),
                kind:    entry_kind(e),
                parent:  parent_of(&entries, &e.name),
                enabled: e.enabled,
                errors:  e.errors.clone(),
            })
            .collect()
    }

    /// Returns the first error message for a config that could not be parsed at
    /// all, or None if every file on disk parsed cleanly.
    ///
    /// A file that fails to parse may well be the base config, in which case the
    /// entire stack for that device is dead — but since parsing failed we cannot
    /// know whether it declared `[device]`. Callers use this to send a
    /// `StateCommand::SetError { id: "base_config", … }` so the tray shows a red
    /// icon rather than just a per-config error marker.
    pub fn base_config_error(&self) -> Option<String> {
        self.entries.lock().unwrap()
            .values()
            .filter(|e| e.config.is_none())
            .flat_map(|e| e.errors.iter())
            .find(|err| err.severity == "error")
            .map(|err| err.message.clone())
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn compositor(&self) -> Option<String> {
        self.compositor.lock().unwrap().clone()
    }

    fn load_entries(config_dir: &str) -> HashMap<String, ConfigEntry> {
        let mut map = HashMap::new();
        // Scan both the root config dir and the `apps/` subdirectory.
        let dirs_to_scan: Vec<std::path::PathBuf> = {
            let root = std::path::PathBuf::from(config_dir);
            let apps = root.join("apps");
            let mut v = vec![root];
            if apps.is_dir() { v.push(apps); }
            v
        };

        // Aliases are declared on the base config's [device] block but apply to
        // every file, so they must be known before the first file is parsed:
        // parse_event_name resolves names the moment a binding is read.
        let aliases = collect_aliases(config_dir);

        for scan_dir in dirs_to_scan {
            let dir = match std::fs::read_dir(&scan_dir) {
                Ok(d)  => d,
                Err(e) => {
                    eprintln!("deckery: config_registry: cannot read {:?}: {}", scan_dir, e);
                    continue;
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

                let (config_opt, errors) = match Config::try_from_file(path_str, name.clone(), &aliases) {
                    Ok(c)    => (Some(c), vec![]),
                    Err(msg) => (None, vec![ConfigError { severity: "error", message: msg }]),
                };

                for e in &errors {
                    eprintln!("deckery: config {:?}: [{}] {}", name, e.severity, e.message);
                }

                let enabled = config_opt.is_some();
                map.insert(name.clone(), ConfigEntry {
                    name,
                    config: config_opt,
                    enabled,
                    errors,
                });
            }
        }

        // Inclusion is only knowable once every file has been read.
        for name in orphan_configs(&map) {
            eprintln!(
                "deckery: config {:?} is never applied — it declares no [device], \
                 no [module] match_window_class or layout, and no config includes \
                 it via [modules] include",
                name
            );
        }

        map
    }
}

// ── Free helpers ──────────────────────────────────────────────────────────────

/// Button aliases from every base config in the directory root. Modules and app
/// overrides live in `apps/` or carry no `[device]` block, so they contribute
/// nothing — the names belong to the hardware, and only a base config names it.
fn collect_aliases(config_dir: &str) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    let dir = match std::fs::read_dir(config_dir) {
        Ok(d) => d,
        Err(_) => return aliases,
    };
    for file in dir.flatten() {
        let filename = file.file_name().into_string().unwrap_or_default();
        if !filename.ends_with(".toml") || filename.starts_with('.') {
            continue;
        }
        if let Some(path) = file.path().to_str() {
            let name = filename.trim_end_matches(".toml");
            for (alias, target) in Config::read_aliases(path) {
                // Reported here rather than at each use site: a broken entry
                // would otherwise surface N times under the substituted value,
                // never naming the alias that actually needs fixing.
                if crate::config::event_from_name(&target).is_none() {
                    eprintln!(
                        "deckery: config {:?}: alias {:?} maps to unknown event {:?} — \
                         alias ignored, every binding using it is skipped",
                        name, alias, target
                    );
                    continue;
                }
                aliases.insert(alias, target);
            }
        }
    }
    aliases
}

/// The config of an entry that may take part in resolution: enabled, parsed
/// successfully, and — if it declares `requires_compositor` — matching the
/// compositor this session is running under. Every query funnels through here
/// so "usable" means exactly one thing across the registry.
/// Names of configs that no code path can reach: not a base config, not
/// activated by a window class or a layout, and not pulled in by anyone.
/// Dropping the `::` convention made this state reachable by accident — a file
/// that used to bind by its name now binds to nothing at all, silently.
pub(crate) fn orphan_configs(entries: &HashMap<String, ConfigEntry>) -> Vec<String> {
    let included: HashSet<&str> = entries.values()
        .filter_map(|e| e.config.as_ref())
        .flat_map(|c| c.module_includes.iter().map(String::as_str))
        .collect();

    let mut orphans: Vec<String> = entries.values()
        .filter_map(|e| e.config.as_ref())
        .filter(|c| {
            c.device.is_none()
                && c.module.match_window_class.is_none()
                && c.module.layout == 0
                && !included.contains(c.name.as_str())
        })
        .map(|c| c.name.clone())
        .collect();
    orphans.sort();
    orphans
}

fn usable<'a>(entry: &'a ConfigEntry, compositor: Option<&str>) -> Option<&'a Config> {
    let config = entry.config.as_ref().filter(|_| entry.enabled)?;
    match config.module.requires_compositor.as_deref() {
        Some(required) if compositor != Some(required) => None,
        _ => Some(config),
    }
}

/// Apply the plain modules a config pulls in via `[modules] include`.
///
/// Includes are the *lowest* priority layer: the including config's own
/// bindings win over anything a module provides, and later includes win over
/// earlier ones. An include that resolves to no usable entry is skipped rather
/// than failing the whole config — a module gated behind a `requires_compositor`
/// this session does not satisfy is simply absent.
fn with_includes(
    entries: &HashMap<String, ConfigEntry>,
    config: &Config,
    compositor: Option<&str>,
) -> Config {
    if config.module_includes.is_empty() {
        return config.clone();
    }
    // merge_base lets `self` win over its argument, so building the stack
    // back-to-front is what makes later includes outrank earlier ones.
    let mut stack = Config::new_empty(config.name.clone());
    for name in config.module_includes.iter().rev() {
        match entries.get(name) {
            // A module the user disabled, or one gated behind a compositor this
            // session does not run, is absent by design. Only a name matching no
            // file at all is a mistake worth reporting — folding the two together
            // buries the typo in per-switch noise.
            Some(entry) => {
                if let Some(module) = usable(entry, compositor) {
                    stack.merge_base(module);
                }
            }
            None => eprintln!(
                "deckery: config {:?}: included module {:?} does not exist",
                config.name, name
            ),
        }
    }

    let mut merged = config.clone();
    merged.merge_base(&stack);
    // merge_base treats its argument as the device-level authority and copies
    // Gaming Mode wholesale from it. Here the roles are reversed — plain modules
    // describe no hardware — so the including config keeps its own.
    merged.gaming_mode_config = config.gaming_mode_config.clone();
    // merge_base also records pre-merge bindings so state_export can tell "own"
    // from "inherited". Includes are part of what this config *is*, not an
    // override on top of it; resolve() sets the marker again if a conditional
    // module applies.
    merged.override_bindings = None;
    merged
}

#[cfg(test)]
#[path = "config_registry_tests.rs"]
mod tests;
