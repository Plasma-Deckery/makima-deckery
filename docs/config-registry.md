# Config Registry

The `ConfigRegistry` (`src/config_registry.rs`) is the single source of truth for all loaded configuration files. It replaced the old `Vec<Config>` approach that was passed through `udev_monitor` and duplicated between modules.

## Why it exists

Previously, config data existed in two forms simultaneously:
- `main.rs` read all files into `Vec<Config>`
- `udev_monitor` parsed associations from filenames, merged base into app configs, and built a per-device `Vec<Config>` that was passed to `EventReader`
- `EventReader` stored an `Arc<Mutex<HashMap<String, ConfigEntry>>>` internally

This dual source of truth made runtime enable/disable of individual configs impossible without a full reload. Configs were stored **pre-merged** (base baked into every app config at load time), so disabling just one app config required rebuilding the entire merged set.

## How a config declares what it is

A config declares its role through its **content**, never through its filename. All `.toml` files in the config directories and their `apps/` subdirectories are loaded into the registry as equals; what distinguishes them is which sections they carry.

| Section | Role |
|---|---|
| `[device]` | **Base config** — names the physical device it drives |
| `[module]` | **Conditional module** — activation conditions (window class, compositor) |
| neither | **Plain module** — merged into every base config |

There is no include list. A plain module applies by existing in a config directory, which means adding a feature is dropping in a file and removing it is deleting one — no second place to keep in sync.

```toml
# Steam Deck Base.toml — a base config
[device]
class = "hid-steam"                       # or "evdev"
names = ["Steam Deck", "Valve Software"]  # substring-matched against the evdev name
```

```toml
# apps/konsole.toml — a conditional module
[module]
match_window_class = "org.kde.konsole"
requires_compositor = "KDE"               # optional gate
```

`names` entries are matched as substrings against the kernel-reported evdev device name, so one base config covers every naming variant of the same hardware.

## The two roots

`ConfigRoots::resolve()` picks the two directories that are scanned, in this order:

| Root | Where |
|---|---|
| system | `$DECKERY_SYSTEM_CONFIG`, else `~/.local/share/deckery/deckery/configs` if that checkout exists, else `/usr/share/deckery/configs` |
| user | `$DECKERY_CONFIG`, else `$MAKIMA_CONFIG`, else `~/.config/deckery` |

Entries are keyed by file base name and the system root is read first, so a user file of the same name **replaces** the system one outright. That is the entire override mechanism: nothing is copied at install time, and an update to a shipped config reaches every user who has not overridden that specific file.

A user file only replaces the shipped one **if it parses**. When it does not, the shipped config stays in effect and the entry carries a `warning` naming the file that was skipped — an override that cannot be read is a mistake in the copy, not a reason to lose the original. Such an entry also reverts to `from_user = false`, because the config actually in effect is the shipped one and has to be layered as one. A broken user file with no shipped twin is still a hard `error`: there is nothing to fall back to.

The fixed sections — `[module]`, `[device]`, `[gaming_mode]`, `[trackpad]`, and the set of section names itself — are `deny_unknown_fields`, so a misspelt key is a parse error rather than a silently dropped line. `match_window_classes` used to turn an app override into a plain module applied to every window; `[remaps]` used to be ignored wholesale. The binding maps are exempt: their keys are button names, and there is no list of legal ones.

Only the system root has to exist. The user root stays absent until someone writes an override, and a missing one is not an error.

## Architecture

```
main.rs
  → ConfigRoots::resolve()               // system + user config directory
  → ConfigRegistry::load(roots)          // read disk, parse, validate
  → Arc<ConfigRegistry>                  // shared via clone — one instance
  → udev_monitor::start_monitoring_udev(registry, ..., ipc_tx)
      → session::set_environment()        // what session are we in
      → registry.set_compositor(...)      // once, after the environment is resolved
      → DeviceSession { registry, environment, ... }   // built once
      → device_tasks::launch_tasks(&session, &mut tasks)   // and on every reinit
          → registry.base_configs()       // declared targets → find matching devices
          → registry.any_device_matches()  // is this evdev node one of ours?
          → EventReader { registry, base_name, ... }
              → registry.resolve()         // on every config switch
              → registry.window_class_modules()  // for active_client matching
              → registry.set_enabled()     // on IPC enable/disable
              → registry.snapshot()        // for SetLoadedConfigs state update
```

Configs are stored **unmerged** — exactly as they appear on disk. `resolve()` merges the layers at the point of use. Enabling or disabling a config takes effect on the next key press without any reload.

Device discovery is *inverted* relative to the old design: instead of scanning evdev nodes and looking for a config named after each one, the registry hands out its declared `[device]` targets and `device_tasks::launch_tasks()` finds the physical device that matches.

## Key types

```rust
pub struct ConfigError {
    pub severity: &'static str,  // "error" | "warning"
    pub message:  String,
}

pub struct ConfigEntry {
    pub name:    String,
    pub config:  Option<Config>,  // None = file could not be parsed
    pub enabled: bool,            // from preferences.toml, toggled via IPC
    pub errors:  Vec<ConfigError>,
}
```

Files that fail to parse are stored with `config: None`. They appear in `state.json` so the tray can show them as broken, but no query ever returns them even if `enabled = true`.

## Key methods

| Method | Used by | Purpose |
|---|---|---|
| `load(roots) -> Arc<Self>` | `main.rs` | Create registry from both roots at startup |
| `reload()` | `udev_monitor` | In-place update on file-watcher event — activation state is re-read from `preferences.toml` |
| `set_compositor(Option<String>)` | `udev_monitor` | Record the session compositor; gates `requires_compositor` modules |
| `any_device_matches(evdev_name) -> bool` | `udev_monitor` | Is this evdev node claimed by a base config? |
| `base_configs() -> Vec<Config>` | `device_tasks` | Declared device targets, plain modules already merged |
| `window_class_modules() -> Vec<Config>` | `active_client` | Modules that declare a `match_window_class` |
| `resolve(base_name, client) -> Option<Config>` | `EventReader` | Merged active config for the current window |
| `set_enabled(name, bool)` | `EventReader` IPC | Toggle config on/off; deactivates exclusive-group siblings and persists to `preferences.toml` |
| `snapshot() -> Vec<ConfigEntry>` | `EventReader`, `udev_monitor`, `device_tasks` | For `SetLoadedConfigs` state update |
| `base_config_error() -> Option<String>` | `udev_monitor` | First hard parse error, for startup diagnostics |

## Usability gate

Every query filters entries through the same predicate: the entry must be `enabled`, must have parsed (`config: Some`), and — if it declares `[module] requires_compositor` — that compositor must be the one recorded via `set_compositor()`. The compositor is unknown at load time, which is why it is set separately once `session::set_environment()` has resolved the session.

## resolve() logic

`resolve()` is keyed on the **base config's name**, not on a kernel device name. Device matching happens exactly once, in `device_tasks::launch_tasks()`; `EventReader` carries the resulting config name and passes it back on every switch. Re-deriving the match per keystroke would make the outcome depend on whether the filename happens to resemble the `[device] names` entries.

Merge order, lowest priority first:

1. **Every usable plain module**, sorted by name. The alphabetically last one outranks the earlier ones.
2. **The base config** — the usable entry with that name, which must have a `[device]` section.
3. **The app override** whose `match_window_class` matches the focused window, if there is one.

Returns `None` only when the name identifies no usable base config. There used to be a second reason — a layout with no module covering it — but the layout mechanism came from upstream makima, no shipped config ever used it, and it was removed along with the loop that probed for those empty slots.

Two asymmetries are worth knowing about, since both are load-bearing in `with_modules()`:

- `merge_base()` lets `self` win over its argument, so the module stack is built **back to front** — that is what makes the alphabetically last module outrank the earlier ones.
- `merge_base()` treats its argument as the *device-level authority* and copies `gaming_mode_config` from it wholesale. Here the roles are reversed (plain modules describe no hardware), so the base config's Gaming Mode settings are restored after the merge.

## Exclusive groups

A module can declare `[module] exclusive_group = "name"`. Enabling one member switches its siblings off inside the same `set_enabled()` call, so there is never a moment in which two members are both live and the tray does not have to send the deactivations itself.

`apply_preferences()` guarantees the other half: after every load, each group has exactly one enabled member — unless the group as a whole is switched off, in which case it has none. The stored choice wins; if there is none — a fresh install — or it names a module that has since been removed or fails to parse, the alphabetically first usable member is picked. There is deliberately **no `default = true` flag**: two files could both claim it, and the file that shipped the default could not be told from the user's choice. Shipped groups are instead named so the intended default sorts first.

### Switching a whole group off

`set_group_enabled(group, false)` — IPC `config group disable <slug>` — leaves every member inactive and records the group in `[groups] disabled`. The member choice in `[exclusive_groups]` is deliberately **not** cleared: off is a state of the group, not the absence of a decision, and dropping the choice would silently reset the user to the alphabetically first member when they switch it back on.

Two things follow from that split:

- **state.json needs no new field.** A group with no enabled member *is* a group switched off, and that is the only part a client can act on. The remembered choice is not client state.
- **`set_enabled(member, false)` is not a plain disable.** Switching the *active* member off is read as switching the group off — the only state in which a member can be inactive while the group still remembers it. On an already inactive member it is a no-op: the click says nothing the group does not already reflect, and taking the whole group down over it would be a surprise.

## preferences.toml

Activation state is not configuration. The module `.toml` files say what a module does; whether it is currently running is recorded separately, in `<user root>/preferences.toml`:

```toml
[exclusive_groups]
kde-desktop-layout = "KDE Desktop Layout Vertical"

[groups]
disabled = []

[modules]
disabled = ["Voice Control"]
```

Keeping them apart is what lets the system root stay strictly read-only. It also means only deviations are stored — a module named nowhere in the file is enabled, so a newly shipped module arrives active without the user opting in.

`load_entries()` skips the file by name: it sits in the same directory as the configs and would otherwise be discovered as a module.

A malformed `preferences.toml` is reported and ignored rather than fatal. Losing the user's toggles is bad; refusing to start the input stack over them is worse.

## reload() and activation state

`reload()` replaces all entries in-place and re-reads `preferences.toml` rather than carrying the in-memory `enabled` flags forward. The file is the single source of truth — if a reload and a restart could disagree about which modules are active, a user's choice would silently revert at the next boot.

A config that fails to parse is never enabled regardless of what preferences say, and becomes active again on the reload that follows the fix.

## Error configs

Configs that fail to parse:
- Start as `enabled: false`
- Appear in `state.json` `configs[]` with `status: "error"` and `errors[]` populated
- Cannot be activated even via `set_enabled(true)` — every query filters `config: None` entries
- Are automatically re-enabled when the file is fixed and the watcher triggers a reload

## Binding conflicts

Plain modules all stack onto the same base config, so two of them binding the same button is a config bug: one silently wins and the other's binding is never reachable. There is deliberately **no priority field** to resolve this — a priority field would make the collision legitimate and hide the mistake instead of showing it.

Instead `report_binding_conflicts()` runs once after loading, walks the modules in name order, and for each one compares its bound `(trigger, combo)` pairs against every module that comes before it. An overlap produces a `warning` on the module that *wins*, naming the one it shadows. The warning reaches `state.json`, so the tray shows the config yellow rather than the collision being visible only in the journal.

Modules gated to different compositors are excluded: `KDE Desktop` and `Hyprland Desktop` never load together, so binding the same button in both is the intended translation of one gesture, not a conflict. Base configs and app overrides are excluded too — they are *meant* to override the modules under them.

## IPC socket architecture

The IPC socket (`$XDG_RUNTIME_DIR/makima-control.sock`) is bound once in `main.rs` and broadcast via `tokio::sync::broadcast::Sender<String>` to all active `EventReader` instances. Each reader has its own `ipc_command_loop(rx)` that reads from the broadcast receiver.

This replaces the old design where `EventReader` bound the socket itself — which caused a silent socket takeover bug in multi-device setups (the second reader stole the socket from the first, leaving the first device unable to receive IPC commands).

## state.json output

Names are plain filenames — there is no naming convention to decode.

```json
"configs": [
  { "name": "Steam Deck",   "enabled": true,  "status": "ok",    "errors": [] },
  { "name": "firefox",      "enabled": true,  "status": "ok",    "errors": [] },
  { "name": "kde-gestures", "enabled": true,  "status": "ok",    "errors": [] },
  { "name": "KDE Desktop Layout", "enabled": true, "status": "ok",
    "exclusive_group": "kde-desktop-layout", "errors": [] },
  { "name": "broken",       "enabled": false, "status": "error",
    "errors": [{ "severity": "error", "message": "TOML parse error at line 5: ..." }] }
]
```

## Tests

`src/config_registry_tests.rs` covers: `any_device_matches`, `base_configs`, `window_class_modules`, `requires_compositor` gating, automatic module merging (merge order and Gaming Mode preservation), binding-conflict warnings, two-root discovery and override, exclusive groups (sibling deactivation, default and fallback selection, persistence), `resolve` (keyed on config name, window-class match, disabled entry filtered, no base config), `set_enabled`, `snapshot`, and `base_config_error`.
