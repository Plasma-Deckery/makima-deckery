# Config Registry

The `ConfigRegistry` (`src/config_registry.rs`) is the single source of truth for all loaded configuration files. It replaced the old `Vec<Config>` approach that was passed through `udev_monitor` and duplicated between modules.

## Why it exists

Previously, config data existed in two forms simultaneously:
- `main.rs` read all files into `Vec<Config>`
- `udev_monitor` parsed associations from filenames, merged base into app configs, and built a per-device `Vec<Config>` that was passed to `EventReader`
- `EventReader` stored an `Arc<Mutex<HashMap<String, ConfigEntry>>>` internally

This dual source of truth made runtime enable/disable of individual configs impossible without a full reload. Configs were stored **pre-merged** (base baked into every app config at load time), so disabling just one app config required rebuilding the entire merged set.

## Architecture

```
main.rs
  → ConfigRegistry::load(config_dir)     // read disk, parse, validate
  → Arc<ConfigRegistry>                  // shared via clone — one instance
  → udev_monitor::start_monitoring_udev(registry, ..., ipc_tx)
      → launch_tasks(registry, ...)
          → registry.device_has_configs()
          → registry.base_config()         // for synchronous EventReader setup
          → EventReader { registry, device_name, ... }
              → registry.resolve()         // on every config switch
              → registry.set_enabled()     // on IPC enable/disable
              → registry.snapshot()        // for SetLoadedConfigs state update
```

Configs are stored **unmerged** — exactly as they appear on disk. `resolve()` merges base + app override at the point of use. Enabling or disabling a config takes effect on the next key press without any reload.

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

Files that fail to parse are stored with `config: None`. They appear in `state.json` so the tray can show them as broken, but `resolve()` never returns them even if `enabled = true`.

## Key methods

| Method | Used by | Purpose |
|---|---|---|
| `load(config_dir) -> Arc<Self>` | `main.rs` | Create registry from disk at startup |
| `reload(config_dir)` | `udev_monitor` | In-place update on file-watcher event — preserves runtime enabled flags |
| `device_has_configs(name) -> bool` | `udev_monitor` | Device matching |
| `base_config(device_name) -> Option<Config>` | `udev_monitor` | Synchronous setup in `EventReader::new()` |
| `resolve(device, client, layout) -> Option<Config>` | `EventReader` | Merged active config for the current window |
| `set_enabled(name, bool)` | `EventReader` IPC | Toggle config on/off |
| `snapshot() -> Vec<ConfigEntry>` | `EventReader`, `udev_monitor` | For `SetLoadedConfigs` state update |

## resolve() logic

1. Find the enabled base config (`name == device_name`, no `"::"` in name)
2. Find the best matching app config (exact: layout + window class > layout-only fallback)
3. Return `app.merged_with_base(&base)` if a match exists, otherwise `base.clone()`
4. Return `None` only if no valid base config exists

A disabled entry or one with `config: None` is never returned.

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
- Cannot be activated even via `set_enabled(true)` — `resolve()` filters `config: None` entries
- Are automatically re-enabled when the file is fixed and the watcher triggers a reload

## IPC socket architecture

The IPC socket (`/tmp/makima-control.sock`) is bound once in `main.rs` and broadcast via `tokio::sync::broadcast::Sender<String>` to all active `EventReader` instances. Each reader has its own `ipc_command_loop(rx)` that reads from the broadcast receiver.

This replaces the old design where `EventReader` bound the socket itself — which caused a silent socket takeover bug in multi-device setups (the second reader stole the socket from the first, leaving the first device unable to receive IPC commands).

## state.json output

```json
"configs": [
  { "name": "Steam Deck",                     "enabled": true,  "status": "ok",    "errors": [] },
  { "name": "Steam Deck::org.mozilla.firefox", "enabled": true,  "status": "ok",    "errors": [] },
  { "name": "Steam Deck::broken",              "enabled": false, "status": "error",
    "errors": [{ "severity": "error", "message": "TOML parse error at line 5: ..." }] }
]
```

## Tests

`src/config_registry_tests.rs` covers: `parse_associations`, `resolve` (exact match, layout-only fallback, no-match fallback, disabled entry filtered), `device_has_configs`, `set_enabled`, `snapshot`.
