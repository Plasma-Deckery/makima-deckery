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
//   neither   → plain module, merged into every base config
//
// Files are discovered by scanning two roots: the system config directory that
// ships with the package, then the user's own, where a file of the same name
// replaces the system version. Nothing has to list them.
//
// Runtime merging happens in resolve() at the point of use, so enabling or
// disabling individual configs always takes effect immediately without a reload.
//
// Error handling: files that fail to parse are stored with config: None and a
// descriptive error entry.  They are visible in state.json so the tray can
// highlight broken configs, but they never become the active config and cannot
// be activated even if enabled = true.

use std::collections::{HashMap, HashSet};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use crate::config::{Config, Event};
use crate::preferences::Preferences;
use crate::udev_monitor::Client;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

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
    /// True when the file was read from the user's config directory rather than
    /// the shipped one. The only thing a file's location decides: a user module
    /// outranks every shipped module when the two bind the same button, so a
    /// small file dropped into `~/.config/deckery/` patches the shipped set
    /// without having to replace a whole file to do it.
    pub from_user: bool,
    /// Validation errors and warnings collected at load time.
    pub errors:  Vec<ConfigError>,
}

/// One config as the tray displays it: what it is and where it belongs.
/// Derived from the parsed content, never from the file's location on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigSummary {
    pub name:    String,
    /// "base" (declares a device), "app" (matches a window class), "module"
    /// (neither — applies wherever it is merged), or "unknown" (unparsed).
    pub kind:    &'static str,
    /// For modules: the base config the tray nests this one under.
    pub parent:  Option<String>,
    /// Set when this module belongs to a set of mutually exclusive modules.
    /// The tray draws such a set as radio buttons rather than checkboxes.
    pub exclusive_group: Option<String>,
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

/// The base config a plain module is displayed under.
///
/// Since auto-discovery replaced the include list there is no declared
/// relationship any more — every plain module applies to every base config. The
/// tray still draws a tree, so it needs one name: the alphabetically first base.
/// That is exact while a single device is configured, which is the only shape
/// the tray was ever able to draw.
fn parent_of(entries: &HashMap<String, ConfigEntry>, name: &str) -> Option<String> {
    let module = entries.get(name)?.config.as_ref()?;
    if !is_plain_module(module) {
        return None;
    }
    entries.values()
        .filter_map(|e| e.config.as_ref())
        .filter(|c| c.device.is_some())
        .map(|c| c.name.clone())
        .min()
}

/// Every module declaring the given exclusive group, whether enabled or not.
fn group_members(entries: &HashMap<String, ConfigEntry>, group: &str) -> HashSet<String> {
    entries.values()
        .filter(|e| e.config.as_ref()
            .and_then(|c| c.module.exclusive_group.as_deref()) == Some(group))
        .map(|e| e.name.clone())
        .collect()
}

/// Stamp the user's stored choices onto freshly loaded entries.
///
/// Modules default to enabled, so only the deviations recorded in
/// `preferences.toml` have to be replayed. For an exclusive group the recorded
/// choice wins; without one — a fresh install, or a choice naming a module that
/// has since been removed — the alphabetically first usable member is picked, so
/// a group has exactly one active member unless the whole group is switched off,
/// in which case none of its members are.
///
/// `compositor` is what the group's member choice is checked against. A member
/// gated to a compositor that is not running can be marked enabled all it
/// likes — `usable()` filters it out again at resolve time, and the group ends
/// up with a named winner and no effect. `None` means the compositor has not
/// been detected yet and nothing can be ruled out, so nothing is.
fn apply_preferences(
    entries: &mut HashMap<String, ConfigEntry>,
    preferences: &Preferences,
    compositor: Option<&str>,
) {
    for entry in entries.values_mut() {
        if entry.config.is_some() && preferences.is_disabled(&entry.name) {
            entry.enabled = false;
        }
    }

    let groups: HashSet<String> = entries.values()
        .filter_map(|e| e.config.as_ref())
        .filter_map(|c| c.module.exclusive_group.clone())
        .collect();

    for group in groups {
        let members = group_members(entries, &group);
        let usable_member = |name: &String| {
            entries.get(name).is_some_and(|e| runnable(e, compositor))
        };
        let chosen = if preferences.is_group_disabled(&group) {
            None
        } else {
            preferences.group_choice(&group)
                .map(str::to_string)
                .filter(|c| members.contains(c) && usable_member(c))
                .or_else(|| members.iter().filter(|n| usable_member(n)).min().cloned())
        };

        for member in &members {
            if let Some(e) = entries.get_mut(member) {
                e.enabled = e.config.is_some() && Some(member) == chosen.as_ref();
            }
        }
    }
}

// ── Config roots ──────────────────────────────────────────────────────────────

/// The two directories configs are read from.
///
/// System configs ship with the package and are never written to. User configs
/// are the user's own: a file there replaces the system file of the same name
/// wholesale, which is what makes a surgical override possible without forking
/// the whole config set.
#[derive(Debug, Clone)]
pub struct ConfigRoots {
    pub system: PathBuf,
    pub user:   PathBuf,
}

impl ConfigRoots {
    /// Resolve both roots from the environment.
    ///
    /// User root: `DECKERY_CONFIG` (canonical) or `MAKIMA_CONFIG` (legacy name,
    /// kept for hand-edited service overrides), else `~/.config/deckery`.
    ///
    /// System root: `DECKERY_SYSTEM_CONFIG`, else the git-install checkout, else
    /// the packaged path. The checkout wins when both exist — somebody with a
    /// working copy is running from it, and silently preferring the RPM's copy
    /// would make their edits appear to do nothing.
    pub fn resolve() -> Self {
        let home = user_home();
        let user = env::var("DECKERY_CONFIG")
            .or_else(|_| env::var("MAKIMA_CONFIG"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(format!("{home}/.config/deckery")));

        let checkout = PathBuf::from(format!("{home}/.local/share/deckery/deckery/configs"));
        let system = match env::var("DECKERY_SYSTEM_CONFIG") {
            Ok(path) => PathBuf::from(path),
            Err(_) if checkout.is_dir() => checkout,
            Err(_) => PathBuf::from("/usr/share/deckery/configs"),
        };

        Self { system, user }
    }
}

/// Name of the explainer copied into the user's config directory.
pub const README_NAME: &str = "README.md";

/// Refresh the README that explains the user's config directory from within it.
///
/// Unlike `preferences.toml` this is documentation, not state, so it is copied
/// on **every** start rather than seeded once: a description of the override
/// rules that still describes last year's rules is worse than none at all. It
/// is also the one thing an update writes into the user directory — which is
/// safe precisely because nothing the user wrote can be in it.
///
/// Skipped when the contents already match, so the common case touches no mtime
/// and wakes no file watcher. `.md` is not `.toml`, so the config watcher would
/// ignore it either way; this keeps other watchers out of it too.
fn refresh_readme(roots: &ConfigRoots) {
    let source = roots.system.join(README_NAME);
    let target = roots.user.join(README_NAME);
    let Ok(shipped) = std::fs::read(&source) else {
        // A config tree that predates the README, or a bare test root.
        return;
    };
    if std::fs::read(&target).is_ok_and(|current| current == shipped) {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&roots.user) {
        eprintln!("deckery: cannot create {:?}: {e}", roots.user);
        return;
    }
    match std::fs::write(&target, &shipped) {
        Ok(()) => eprintln!("deckery: refreshed {target:?}"),
        Err(e) => eprintln!("deckery: cannot write {target:?}: {e}"),
    }
}

/// The invoking user's home directory. Under `sudo` the process sees `/root`
/// while the configs that matter belong to the real user, so `SUDO_USER` wins
/// in that one case.
fn user_home() -> String {
    match env::var("HOME") {
        Ok(home) if home == "/root" => match env::var("SUDO_USER") {
            Ok(sudo_user) => format!("/home/{sudo_user}"),
            _ => home,
        },
        Ok(home) => home,
        _ => "/root".to_string(),
    }
}

// ── Registry ──────────────────────────────────────────────────────────────────

pub struct ConfigRegistry {
    /// Where entries are loaded from. Fixed for the lifetime of the process;
    /// `reload()` and the file watcher both read it instead of being handed a
    /// path by callers that would have to agree on it.
    roots: ConfigRoots,
    /// Keyed by config name (= file base name).
    entries: Mutex<HashMap<String, ConfigEntry>>,
    /// Name of the running compositor, as reported by XDG_CURRENT_DESKTOP
    /// (or "x11"). Gates modules that declare `[module] requires_compositor`.
    /// Set once by `set_compositor()` after the session environment is resolved;
    /// until then no compositor-specific module is considered usable.
    compositor: Mutex<Option<String>>,
    /// The user's activation choices, mirrored to `preferences.toml` on every
    /// change. Held in memory so `reload()` can re-apply them without a read.
    preferences: Mutex<Preferences>,
    /// Memoised `resolve()` results.
    ///
    /// `resolve()` runs on every key press, and rebuilding its answer means
    /// scanning every entry, sorting the plain modules and merging the whole
    /// stack — none of which depends on the key that was pressed. The answer
    /// only changes when the entries, the activation flags or the compositor
    /// change, so it is computed once per distinct question and dropped whole
    /// at those three points.
    ///
    /// `None` is cached too: "this base has no config for that layout" is the
    /// answer `change_active_layout` probes for in a loop.
    resolved: Mutex<HashMap<ResolveKey, Option<Arc<Config>>>>,
    /// Bumped by `invalidate_resolved()`. An answer is only worth caching if the
    /// entries did not change while it was being computed — the merge runs
    /// without the cache lock held, so a reload or a tray toggle can land in
    /// that window and a stale result would otherwise be written on top of the
    /// clean slate it just made.
    generation: AtomicU64,
}

/// What a resolved config depends on. The other two fields of `Client::Class`
/// are ignored by `resolve`, so they are left out rather than splitting the
/// cache on values that cannot change the answer.
#[derive(PartialEq, Eq, Hash, Clone)]
struct ResolveKey {
    base_name: String,
    window_class: Option<String>,
    layout: u16,
}

impl ConfigRegistry {
    // ── Constructors ──────────────────────────────────────────────────────────

    /// Load all `.toml` files from both roots and their `apps/` subdirectories.
    /// Never panics — parse errors are stored as ConfigEntry with config: None.
    pub fn load(roots: ConfigRoots) -> Arc<Self> {
        // Before the first read, not on every one: seeding is a no-op once the
        // user has their own copy.
        crate::preferences::seed_from(&roots.system, &roots.user);
        refresh_readme(&roots);
        let preferences = Preferences::load(&roots.user);
        let mut entries = Self::load_entries(&roots);
        // No compositor yet — `set_compositor()` runs this again once there is
        // one, which is what settles a group whose choice is gated.
        apply_preferences(&mut entries, &preferences, None);
        Arc::new(Self {
            entries: Mutex::new(entries),
            roots,
            compositor: Mutex::new(None),
            preferences: Mutex::new(preferences),
            resolved: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
        })
    }

    /// The directories this registry reads from. Fixed for its lifetime.
    pub fn roots(&self) -> &ConfigRoots {
        &self.roots
    }

    /// Empty registry.
    #[cfg(test)]
    pub(crate) fn empty() -> Arc<Self> {
        Arc::new(Self {
            roots: ConfigRoots { system: PathBuf::new(), user: PathBuf::new() },
            entries: Mutex::new(HashMap::new()),
            compositor: Mutex::new(None),
            preferences: Mutex::new(Preferences::default()),
            resolved: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
        })
    }

    /// Build a registry directly from a list of entries — for use in tests
    /// outside the `config_registry` module where `entries` is private.
    #[cfg(test)]
    pub(crate) fn from_entries(entries: Vec<ConfigEntry>) -> Arc<Self> {
        Arc::new(Self {
            roots: ConfigRoots { system: PathBuf::new(), user: PathBuf::new() },
            entries: Mutex::new(entries.into_iter().map(|e| (e.name.clone(), e)).collect()),
            compositor: Mutex::new(None),
            preferences: Mutex::new(Preferences::default()),
            resolved: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
        })
    }

    // ── Reload ────────────────────────────────────────────────────────────────

    /// Replace all entries with freshly loaded ones.
    /// Called on SIGHUP / file-watcher event — in-place so all Arc holders
    /// (udev_monitor, EventReader) see the update automatically.
    ///
    /// Activation state comes from `preferences.toml`, re-read here rather than
    /// carried forward from the old entries. It is the single source of truth:
    /// if a reload and a restart could disagree about which modules are active,
    /// the user would see their choice silently revert at the next boot.
    pub fn reload(&self) {
        let preferences = Preferences::load(&self.roots.user);
        let mut new_entries = Self::load_entries(&self.roots);
        apply_preferences(&mut new_entries, &preferences, self.compositor().as_deref());
        *self.entries.lock().unwrap() = new_entries;
        *self.preferences.lock().unwrap() = preferences;
        self.invalidate_resolved();
    }

    /// Drop every memoised `resolve()` answer.
    ///
    /// Called wherever an input to `resolve` changes: the entries, their enabled
    /// flags, or the compositor. Dropping the whole map rather than the affected
    /// keys is deliberate — a single module can appear in every resolved config,
    /// so working out which entries a change invalidates costs more than the
    /// handful of rebuilds it would save.
    fn invalidate_resolved(&self) {
        // Bump first: an in-flight resolve() that reads the counter after this
        // point but caches after the clear must still see a difference.
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.resolved.lock().unwrap().clear();
    }

    // ── File watching ─────────────────────────────────────────────────────────

    /// Start a file watcher over both config directory trees.
    ///
    /// Returns a `RecommendedWatcher` handle — drop it to stop watching.
    /// The `change_tx` channel receives a `()` unit whenever any `.toml` file
    /// under either root (recursively) is created, modified, or removed.
    ///
    /// The registry owns this logic because it is the only component that knows
    /// which directories it scans.  `udev_monitor` must not hardcode directory
    /// names; it only receives the change signal and calls `reload()`.
    ///
    /// Symlink handling: inotify does not follow symlinks, so for every `.toml`
    /// file that is a symlink (e.g. files in a git-worktree config repo) we also
    /// watch the real parent directory.  This survives editor-rename workflows
    /// (sed -i, vim swapfiles) that would otherwise invalidate an inode-based
    /// per-file watch.
    pub fn start_watcher(
        &self,
        change_tx: tokio::sync::mpsc::Sender<()>,
    ) -> RecommendedWatcher {
        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    use notify::EventKind::*;
                    match event.kind {
                        Create(_) | Modify(_) | Remove(_) => {
                            // preferences.toml is deliberately not watched. It is
                            // written by this process on every IPC toggle, and the
                            // resulting event would reload every config just to
                            // re-read what we had just written. It only ever
                            // changes through IPC, so there is nothing to observe.
                            let is_toml = event.paths.iter().any(|p| {
                                p.extension().and_then(|e| e.to_str()) == Some("toml")
                                    && p.file_name().and_then(|n| n.to_str())
                                        .is_some_and(|n| !crate::preferences::is_preferences_file(n))
                            });
                            if is_toml { let _ = change_tx.try_send(()); }
                        }
                        _ => {}
                    }
                }
            },
            notify::Config::default(),
        ).expect("Failed to create config file watcher");

        // Watch both trees recursively — no hardcoded subdirectory names; any
        // new subdirectory added in the future is covered automatically.
        //
        // A missing directory is not fatal: the user root exists only once the
        // user has actually overridden something, which most never do.
        for root in [&self.roots.system, &self.roots.user] {
            if !root.is_dir() {
                continue;
            }
            if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
                eprintln!("deckery: cannot watch config dir {root:?}: {e}");
                continue;
            }
            // Also watch the real parent directories of any symlinked .toml files.
            for dir in Self::symlink_target_parents(root) {
                let _ = watcher.watch(&dir, RecursiveMode::NonRecursive);
            }
        }

        watcher
    }

    /// Recursively collect the parent directories of every symlinked `.toml`
    /// file found under `dir`.  Used by `start_watcher` to set up additional
    /// inotify watches so that changes in git-worktree repos are detected.
    fn symlink_target_parents(dir: &Path) -> HashSet<PathBuf> {
        let mut out = HashSet::new();
        Self::collect_symlink_parents(dir, &mut out);
        out
    }

    fn collect_symlink_parents(dir: &Path, out: &mut HashSet<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::collect_symlink_parents(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                if let Ok(real) = std::fs::canonicalize(&path) {
                    if real != path {
                        if let Some(parent) = real.parent() {
                            out.insert(parent.to_path_buf());
                        }
                    }
                }
            }
        }
    }

    // ── Environment ───────────────────────────────────────────────────────────

    /// Record which compositor the session is running under. Modules declaring
    /// a different `[module] requires_compositor` are excluded from every query
    /// from this point on. Called once, after the environment is resolved.
    pub fn set_compositor(&self, name: Option<String>) {
        *self.compositor.lock().unwrap() = name.clone();
        // A group's member choice is made against the gating, and at load time
        // there was no compositor to check it against. Re-running that choice
        // here is what keeps a group from settling on a member that cannot run.
        {
            let preferences = self.preferences.lock().unwrap();
            let mut entries = self.entries.lock().unwrap();
            apply_preferences(&mut entries, &preferences, name.as_deref());
        }
        // Gating decides which modules are usable, so every answer changes.
        self.invalidate_resolved();
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
    /// merged with every enabled plain module.
    /// `launch_tasks()` iterates these declared targets to find physical devices.
    pub fn base_configs(&self) -> Vec<Config> {
        let compositor = self.compositor();
        let entries = self.entries.lock().unwrap();
        entries.values()
            .filter_map(|e| usable(e, compositor.as_deref()))
            .filter(|c| c.device.is_some())
            .map(|c| with_modules(&entries, c, compositor.as_deref()))
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
    ///   1. every enabled plain module, alphabetically last one winning
    ///   2. the base config itself
    ///   3. the conditional module matching the current window class and layout
    ///
    /// Returns None when `base_name` names no usable base config, or when a
    /// layout other than 0 is requested and no module covers it — the latter is
    /// what lets `change_active_layout()` skip unpopulated layout slots.
    /// The result is memoised — see the `resolved` field. Callers get an `Arc`
    /// because this runs per key press and the config holds several maps that
    /// would otherwise be deep-copied each time.
    pub fn resolve(&self, base_name: &str, client: &Client, layout: u16) -> Option<Arc<Config>> {
        let key = ResolveKey {
            base_name: base_name.to_string(),
            window_class: match client {
                // A class no module claims resolves exactly like no class at all,
                // so it is folded into the same key. Without this the cache would
                // grow by one full merged config for every window the user has
                // ever focused, all of them identical.
                Client::Class(class, _, _) if self.claims_window_class(class) => {
                    Some(class.clone())
                }
                _ => None,
            },
            layout,
        };
        if let Some(hit) = self.resolved.lock().unwrap().get(&key) {
            return hit.clone();
        }
        // Read before the work, compared after it. The merge runs without the
        // cache lock held — on purpose, it is the expensive part — so a reload
        // or a tray toggle can clear the cache while this answer is still being
        // built. Writing it anyway would put a pre-change config back into a
        // cache that was just emptied because of that change, where it would sit
        // until something else invalidated it.
        let generation = self.generation.load(Ordering::Acquire);
        let answer = self.resolve_uncached(base_name, client, layout).map(Arc::new);
        let mut resolved = self.resolved.lock().unwrap();
        if self.generation.load(Ordering::Acquire) == generation {
            resolved.insert(key, answer.clone());
        }
        answer
    }

    /// Does any usable module name this window class?
    ///
    /// Deliberately ignores the layout, unlike the lookup in `resolve_uncached`:
    /// a class claimed for some other layout is still worth keying on, and being
    /// generous here only costs a cache entry, whereas being too narrow would
    /// serve one window's override to another.
    fn claims_window_class(&self, class: &str) -> bool {
        let compositor = self.compositor();
        self.entries.lock().unwrap()
            .values()
            .filter_map(|e| usable(e, compositor.as_deref()))
            .any(|c| c.module.match_window_class.as_deref()
                .is_some_and(|patterns| patterns.iter().any(|p| p == class)))
    }

    /// Build a resolved config from scratch. Everything expensive lives here;
    /// `resolve` is what keeps it from running more than once per question.
    fn resolve_uncached(&self, base_name: &str, client: &Client, layout: u16) -> Option<Config> {
        let compositor = self.compositor();
        let compositor = compositor.as_deref();
        let entries = self.entries.lock().unwrap();

        let base_raw = usable(entries.get(base_name)?, compositor)
            .filter(|c| c.device.is_some())?;
        let base = with_modules(&entries, base_raw, compositor);

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
            candidates().find(|c| {
                c.module.match_window_class.as_deref()
                    .is_some_and(|patterns| patterns.iter().any(|p| p == class))
            })
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
            let base = with_modules(&entries, base_raw, compositor);
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

    /// Set the enabled flag for one config entry and persist the choice.
    ///
    /// Returns true if the change was applied.
    /// Returns false if the name does not exist, or if `enabled = true` is
    /// requested for an entry that failed to parse (`config: None`) — a
    /// broken config cannot be activated regardless of the flag.
    ///
    /// Enabling a member of an exclusive group switches its siblings off in the
    /// same step. The group is a single choice, so the tray only ever sends the
    /// activation — the deactivations happen here, which is what keeps them from
    /// racing the activation over IPC.
    ///
    /// Switching the *active* member of a group off switches the whole group off
    /// — that is the only state in which a group member can be off while the
    /// group still remembers it. Doing that on an already inactive member is a
    /// no-op: the click says nothing the group does not already reflect, and
    /// taking the whole group down over it would be a surprise.
    pub fn set_enabled(&self, name: &str, enabled: bool) -> bool {
        // Both locks are dropped before the file is written. resolve() needs
        // `entries` and runs on the input path, so it must not wait on disk.
        let preferences = {
            let mut entries = self.entries.lock().unwrap();
            let Some(entry) = entries.get(name) else { return false };
            if enabled && entry.config.is_none() {
                return false;
            }
            let group = entry.config.as_ref().and_then(|c| c.module.exclusive_group.clone());
            let was_enabled = entry.enabled;

            let mut preferences = self.preferences.lock().unwrap();
            match (&group, enabled) {
                (Some(group), true) => {
                    let siblings = group_members(&entries, group);
                    for sibling in &siblings {
                        if let Some(e) = entries.get_mut(sibling) {
                            e.enabled = sibling == name;
                        }
                    }
                    preferences.set_group_choice(group, name, &siblings);
                }
                (Some(group), false) => {
                    if !was_enabled {
                        return true;
                    }
                    let group = group.clone();
                    for member in group_members(&entries, &group) {
                        if let Some(e) = entries.get_mut(&member) {
                            e.enabled = false;
                        }
                    }
                    preferences.set_group_disabled(&group, true);
                }
                _ => {
                    entries.get_mut(name).unwrap().enabled = enabled;
                    preferences.set_disabled(name, !enabled);
                }
            }
            preferences.clone()
        };
        self.invalidate_resolved();
        preferences.save(&self.roots.user);
        true
    }

    /// Switch a whole exclusive group on or off and persist the choice.
    ///
    /// Returns false if no loaded config declares that group.
    ///
    /// Switching on restores the remembered member, falling back to the
    /// alphabetically first usable one — the same rule `apply_preferences` uses,
    /// so a restart lands on the member the click just produced.
    pub fn set_group_enabled(&self, group: &str, enabled: bool) -> bool {
        let compositor = self.compositor();
        let preferences = {
            let mut entries = self.entries.lock().unwrap();
            let members = group_members(&entries, group);
            if members.is_empty() {
                return false;
            }
            let mut preferences = self.preferences.lock().unwrap();

            let usable = |name: &String| entries.get(name)
                .is_some_and(|e| runnable(e, compositor.as_deref()));
            let chosen = enabled
                .then(|| {
                    preferences.group_choice(group)
                        .map(str::to_string)
                        .filter(|c| members.contains(c) && usable(c))
                        .or_else(|| members.iter().filter(|n| usable(n)).min().cloned())
                })
                .flatten();

            for member in &members {
                if let Some(e) = entries.get_mut(member) {
                    e.enabled = Some(member) == chosen.as_ref();
                }
            }
            match &chosen {
                Some(name) => preferences.set_group_choice(group, name, &members),
                // Also covers "on, but every member is broken": nothing can be
                // activated, so the stored state has to stay off.
                None => preferences.set_group_disabled(group, true),
            }
            preferences.clone()
        };
        self.invalidate_resolved();
        preferences.save(&self.roots.user);
        true
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
                exclusive_group: e.config.as_ref()
                    .and_then(|c| c.module.exclusive_group.clone()),
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

    fn load_entries(roots: &ConfigRoots) -> HashMap<String, ConfigEntry> {
        let mut map = HashMap::new();
        // System first, user second: entries are keyed by config name, so a user
        // file of the same name simply overwrites the system entry as it is read.
        // Within each root, the `apps/` subdirectory is scanned alongside it.
        let dirs_to_scan: Vec<(PathBuf, bool)> = [(&roots.system, false), (&roots.user, true)]
            .into_iter()
            .flat_map(|(root, from_user)| {
                [(root.clone(), from_user), (root.join("apps"), from_user)]
            })
            .filter(|(dir, _)| dir.is_dir())
            .collect();

        // Aliases are declared on the base config's [device] block but apply to
        // every file, so they must be known before the first file is parsed:
        // parse_event_name resolves names the moment a binding is read.
        let aliases = collect_aliases(roots);

        for (scan_dir, from_user) in dirs_to_scan {
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
                // Sits next to the configs but is not one — it records which of
                // them the user switched on.
                if crate::preferences::is_preferences_file(&filename) {
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
                    from_user,
                });
            }
        }

        // Only knowable once every file has been read.
        report_binding_conflicts(&mut map);
        report_device_conflicts(&mut map);

        map
    }
}

// ── Free helpers ──────────────────────────────────────────────────────────────

/// Button aliases from every base config in either root. Modules and app
/// overrides live in `apps/` or carry no `[device]` block, so they contribute
/// nothing — the names belong to the hardware, and only a base config names it.
///
/// Read system-first so a user base config redefining an alias wins, matching
/// how the entries themselves are layered.
fn collect_aliases(roots: &ConfigRoots) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    for root in [&roots.system, &roots.user] {
        collect_aliases_from(root, &mut aliases);
    }
    aliases
}

fn collect_aliases_from(config_dir: &Path, aliases: &mut HashMap<String, String>) {
    let dir = match std::fs::read_dir(config_dir) {
        Ok(d) => d,
        Err(_) => return,
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
}

/// The config of an entry that may take part in resolution: enabled, parsed
/// successfully, and — if it declares `requires_compositor` — matching the
/// compositor this session is running under. Every query funnels through here
/// so "usable" means exactly one thing across the registry.
/// Could this entry be active here — ignoring whether it currently is?
///
/// `usable()` answers "is it live right now", which folds in `enabled`. Picking
/// which member of a group to enable has to ask the other question: the members
/// are all switched off at that moment, and one is about to be chosen.
///
/// An undetected compositor rules nothing out. Gating is only ever a reason to
/// reject, so not knowing the answer means not rejecting.
fn runnable(entry: &ConfigEntry, compositor: Option<&str>) -> bool {
    let Some(config) = entry.config.as_ref() else { return false };
    match (config.module.requires_compositor.as_deref(), compositor) {
        (Some(required), Some(running)) => required == running,
        _ => true,
    }
}

fn usable<'a>(entry: &'a ConfigEntry, compositor: Option<&str>) -> Option<&'a Config> {
    let config = entry.config.as_ref().filter(|_| entry.enabled)?;
    match config.module.requires_compositor.as_deref() {
        Some(required) if compositor != Some(required) => None,
        _ => Some(config),
    }
}

/// A config that applies wherever it is merged: it names no device, matches no
/// window class, and belongs to no layout but the base one. These are the files
/// auto-discovery pulls in — everything else is activated by a condition.
fn is_plain_module(config: &Config) -> bool {
    config.device.is_none()
        && config.module.match_window_class.is_none()
        && config.module.layout == 0
}

/// Merge every usable plain module into `config`.
///
/// Configs are layered by who wrote them, not by what kind of file they are:
///
/// ```text
///   shipped modules  <  shipped base config  <  the user's own files
/// ```
///
/// Within one of those layers the alphabetically later name wins. That tie-break
/// is arbitrary but deterministic, and it is not meant to be used — two shipped
/// modules claiming one binding is a config bug, reported at load time by
/// `report_binding_conflicts()` instead of being settled quietly here.
///
/// The user's modules sit *above* the shipped base config rather than under it,
/// and that is deliberate: the bindings somebody most wants to change — what A
/// does, what the paddles do — are declared in the base config, and having to
/// adopt the whole file to move one of them is the trade the root rule exists to
/// avoid. When the base config is itself the user's, it keeps the last word: at
/// that point nothing distinguishes the two, and the base config is the more
/// specific statement.
fn with_modules(
    entries: &HashMap<String, ConfigEntry>,
    config: &Config,
    compositor: Option<&str>,
) -> Config {
    let mut modules: Vec<(bool, &Config)> = entries.values()
        .filter_map(|e| Some((e.from_user, usable(e, compositor)?)))
        .filter(|(_, c)| is_plain_module(c))
        .collect();
    if modules.is_empty() {
        return config.clone();
    }
    modules.sort_by(|(a_user, a), (b_user, b)| a_user.cmp(b_user).then(a.name.cmp(&b.name)));

    let base_from_user = entries.get(&config.name).is_some_and(|e| e.from_user);
    // Everything goes under the base config when the base config is the user's
    // own, so there is nothing to lift above it.
    let split = if base_from_user {
        modules.len()
    } else {
        modules.iter().position(|(from_user, _)| *from_user).unwrap_or(modules.len())
    };
    let (below, above) = modules.split_at(split);

    // merge_base lets `self` win over its argument, so building a stack
    // back-to-front is what makes later modules outrank earlier ones.
    let mut stack = Config::new_empty(config.name.clone());
    for (_, module) in below.iter().rev() {
        stack.merge_base(module);
    }

    let mut merged = config.clone();
    merged.merge_base(&stack);

    if !above.is_empty() {
        let mut top = Config::new_empty(config.name.clone());
        for (_, module) in above.iter().rev() {
            top.merge_base(module);
        }
        top.merge_base(&merged);
        // merge_base carries bindings, settings and the trackpad across, but not
        // what the config *is* — and `top` started life as an empty shell.
        top.device  = merged.device.clone();
        top.aliases = merged.aliases.clone();
        top.module  = merged.module.clone();
        merged = top;
    }
    // merge_base treats its argument as the device-level authority and copies
    // Gaming Mode wholesale from it. Here the roles are reversed — plain modules
    // describe no hardware — so the base config keeps its own.
    merged.gaming_mode_config = config.gaming_mode_config.clone();
    // merge_base also records pre-merge bindings so state_export can tell "own"
    // from "inherited". Modules are part of what this config *is*, not an
    // override on top of it; resolve() sets the marker again if a conditional
    // module applies.
    merged.override_bindings = None;
    merged
}

/// Every button combination a config binds, whatever the binding type.
fn bound_combos(config: &Config) -> HashSet<(Event, Vec<Event>)> {
    let b = &config.bindings;
    let mut out = HashSet::new();
    for (trigger, combos) in &b.remap {
        out.extend(combos.keys().map(|combo| (*trigger, combo.clone())));
    }
    for (trigger, combos) in &b.commands {
        out.extend(combos.keys().map(|combo| (*trigger, combo.clone())));
    }
    for (trigger, combos) in &b.movements {
        out.extend(combos.keys().map(|combo| (*trigger, combo.clone())));
    }
    out
}

/// Two modules can only collide if they can be active at the same time. Ones
/// gated to different compositors never are — KDE and Hyprland modules binding
/// the same button is the normal case, not a mistake.
fn gates_overlap(a: Option<&String>, b: Option<&String>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => x == y,
        _ => true,
    }
}

/// Attach a warning to every plain module that takes a binding away from
/// another one.
///
/// The merge order settles the collision deterministically, but silently: the
/// losing binding simply never fires, and nothing about the two files says why.
/// The warning names both configs so the fix is obvious. It lands on the winner
/// because that is the file whose binding is actually in effect.
///
/// A user module beating a shipped one is exempt: that is not an accident but
/// the supported way to change a single binding without adopting the whole file
/// it came in.
fn report_binding_conflicts(entries: &mut HashMap<String, ConfigEntry>) {
    struct Module {
        name:       String,
        gate:       Option<String>,
        group:      Option<String>,
        from_user:  bool,
        combos:     HashSet<(Event, Vec<Event>)>,
    }
    let mut modules: Vec<Module> = entries.values()
        .filter_map(|e| Some((e.from_user, e.config.as_ref()?)))
        .filter(|(_, c)| is_plain_module(c))
        .map(|(from_user, c)| Module {
            name:   c.name.clone(),
            gate:   c.module.requires_compositor.clone(),
            group:  c.module.exclusive_group.clone(),
            from_user,
            combos: bound_combos(c),
        })
        .collect();
    modules.sort_by(|a, b| a.from_user.cmp(&b.from_user).then(a.name.cmp(&b.name)));

    // This is merge order, so anything later in this list wins over what came
    // before it.
    for (i, module) in modules.iter().enumerate() {
        let name = &module.name;
        let mut warnings: Vec<String> = Vec::new();
        for earlier in &modules[..i] {
            if !gates_overlap(module.gate.as_ref(), earlier.gate.as_ref()) {
                continue;
            }
            // The deliberate case: a file in the user's directory taking a
            // binding off a shipped one is the patch mechanism working.
            if module.from_user && !earlier.from_user {
                continue;
            }
            // Two members of the same exclusive group are never live together,
            // so binding the same button in both is the point of the group —
            // the same reasoning that exempts differently gated compositors.
            if module.group.is_some() && module.group == earlier.group {
                continue;
            }
            let mut shared: Vec<&(Event, Vec<Event>)> =
                module.combos.intersection(&earlier.combos).collect();
            if shared.is_empty() {
                continue;
            }
            shared.sort();
            warnings.push(format!(
                "overrides {} binding(s) from config {:?}: {} — \
                 both configs bind the same button, only this one takes effect",
                shared.len(),
                earlier.name,
                shared.iter()
                    .map(|(trigger, combo)| format!("{trigger:?}+{combo:?}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        for message in warnings {
            eprintln!("deckery: config {name:?}: {message}");
            if let Some(entry) = entries.get_mut(name) {
                entry.errors.push(ConfigError { severity: "warning", message });
            }
        }
    }
}

/// Warn when two base configs can be claimed by the same controller.
///
/// `launch_tasks()` walks the base configs and opens the first evdev device
/// each one matches, so two declarations covering one controller both find it —
/// and which of them ends up driving it comes down to `HashMap` iteration
/// order. Nothing about either file says that, and the loser's bindings simply
/// are not there.
///
/// The usual way to get into this: a copy of a base config left in the user's
/// directory under a name that has since been retired. It is still a valid base
/// config, it still names the same hardware, and it is invisible as a problem
/// unless something says so.
///
/// Overlap is only reported when one declared name contains the other. That is
/// the case where a device matching the narrower declaration necessarily
/// matches the wider one too — a guarantee, not a guess, which is what keeps
/// this from warning about unrelated hardware that merely shares a word.
fn report_device_conflicts(entries: &mut HashMap<String, ConfigEntry>) {
    struct Base {
        name:  String,
        gate:  Option<String>,
        names: Vec<String>,
    }
    let mut bases: Vec<Base> = entries.values()
        .filter_map(|e| e.config.as_ref())
        .filter_map(|c| Some(Base {
            name:  c.name.clone(),
            gate:  c.module.requires_compositor.clone(),
            names: c.device.as_ref()?.names.clone(),
        }))
        .collect();
    bases.sort_by(|a, b| a.name.cmp(&b.name));

    for (i, base) in bases.iter().enumerate() {
        for earlier in &bases[..i] {
            if !gates_overlap(base.gate.as_ref(), earlier.gate.as_ref()) {
                continue;
            }
            let Some(shared) = base.names.iter()
                .find(|n| earlier.names.iter()
                    .any(|m| n.contains(m.as_str()) || m.contains(n.as_str())))
            else { continue };

            let message = format!(
                "declares device {shared:?}, which config {:?} also claims — \
                 both match the same controller and only one of them drives it. \
                 Delete or rename whichever is the leftover.",
                earlier.name,
            );
            eprintln!("deckery: config {:?}: {message}", base.name);
            if let Some(entry) = entries.get_mut(&base.name) {
                entry.errors.push(ConfigError { severity: "warning", message });
            }
        }
    }
}

#[cfg(test)]
#[path = "config_registry_tests.rs"]
mod tests;
