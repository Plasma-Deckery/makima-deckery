# Config Registry

The `ConfigRegistry` (`src/config_registry.rs`) is the single source of truth for all loaded configuration files. It replaced the old `Vec<Config>` approach that was passed through `udev_monitor` and duplicated between modules.

## Why it exists

Previously, config data existed in two forms simultaneously:
- `main.rs` read all files into `Vec<Config>`
- `udev_monitor` parsed associations from filenames, merged base into app configs, and built a per-device `Vec<Config>` that was passed to `EventReader`
- `EventReader` stored an `Arc<Mutex<HashMap<String, ConfigEntry>>>` internally

This dual source of truth made runtime enable/disable of individual configs impossible without a full reload. Configs were stored **pre-merged** (base baked into every app config at load time), so disabling just one app config required rebuilding the entire merged set.

## How a config declares what it is

A config declares its role through its **content**, never through its filename. All `.toml` files in the config directory and its `apps/` subdirectory are loaded into the registry as equals; what distinguishes them is which sections they carry.

| Section | Role |
|---|---|
| `[device]` | **Base config** — names the physical device it drives |
| `[module]` | **Conditional module** — activation conditions (window class, layout, compositor) |
| `[modules] include` | **Composition** — plain modules merged into this config |

A file with none of these is a *plain module*: inert on its own, usable only by being named in someone else's `include` list.

```toml
# Steam Deck.toml — a base config
[device]
class = "hid-steam"                       # or "evdev"
names = ["Steam Deck", "Valve Software"]  # substring-matched against the evdev name

[modules]
include = ["kde-gestures", "media-keys"]  # later entries outrank earlier ones
```

```toml
# apps/konsole.toml — a conditional module
[module]
match_window_class = "org.kde.konsole"
layout = 0
requires_compositor = "KDE"               # optional gate
```

`names` entries are matched as substrings against the kernel-reported evdev device name, so one base config covers every naming variant of the same hardware.

## Architecture

```
main.rs
  → ConfigRegistry::load(config_dir)     // read disk, parse, validate
  → Arc<ConfigRegistry>                  // shared via clone — one instance
  → udev_monitor::start_monitoring_udev(registry, ..., ipc_tx)
      → registry.set_compositor(...)      // once, after the environment is resolved
      → launch_tasks(registry, ...)
          → registry.base_configs()       // declared targets → find matching devices
          → registry.any_device_matches()  // is this evdev node one of ours?
          → EventReader { registry, base_name, ... }
              → registry.resolve()         // on every config switch
              → registry.window_class_modules()  // for active_client matching
              → registry.set_enabled()     // on IPC enable/disable
              → registry.snapshot()        // for SetLoadedConfigs state update
```

Configs are stored **unmerged** — exactly as they appear on disk. `resolve()` merges the layers at the point of use. Enabling or disabling a config takes effect on the next key press without any reload.

Device discovery is *inverted* relative to the old design: instead of scanning evdev nodes and looking for a config named after each one, the registry hands out its declared `[device]` targets and `launch_tasks()` finds the physical device that matches.

## Key types

```rust
pub struct ConfigError {
    pub severity: &'static str,  // "error" | "warning"
    pub message:  String,
}

pub struct ConfigEntry {
    pub name:    String,
    pub config:  Option<Config>,  // None = file could not be parsed
    pub enabled: bool,            // runtime-toggled via IPC
    pub errors:  Vec<ConfigError>,
}
```

Files that fail to parse are stored with `config: None`. They appear in `state.json` so the tray can show them as broken, but no query ever returns them even if `enabled = true`.

## Key methods

| Method | Used by | Purpose |
|---|---|---|
| `load(config_dir) -> Arc<Self>` | `main.rs` | Create registry from disk at startup |
| `reload(config_dir)` | `udev_monitor` | In-place update on file-watcher event — preserves runtime enabled flags |
| `set_compositor(Option<String>)` | `udev_monitor` | Record the session compositor; gates `requires_compositor` modules |
| `any_device_matches(evdev_name) -> bool` | `udev_monitor` | Is this evdev node claimed by a base config? |
| `base_configs() -> Vec<Config>` | `udev_monitor` | Declared device targets, includes already merged |
| `window_class_modules() -> Vec<Config>` | `active_client` | Modules that declare a `match_window_class` |
| `resolve(base_name, client, layout) -> Option<Config>` | `EventReader` | Merged active config for the current window |
| `set_enabled(name, bool)` | `EventReader` IPC | Toggle config on/off |
| `snapshot() -> Vec<ConfigEntry>` | `EventReader`, `udev_monitor` | For `SetLoadedConfigs` state update |
| `base_config_error() -> Option<String>` | `udev_monitor` | First hard parse error, for startup diagnostics |

## Usability gate

Every query filters entries through the same predicate: the entry must be `enabled`, must have parsed (`config: Some`), and — if it declares `[module] requires_compositor` — that compositor must be the one recorded via `set_compositor()`. The compositor is unknown at load time, which is why it is set separately once `udev_monitor` has resolved the session environment.

## resolve() logic

`resolve()` is keyed on the **base config's name**, not on a kernel device name. Device matching happens exactly once, in `launch_tasks()`; `EventReader` carries the resulting config name and passes it back on every switch. Re-deriving the match per keystroke would make the outcome depend on whether the filename happens to resemble the `[device] names` entries.

Merge order, lowest priority first:

1. **Plain modules** named in the base config's `[modules] include`. Later entries in the list outrank earlier ones.
2. **The base config** — the usable entry with that name, which must have a `[device]` section.
3. **The conditional module** matching the current window class and layout. Exact window-class match wins; otherwise a layout-only module (`layout != 0`, no `match_window_class`) is used.

Returns `None` when the name identifies no usable base config, or when a layout other than 0 is requested and no module covers it. The latter is deliberate: it lets `change_active_layout()` skip unpopulated layout slots instead of cycling into an empty one.

Two asymmetries are worth knowing about, since both are load-bearing in `with_includes()`:

- `merge_base()` lets `self` win over its argument, so the include stack is built **back to front** — that is what makes later includes outrank earlier ones.
- `merge_base()` treats its argument as the *device-level authority* and copies `gaming_mode_config` from it wholesale. With includes the roles are reversed (plain modules describe no hardware), so the including config's Gaming Mode settings are restored after the merge.

## reload() and enabled flag preservation

`reload()` replaces all entries in-place. It preserves the runtime `enabled` flag only when **both** the old and the new entry have `config: Some` (i.e. both are parseable). This means:

| Transition | `enabled` after reload |
|---|---|
| valid → valid | preserved (user's runtime choice kept) |
| valid → broken | `false` (auto-disabled — broken config cannot be active) |
| broken → fixed | `true` (auto-re-enabled — no manual re-activation needed) |
| broken → broken | `false` (stays disabled) |

## Error configs

Configs that fail to parse:
- Start as `enabled: false`
- Appear in `state.json` `configs[]` with `status: "error"` and `errors[]` populated
- Cannot be activated even via `set_enabled(true)` — every query filters `config: None` entries
- Are automatically re-enabled when the file is fixed and the watcher triggers a reload

## Orphan configs

Dropping the filename convention made a new failure mode reachable: a `.toml` that binds to nothing. `orphan_configs()` runs once after loading and warns for every config that declares no `[device]`, no `[module] match_window_class` and no layout, and that no other config pulls in via `[modules] include`. Such a file parses cleanly and is simply never applied — without the warning that would be silent.

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
  { "name": "broken",       "enabled": false, "status": "error",
    "errors": [{ "severity": "error", "message": "TOML parse error at line 5: ..." }] }
]
```

## Tests

`src/config_registry_tests.rs` covers: `any_device_matches`, `base_configs`, `window_class_modules`, `requires_compositor` gating, `[modules] include` merge order and Gaming Mode preservation, `resolve` (keyed on config name, window-class match, layout-only fallback, empty-layout `None`, disabled entry filtered, no base config), `set_enabled`, `snapshot`, and `base_config_error`.
