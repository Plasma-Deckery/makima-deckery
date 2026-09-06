use deckery_controller::{HapticChain, HapticPulse};
use evdev::Key;
use serde;
use std::{collections::{HashMap, HashSet}, str::FromStr};

// ── Trackpad configuration ────────────────────────────────────────────────────
//
// Config ownership is split between this module ("Core") and the per-mode
// handler module (`mt_trackpad.rs`, `trackball.rs`, `scroll_pad.rs`, ...):
//
//   - Core owns `mode` (which handler gets spawned) and `click_pressure`
//     (a HID feature-report value that lives on the physical sensor itself,
//     independent of whichever handler is active — two handlers can never
//     want different firmware thresholds at the same time), plus router-level
//     settings like `combined_gesture_device`.
//   - Everything else in a `[trackpad.left]`/`[trackpad.right]` table is
//     handler-specific (haptics policy, movement algorithm, gesture
//     semantics) and is passed through as a raw, unparsed `toml::Value` —
//     Core never learns the shape of a handler's config. Each handler module
//     defines its own `#[derive(Deserialize)]` struct and parses itself.

/// Configuration for one trackpad side.
#[derive(Debug, Clone)]
pub struct TrackpadSideConfig {
    /// Which handler to spawn: `"disabled"`, `"mt-trackpad"`, `"trackball"`,
    /// `"scroll"`, ... — an unrecognised value behaves like `"disabled"`
    /// (no handler spawned); see `event_reader::start` for the dispatch.
    pub mode: String,
    /// Physical click pressure threshold sent to firmware via HID.
    /// Raw u16 firmware value. None = firmware default (0xFFFF = disabled).
    pub click_pressure: Option<u16>,
    /// The rest of this side's `[trackpad.left]`/`[trackpad.right]` TOML
    /// table (including `mode`/`click_pressure`, which handlers are free to
    /// ignore), handed unparsed to whichever handler `mode` selects.
    pub handler_config: toml::Value,
    /// Raw `[trackpad.left.kde]` / `[trackpad.right.kde]` sub-table, handed
    /// unparsed to `kde_input_defaults`. Empty table when absent.
    pub kde_config: toml::Value,
}

impl Default for TrackpadSideConfig {
    fn default() -> Self {
        TrackpadSideConfig {
            mode: "disabled".to_string(),
            click_pressure: None,
            handler_config: toml::Value::Table(toml::value::Table::new()),
            kde_config: toml::Value::Table(toml::value::Table::new()),
        }
    }
}

/// Configuration for both trackpad sides, parsed from `[trackpad.left]` /
/// `[trackpad.right]` in the TOML config.
#[derive(Debug, Clone)]
pub struct TrackpadConfig {
    pub left: TrackpadSideConfig,
    pub right: TrackpadSideConfig,
    /// When true, a third virtual MT device ("Deckery Gesture Pad") is created.
    /// As soon as both pads are simultaneously touched, events are routed to that
    /// device with left=slot 0 and right=slot 1 — enabling pinch-zoom, two-finger
    /// scroll and pan via libinput. The gesture session persists until both fingers
    /// are fully lifted.
    pub combined_gesture_device: bool,
    /// Raw `[trackpad.gestures]` TOML sub-table, handed unparsed to
    /// `gesture_pad::GesturePadConfig::from_toml_value`. The combined gesture
    /// device isn't a distinct physical sensor — it has no `mode`/
    /// `click_pressure` of its own — so unlike `left`/`right` there's no
    /// dedicated side-config type, just this raw passthrough.
    pub gesture_handler_config: toml::Value,
    /// Raw `[trackpad.gestures.kde]` sub-table, handed unparsed to
    /// `kde_input_defaults`. Empty table when absent.
    pub gesture_kde_config: toml::Value,
}

impl Default for TrackpadConfig {
    fn default() -> Self {
        TrackpadConfig {
            left: TrackpadSideConfig::default(),
            right: TrackpadSideConfig::default(),
            combined_gesture_device: false,
            gesture_handler_config: toml::Value::Table(toml::value::Table::new()),
            gesture_kde_config: toml::Value::Table(toml::value::Table::new()),
        }
    }
}

// ── Raw deserialization types ─────────────────────────────────────────────────

#[derive(serde::Deserialize, Default, Debug, Clone)]
pub struct RawTrackpadConfig {
    pub left: Option<toml::Value>,
    pub right: Option<toml::Value>,
    pub combined_gesture_device: Option<bool>,
    pub gestures: Option<toml::Value>,
}

fn parse_trackpad_side(raw: Option<&toml::Value>) -> TrackpadSideConfig {
    if let Some(toml::Value::Table(t)) = raw {
        let mode = t.get("mode")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_lowercase())
            .unwrap_or_else(|| "disabled".to_string());
        let click_pressure = t.get("click_pressure")
            .and_then(|v| v.as_integer())
            .map(|i| i as u16);
        let kde_config = t.get("kde")
            .cloned()
            .unwrap_or_else(|| toml::Value::Table(toml::value::Table::new()));
        TrackpadSideConfig {
            mode,
            click_pressure,
            handler_config: toml::Value::Table(t.clone()),
            kde_config,
        }
    } else {
        TrackpadSideConfig::default()
    }
}

/// Value for a `[remap]` entry — simple array or inline table with attributes.
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(untagged)]
pub(crate) enum RemapValue {
    Simple(Vec<Key>),
    WithAttrs { keys: Vec<Key>, #[serde(default)] no_pause: bool, #[serde(default)] while_gaming: bool, #[serde(default)] label: Option<String>, #[serde(default)] silent: bool },
}

/// Value for a `[commands]` entry — simple array or inline table with attributes.
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(untagged)]
pub(crate) enum CommandValue {
    Simple(Vec<String>),
    WithAttrs { run: Vec<String>, #[serde(default)] no_pause: bool, #[serde(default)] while_gaming: bool, #[serde(default)] label: Option<String>, #[serde(default)] silent: bool },
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy)]
pub enum Event {
    Axis(Axis),
    Key(Key),
    Hold,
}

#[allow(non_camel_case_types)]
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy)]
pub enum Axis {
    BTN_DPAD_UP,
    BTN_DPAD_DOWN,
    BTN_DPAD_LEFT,
    BTN_DPAD_RIGHT,
    LSTICK_UP,
    LSTICK_DOWN,
    LSTICK_LEFT,
    LSTICK_RIGHT,
    RSTICK_UP,
    RSTICK_DOWN,
    RSTICK_LEFT,
    RSTICK_RIGHT,
    SCROLL_WHEEL_UP,
    SCROLL_WHEEL_DOWN,
    ABS_Z,
    ABS_RZ,
    ABS_WHEEL_CW,
    ABS_WHEEL_CCW,
}

impl FromStr for Axis {
    type Err = String;
    fn from_str(s: &str) -> Result<Axis, Self::Err> {
        match s {
            "LSTICK_UP" => Ok(Axis::LSTICK_UP),
            "LSTICK_DOWN" => Ok(Axis::LSTICK_DOWN),
            "LSTICK_LEFT" => Ok(Axis::LSTICK_LEFT),
            "LSTICK_RIGHT" => Ok(Axis::LSTICK_RIGHT),
            "RSTICK_UP" => Ok(Axis::RSTICK_UP),
            "RSTICK_DOWN" => Ok(Axis::RSTICK_DOWN),
            "RSTICK_LEFT" => Ok(Axis::RSTICK_LEFT),
            "RSTICK_RIGHT" => Ok(Axis::RSTICK_RIGHT),
            "SCROLL_WHEEL_UP" => Ok(Axis::SCROLL_WHEEL_UP),
            "SCROLL_WHEEL_DOWN" => Ok(Axis::SCROLL_WHEEL_DOWN),
            "ABS_Z" => Ok(Axis::ABS_Z),
            "ABS_RZ" => Ok(Axis::ABS_RZ),
            "ABS_WHEEL_CW" => Ok(Axis::ABS_WHEEL_CW),
            "ABS_WHEEL_CCW" => Ok(Axis::ABS_WHEEL_CCW),
            _ => Err(s.to_string()),
        }
    }
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy)]
pub enum Relative {
    Cursor(Cursor),
    Scroll(Scroll),
}

#[allow(non_camel_case_types)]
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy)]
pub enum Cursor {
    CURSOR_UP,
    CURSOR_DOWN,
    CURSOR_LEFT,
    CURSOR_RIGHT,
}

#[allow(non_camel_case_types)]
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy)]
pub enum Scroll {
    SCROLL_UP,
    SCROLL_DOWN,
    SCROLL_LEFT,
    SCROLL_RIGHT,
}

impl FromStr for Relative {
    type Err = String;
    fn from_str(s: &str) -> Result<Relative, Self::Err> {
        match s {
            "CURSOR_UP" => Ok(Relative::Cursor(Cursor::CURSOR_UP)),
            "CURSOR_DOWN" => Ok(Relative::Cursor(Cursor::CURSOR_DOWN)),
            "CURSOR_LEFT" => Ok(Relative::Cursor(Cursor::CURSOR_LEFT)),
            "CURSOR_RIGHT" => Ok(Relative::Cursor(Cursor::CURSOR_RIGHT)),
            "SCROLL_UP" => Ok(Relative::Scroll(Scroll::SCROLL_UP)),
            "SCROLL_DOWN" => Ok(Relative::Scroll(Scroll::SCROLL_DOWN)),
            "SCROLL_LEFT" => Ok(Relative::Scroll(Scroll::SCROLL_LEFT)),
            "SCROLL_RIGHT" => Ok(Relative::Scroll(Scroll::SCROLL_RIGHT)),
            _ => Err(s.to_string()),
        }
    }
}

/// Which hardware class a base config targets.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceClass {
    HidSteam,
    Evdev,
}

/// Declared hardware target — present only in base configs (those with a `[device]` table).
#[derive(Debug, Clone)]
pub struct DeviceDeclaration {
    pub class: DeviceClass,
    /// Substring-match list: first evdev device whose name contains any of these strings is used.
    pub names: Vec<String>,
}

impl DeviceDeclaration {
    pub fn matches_evdev_name(&self, evdev_name: &str) -> bool {
        self.names.iter().any(|n| evdev_name.contains(n.as_str()))
    }
}

/// Module-level activation conditions. Meaningful for files without a `[device]` section.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModuleMetadata {
    /// Only active when the named compositor is running.
    pub requires_compositor: Option<String>,
    /// Applied when the focused window's class matches this string exactly.
    pub match_window_class: Option<String>,
    /// Applied only while this layout is active. Defaults to layout 0.
    pub layout: u16,
}

#[derive(Default, Debug, Clone)]
pub struct Bindings {
    pub remap: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>>,
    pub commands: HashMap<Event, HashMap<Vec<Event>, Vec<String>>>,
    pub movements: HashMap<Event, HashMap<Vec<Event>, Relative>>,
    /// (trigger, combo) pairs that bypass the pause check.
    /// Set via `no_pause = true` in an inline-table binding.
    pub no_pause: HashSet<(Event, Vec<Event>)>,
    /// (trigger, combo) pairs that fire even when Gaming Mode is active.
    /// Set via `while_gaming = true` in an inline-table binding.
    pub while_gaming: HashSet<(Event, Vec<Event>)>,
    /// Human-readable label per (trigger, combo), exported to the state JSON.
    /// Set via `label = "…"` in an inline-table binding.
    pub labels: HashMap<(Event, Vec<Event>), String>,
    /// Bindings whose outputs are suppressed from active_outputs in the HUD.
    /// Set via `silent = true` in an inline-table binding.
    pub silent: HashSet<(Event, Vec<Event>)>,
    /// Raw `[hints]` entries in source form: TOML key → label.
    ///
    /// Kept unresolved on purpose. A hint written in output space
    /// (`KEY_LEFTCTRL-KEY_UP`) has to be answered by "which button emits this
    /// key", and that answer depends on the *merged* config — a module or an
    /// app override can move `KEY_LEFTCTRL` onto a different button. Resolving
    /// per file would bake in the base config's answer. See `resolve_hints`.
    pub hints: HashMap<String, String>,
    /// Hints resolved to concrete (trigger, combo) button pairs.
    ///
    /// Rebuilt by `resolve_hints` after every merge; never written by the parser.
    /// Display-only — nothing in here ever reaches `resolve_binding`, and no
    /// entry ever lands in `MappedModifiers`.
    pub hints_resolved: HashMap<(Event, Vec<Event>), Hint>,
}

/// A resolved hint: what to show, and where the line was written.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hint {
    pub label: String,
    /// True when the source line lives in the app override rather than the base
    /// config. The merge flattens both into one map, so this is the only record
    /// left — and without it a base-config hint would report the active app as
    /// its origin and render as an app-specific override everywhere.
    pub from_override: bool,
}

#[derive(Default, Debug, Clone)]
pub struct MappedModifiers {
    pub default: Vec<Event>,
    pub custom: Vec<Event>,
    pub all: Vec<Event>,
}

// ── Gaming Mode configuration ─────────────────────────────────────────────────

/// Raw TOML form of the double-click trigger: the key name plus an optional
/// inter-click window. Parsed from a `trigger = { key = "...", ms = N }` table.
#[derive(serde::Deserialize, Debug, Clone)]
pub struct RawDoubleclickTrigger {
    /// Key name (e.g. `"BTN_BASE"`).
    /// `"disabled"` → trigger disabled (explicit opt-out).
    /// Any other unrecognised string → warning + disabled.
    pub key: String,
    /// Maximum milliseconds between two presses to count as a double-click.
    /// Absent → 400.
    pub ms: Option<u64>,
}

/// Parsed double-click trigger, ready for use at runtime.
#[derive(Debug, Clone)]
pub struct DoubleclickTrigger {
    pub key: Key,
    pub ms: u64,
}

/// Raw TOML deserialization type for the `[gaming_mode]` section.
#[derive(serde::Deserialize, Debug, Clone, Default)]
pub struct RawGamingModeConfig {
    /// Double-click trigger configuration.
    /// Absent → default (BTN_BASE, 400 ms).
    /// `trigger = { key = "disabled" }` → trigger disabled.
    pub trigger: Option<RawDoubleclickTrigger>,
    /// Haptic chain fired when Gaming Mode is *enabled*. Falls back to the built-in default.
    pub haptic_on: Option<HapticChain>,
    /// Haptic chain fired when Gaming Mode is *disabled*. Falls back to the built-in default.
    pub haptic_off: Option<HapticChain>,
    /// Automatically enable Gaming Mode when a Steam game is detected running
    /// (process tree walk: focused PID → reaper → steam) or Steam Big Picture
    /// Mode is focused. Default: true. Set to false to disable auto-detection.
    pub auto_detect_steam_games: Option<bool>,
}

/// Parsed Gaming Mode configuration, ready for use at runtime.
#[derive(Debug, Clone)]
pub struct GamingModeConfig {
    /// Double-click trigger. `None` means the trigger is disabled.
    pub trigger: Option<DoubleclickTrigger>,
    /// Haptic chain fired when Gaming Mode is *enabled*.
    pub haptic_on: HapticChain,
    /// Haptic chain fired when Gaming Mode is *disabled*.
    pub haptic_off: HapticChain,
    /// Whether Steam game auto-detection is active. Default: true.
    pub auto_detect_steam_games: bool,
}

impl Default for GamingModeConfig {
    fn default() -> Self {
        // Two staccato pings (8 ms burst, 150 ms apart) for both on and off.
        // On/off use the same feel by default; users can differentiate via TOML.
        let default_chain = HapticChain::Chain(vec![
            deckery_controller::HapticChainStep {
                pulse: HapticPulse { duration_us: 8000, interval_us: 8000, count: 1, gain_db: 0 },
                pause_ms: Some(150),
            },
            deckery_controller::HapticChainStep {
                pulse: HapticPulse { duration_us: 8000, interval_us: 8000, count: 1, gain_db: 0 },
                pause_ms: None,
            },
        ]);
        GamingModeConfig {
            trigger: Some(DoubleclickTrigger { key: Key::BTN_BASE, ms: 400 }),
            haptic_on:  default_chain.clone(),
            haptic_off: default_chain,
            auto_detect_steam_games: true,
        }
    }
}

impl GamingModeConfig {
    fn from_raw(raw: Option<RawGamingModeConfig>, aliases: &HashMap<String, String>) -> Self {
        let Some(raw) = raw else { return Self::default(); };

        // Parse trigger: absent → default, key="disabled" → None, invalid → None + warning.
        let trigger: Option<DoubleclickTrigger> = match raw.trigger {
            None => Self::default().trigger,
            Some(ref t) if t.key == "disabled" => None,
            Some(ref t) => match Key::from_str(resolve_alias(&t.key, aliases)) {
                Ok(key) => Some(DoubleclickTrigger { key, ms: t.ms.unwrap_or(400) }),
                Err(_) => {
                    eprintln!(
                        "[makima] WARNING: unknown gaming_mode trigger key {:?} — trigger disabled",
                        t.key
                    );
                    None
                }
            },
        };

        let default_chain = Self::default();
        let haptic_on  = raw.haptic_on .unwrap_or_else(|| default_chain.haptic_on.clone());
        let haptic_off = raw.haptic_off.unwrap_or(default_chain.haptic_off);

        let auto_detect_steam_games = raw.auto_detect_steam_games.unwrap_or(true);

        GamingModeConfig { trigger, haptic_on, haptic_off, auto_detect_steam_games }
    }
}

// ── Raw TOML types for [device] / [module] / [modules] ───────────────────────

#[derive(serde::Deserialize, Debug, Clone)]
pub struct RawDeviceDeclaration {
    pub class: String,
    pub names: Vec<String>,
    /// Readable names for this hardware's buttons, e.g. `L1 = "BTN_TL"`.
    /// Declared on the base config and applied to every file in the config
    /// directory: button labels belong to the device, not to the program.
    #[serde(default)]
    pub aliases: HashMap<String, String>,
}

#[derive(serde::Deserialize, Debug, Clone, Default)]
pub struct RawModuleMetadata {
    pub requires_compositor: Option<String>,
    pub match_window_class: Option<String>,
    #[serde(default)]
    pub layout: u16,
}

#[derive(serde::Deserialize, Debug, Clone, Default)]
pub struct RawModuleIncludes {
    #[serde(default)]
    pub include: Vec<String>,
}

// ── RawConfig ─────────────────────────────────────────────────────────────────

#[derive(serde::Deserialize, Debug, Clone)]
pub struct RawConfig {
    #[serde(default)]
    pub remap: HashMap<String, RemapValue>,
    #[serde(default)]
    pub commands: HashMap<String, CommandValue>,
    #[serde(default)]
    pub movements: HashMap<String, String>,
    /// Display-only labels for button combinations that are not bindings.
    /// Same `-` separator as `[remap]`, but every segment is a `KEY_*` name —
    /// see `resolve_hints`.
    #[serde(default)]
    pub hints: HashMap<String, String>,
    #[serde(default)]
    pub settings: HashMap<String, String>,
    #[serde(default)]
    pub trackpad: RawTrackpadConfig,
    #[serde(default)]
    pub gaming_mode: Option<RawGamingModeConfig>,
    #[serde(default)]
    pub device: Option<RawDeviceDeclaration>,
    #[serde(default)]
    pub module: RawModuleMetadata,
    #[serde(default)]
    pub modules: RawModuleIncludes,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
    pub bindings: Bindings,
    /// Snapshot of bindings before base was merged in.
    /// None for the base config itself (everything is its own).
    /// Some(overrides) for app-specific configs — used by state_export
    /// to distinguish "came from override" vs "inherited from base".
    pub override_bindings: Option<Bindings>,
    pub settings: HashMap<String, String>,
    pub mapped_modifiers: MappedModifiers,
    /// Trackpad configuration parsed from `[trackpad.left]` / `[trackpad.right]`.
    /// Only meaningful on the base config; app-specific configs inherit from base.
    pub trackpad: TrackpadConfig,
    /// Gaming Mode trigger configuration parsed from `[gaming_mode]`.
    /// Only meaningful on the base config; app-specific configs inherit from base.
    pub gaming_mode_config: GamingModeConfig,
    /// Hardware target declaration — present only when `[device]` is in the TOML.
    /// Identifies this as a base config and specifies which physical device to open.
    pub device: Option<DeviceDeclaration>,
    /// Module-level metadata — activation conditions for non-base configs.
    pub module: ModuleMetadata,
    /// Names of plain modules to merge in, from `[modules] include = [...]`.
    pub module_includes: Vec<String>,
    /// Button aliases in effect while this file was parsed, kept so settings
    /// read later at runtime (`LSTICK_ACTIVATION_MODIFIERS`) resolve the same way.
    pub aliases: HashMap<String, String>,
}

impl Config {
    /// Parse a config file, returning `Err(message)` instead of panicking on
    /// read or TOML errors.
    pub fn try_from_file(
        file: &str,
        file_name: String,
        aliases: &HashMap<String, String>,
    ) -> Result<Self, String> {
        println!("Parsing config file:\n{:?}\n",
            file.rsplit_once('/').map(|(_, f)| f).unwrap_or(file));
        let content = std::fs::read_to_string(file)
            .map_err(|e| format!("Cannot read {:?}: {}", file, e))?;
        let raw: RawConfig = toml::from_str(&content)
            .map_err(|e| format!("TOML error in {:?}: {}", file_name, e))?;
        Ok(Self::from_raw(raw, file_name, aliases))
    }

    /// Read only the `[device] aliases` table from a config file. Used by the
    /// registry to collect the alias map before any file is fully parsed —
    /// bindings cannot be resolved until the device's button names are known.
    pub fn read_aliases(file: &str) -> HashMap<String, String> {
        #[derive(serde::Deserialize)]
        struct Probe { device: Option<RawDeviceDeclaration> }

        std::fs::read_to_string(file).ok()
            .and_then(|c| toml::from_str::<Probe>(&c).ok())
            .and_then(|p| p.device)
            .map(|d| d.aliases)
            .unwrap_or_default()
    }

    /// Merge `base` into `self` and return the result as a new Config.
    /// `self` supplies the overrides; `base` fills everything not overridden.
    /// The returned Config has `override_bindings` set to `self`'s raw bindings
    /// so `state_export` can distinguish inherited from app-specific entries.
    pub fn merged_with_base(&self, base: &Config) -> Self {
        let mut result = self.clone();
        result.merge_base(base);
        result
    }

    /// Build a Config from an already-parsed RawConfig.
    fn from_raw(
        raw_config: RawConfig,
        file_name: String,
        aliases: &HashMap<String, String>,
    ) -> Self {
        let raw_trackpad    = raw_config.trackpad.clone();
        let raw_gaming_mode = raw_config.gaming_mode.clone();
        let raw_device      = raw_config.device.clone();
        let raw_module      = raw_config.module.clone();
        let raw_modules     = raw_config.modules.clone();
        let (bindings, settings, mapped_modifiers) = parse_raw_config(raw_config, aliases);
        let trackpad = TrackpadConfig {
            left: parse_trackpad_side(raw_trackpad.left.as_ref()),
            right: parse_trackpad_side(raw_trackpad.right.as_ref()),
            combined_gesture_device: raw_trackpad.combined_gesture_device.unwrap_or(false),
            gesture_kde_config: raw_trackpad.gestures.as_ref()
                .and_then(|v| v.get("kde"))
                .cloned()
                .unwrap_or_else(|| toml::Value::Table(toml::value::Table::new())),
            gesture_handler_config: raw_trackpad.gestures
                .unwrap_or_else(|| toml::Value::Table(toml::value::Table::new())),
        };
        let gaming_mode_config = GamingModeConfig::from_raw(raw_gaming_mode, aliases);
        let device = raw_device.map(|d| DeviceDeclaration {
            class: if d.class == "hid-steam" { DeviceClass::HidSteam } else { DeviceClass::Evdev },
            names: d.names,
        });
        let module = ModuleMetadata {
            requires_compositor: raw_module.requires_compositor,
            match_window_class: raw_module.match_window_class,
            layout: raw_module.layout,
        };
        Self {
            name: file_name,
            bindings,
            override_bindings: None,
            settings,
            mapped_modifiers,
            trackpad,
            gaming_mode_config,
            device,
            module,
            module_includes: raw_modules.include,
            aliases: aliases.clone(),
        }
    }

    pub fn new_empty(file_name: String) -> Self {
        Self {
            name: file_name,
            bindings: Default::default(),
            override_bindings: None,
            settings: Default::default(),
            mapped_modifiers: Default::default(),
            trackpad: Default::default(),
            gaming_mode_config: Default::default(),
            device: None,
            module: Default::default(),
            module_includes: Vec::new(),
            aliases: Default::default(),
        }
    }

    /// Merge `base` into `self` without overwriting existing entries.
    /// Used for app-specific configs that declare only overrides — all
    /// base bindings not present in the app config are inherited.
    /// Note: remap is checked before commands at event time, so a remap
    /// override silently shadows a base command for the same trigger/combo.
    pub fn merge_base(&mut self, base: &Config) {
        // Snapshot the override-only bindings before merging so state_export
        // can tell which bindings are from this config vs inherited from base.
        self.override_bindings = Some(self.bindings.clone());

        // Base-first strategy: start from a full clone of the base bindings,
        // then apply each override binding on top. When an override defines a
        // binding for a given (trigger, combo), it evicts any base entry of a
        // *different* type for that same combo before inserting — so a
        // command→remap or remap→command type change never leaves a stale
        // entry behind. Adding a new binding type in the future only requires
        // one additional `remove` call per loop, not a new guard matrix.
        let mut merged = base.bindings.clone();

        for (trigger, modifier_map) in &self.bindings.remap {
            for (combo, actions) in modifier_map {
                if let Some(m) = merged.commands.get_mut(trigger)  { m.remove(combo); }
                if let Some(m) = merged.movements.get_mut(trigger) { m.remove(combo); }
                // Label and silent belong to the action: evict base values so a
                // stale description or silence flag never sticks to a replaced action.
                // Override values, if any, are re-added below.
                merged.labels.remove(&(*trigger, combo.clone()));
                merged.silent.remove(&(*trigger, combo.clone()));
                merged.remap.entry(*trigger).or_default().insert(combo.clone(), actions.clone());
            }
        }
        for (trigger, modifier_map) in &self.bindings.commands {
            for (combo, cmds) in modifier_map {
                if let Some(m) = merged.remap.get_mut(trigger)     { m.remove(combo); }
                if let Some(m) = merged.movements.get_mut(trigger) { m.remove(combo); }
                merged.labels.remove(&(*trigger, combo.clone()));
                merged.silent.remove(&(*trigger, combo.clone()));
                merged.commands.entry(*trigger).or_default().insert(combo.clone(), cmds.clone());
            }
        }
        for (trigger, modifier_map) in &self.bindings.movements {
            for (combo, movement) in modifier_map {
                if let Some(m) = merged.remap.get_mut(trigger)    { m.remove(combo); }
                if let Some(m) = merged.commands.get_mut(trigger) { m.remove(combo); }
                merged.labels.remove(&(*trigger, combo.clone()));
                merged.silent.remove(&(*trigger, combo.clone()));
                merged.movements.entry(*trigger).or_default().insert(combo.clone(), *movement);
            }
        }

        // Override no_pause, while_gaming, labels, and silent win; base values are already in merged.
        for entry in &self.bindings.no_pause {
            merged.no_pause.insert(entry.clone());
        }
        for entry in &self.bindings.while_gaming {
            merged.while_gaming.insert(entry.clone());
        }
        for (key, label) in &self.bindings.labels {
            merged.labels.insert(key.clone(), label.clone());
        }
        for entry in &self.bindings.silent {
            merged.silent.insert(entry.clone());
        }
        // Hints merge like labels: same key → override wins. They stay in source
        // form; resolve_hints turns them into buttons once the merge is complete.
        for (key, label) in &self.bindings.hints {
            merged.hints.insert(key.clone(), label.clone());
        }

        self.bindings = merged;

        for key in &base.mapped_modifiers.custom {
            if !self.mapped_modifiers.custom.contains(key) {
                self.mapped_modifiers.custom.push(*key);
            }
        }
        for (key, value) in &base.settings {
            self.settings.entry(key.clone()).or_insert_with(|| value.clone());
        }
        // Trackpad config is device-level; always inherit from base for app configs.
        // App-specific configs should not override hardware settings.
        if self.trackpad.left.mode == "disabled"
            && self.trackpad.right.mode == "disabled"
        {
            self.trackpad = base.trackpad.clone();
        }
        // Gaming Mode config is device-level; always inherit from base.
        self.gaming_mode_config = base.gaming_mode_config.clone();
        self.mapped_modifiers.all.clear();
        self.mapped_modifiers.all.extend(self.mapped_modifiers.default.clone());
        self.mapped_modifiers.all.extend(self.mapped_modifiers.custom.clone());
        self.mapped_modifiers.all.sort();
        self.mapped_modifiers.all.dedup();
    }

    /// Turn the raw `[hints]` entries into concrete (trigger, combo) button
    /// pairs, filling `bindings.hints_resolved`. Returns one message per
    /// problem found, for the caller to surface.
    ///
    /// Must run on the *merged* config: an output-space segment asks "which
    /// button emits this key", and a module or app override can change that
    /// answer. Idempotent — clears and rebuilds.
    ///
    /// Every segment is a `KEY_*` name, resolved through the reverse index to
    /// the buttons that emit it. There is no input-space form: a hint names the
    /// shortcut the application listens for, and the button follows from it.
    ///
    /// A key with no separator resolves to an empty combo. That is the
    /// modifier-less form: it relabels a plain button instead of describing a
    /// combination. Both forms share `hints_resolved`; `state_export` routes
    /// the empty-combo ones into `bindings` and the rest into `modifier_active`.
    ///
    /// Known limit: a combination is only hintable when every part of it sends a
    /// key. `L1-…` cannot be hinted — no application would recognise it anyway;
    /// that case wants a real binding.
    pub fn resolve_hints(&mut self) -> Vec<String> {
        self.bindings.hints_resolved.clear();
        let mut warnings: Vec<String> = Vec::new();
        if self.bindings.hints.is_empty() {
            return warnings;
        }

        // The merge flattens base and override hints into one map, so the only
        // record of where a line came from is the override's own raw section.
        let override_hints = self.override_bindings.as_ref().map(|ov| &ov.hints);

        // Reverse index over base remaps only (combo == []): output key → the
        // buttons that emit it. Combo outputs are excluded on purpose — a hint
        // must land on a button you can press *while* the modifiers are held.
        let mut emitters: HashMap<Key, Vec<Event>> = HashMap::new();
        for (trigger, modifier_map) in &self.bindings.remap {
            let Some(out_keys) = modifier_map.get(&vec![]) else { continue };
            for key in out_keys {
                emitters.entry(*key).or_default().push(*trigger);
            }
        }
        for buttons in emitters.values_mut() {
            buttons.sort();
            buttons.dedup();
        }

        // Hints live in output space only. A hint describes a keyboard shortcut
        // the focused application understands, so every segment names a key —
        // the button is what the lookup *returns*, never what it is given.
        // Naming a button directly (`A`, `BTN_SOUTH`) would skip the lookup and
        // assert a mapping instead of describing one, for a combination no app
        // can ever see. Aliases are button names, so they do not apply here.
        let resolve_segment = |seg: &str| -> Result<Vec<Event>, String> {
            if !seg.starts_with("KEY_") {
                return Err(format!(
                    "{seg:?} is not a key name — hints are written in output space (KEY_*)"));
            }
            let Some(Event::Key(k)) = event_from_name(seg) else {
                return Err(format!("{seg:?} is not a known key"));
            };
            match emitters.get(&k) {
                Some(buttons) => Ok(buttons.clone()),
                None => Err(format!("no button emits {seg:?}")),
            }
        };

        // Which raw line claimed each resolved button, so a second line landing
        // on the same one is reported instead of silently replacing it.
        let mut claimed_by: HashMap<(Event, Vec<Event>), &String> = HashMap::new();

        // Sorted, not HashMap order: two lines can resolve to the same button
        // (`KEY_ENTER` and `A` both reach BTN_SOUTH), and which one wins must
        // not change between runs.
        let mut raw_keys: Vec<&String> = self.bindings.hints.keys().collect();
        raw_keys.sort();

        for raw in raw_keys {
            let label = &self.bindings.hints[raw];
            // No separator means no modifier side: the hint relabels a plain
            // button. That is a deliberate second use of the same syntax —
            // "label without a binding" — not a malformed combo.
            let (mods_str, trigger_str) = raw.rsplit_once('-').unwrap_or(("", raw.as_str()));
            let from_override = override_hints.map_or(false, |h| h.contains_key(raw));

            // Modifier side: build every combination the segments can stand for.
            // Several buttons emitting the same key multiply the combinations.
            let mut combos: Vec<Vec<Event>> = vec![Vec::new()];
            let mut dead_segment = false;
            for seg in mods_str.split('-').filter(|s| !s.is_empty()) {
                let candidates = match resolve_segment(seg) {
                    Ok(c) => c,
                    Err(why) => {
                        warnings.push(format!(
                            "hint {raw:?}: {why} — hint can never be shown"));
                        dead_segment = true;
                        break;
                    }
                };
                if candidates.len() > 1 {
                    warnings.push(format!(
                        "hint {:?}: {:?} maps to {} buttons — shown on all of them",
                        raw, seg, candidates.len()));
                }
                combos = combos.iter().flat_map(|prefix| {
                    candidates.iter().map(move |cand| {
                        let mut next = prefix.clone();
                        next.push(*cand);
                        next
                    })
                }).collect();
            }
            if dead_segment {
                continue;
            }

            let triggers = match resolve_segment(trigger_str) {
                Ok(t) => t,
                Err(why) => {
                    warnings.push(format!("hint {raw:?}: {why} — hint can never be shown"));
                    continue;
                }
            };
            if triggers.len() > 1 {
                warnings.push(format!(
                    "hint {:?}: {:?} maps to {} buttons — shown on all of them",
                    raw, trigger_str, triggers.len()));
            }

            for mut combo in combos {
                combo.sort();
                combo.dedup();
                for trigger in &triggers {
                    // A real binding always wins over a combo hint — a typo in
                    // [hints] must never mask something that actually fires.
                    // Spelled out per map because the three value types differ.
                    //
                    // A modifier-less hint is the exception: it exists *to* sit
                    // on top of a base binding, so the base remap it resolved
                    // through cannot disqualify it. Only a label written out on
                    // the binding itself outranks it.
                    let taken = if combo.is_empty() {
                        self.bindings.labels.contains_key(&(*trigger, Vec::new()))
                    } else {
                        self.bindings.remap.get(trigger).map_or(false, |m| m.contains_key(&combo))
                     || self.bindings.commands.get(trigger).map_or(false, |m| m.contains_key(&combo))
                     || self.bindings.movements.get(trigger).map_or(false, |m| m.contains_key(&combo))
                    };
                    if taken {
                        continue;
                    }
                    // Two lines can reach the same button, because a button can
                    // emit several keys (`Y = ["KEY_SPACE", "KEY_X"]`, both
                    // hinted). Only one label fits, so the loser is dropped —
                    // but loudly: a silently vanished hint looks like a bug in
                    // the resolver, not like a duplicate in the config.
                    let resolved_key = (*trigger, combo.clone());
                    match claimed_by.get(&resolved_key) {
                        Some(owner) if *owner != raw => {
                            warnings.push(format!(
                                "hint {:?}: resolves to the same button as {:?} — only {:?} is shown",
                                raw, owner, owner));
                            continue;
                        }
                        _ => {}
                    }
                    claimed_by.insert(resolved_key.clone(), raw);
                    self.bindings.hints_resolved.insert(
                        resolved_key,
                        Hint { label: label.clone(), from_override },
                    );
                }
            }
        }
        warnings
    }
}

/// Parse a single event name like "BTN_MODE" or "LSTICK_UP" into an Event.
/// Returns None and logs a warning if the name is not recognized.
pub fn parse_event_name(name: &str) -> Option<Event> {
    let event = event_from_name(name);
    if event.is_none() {
        eprintln!("[makima] WARNING: unknown binding name {:?} — skipping (typo in config?)", name);
    }
    event
}

/// Resolve a name to its event without logging. The alias table is checked with
/// this so a broken entry can be reported once by its alias name, instead of
/// once per use site under the substituted value.
pub fn event_from_name(name: &str) -> Option<Event> {
    if let Ok(axis) = Axis::from_str(name) {
        return Some(Event::Axis(axis));
    }
    evdev::Key::from_str(name).ok().map(Event::Key)
}

/// Substitute a device alias (`L1`) for the name it stands for (`BTN_TL`).
/// Unknown names pass through so kernel names keep working alongside aliases.
fn resolve_alias<'a>(name: &'a str, aliases: &'a HashMap<String, String>) -> &'a str {
    aliases.get(name).map(String::as_str).unwrap_or(name)
}

/// Parse a binding input string (e.g. "BTN_TL-BTN_SOUTH", "L1-A") into the
/// trigger event and its modifier list. Registers any new custom modifiers.
/// Returns (None, _) if the trigger event name is unrecognized.
fn parse_binding_input(
    input: &str,
    mapped_modifiers: &mut MappedModifiers,
    aliases: &HashMap<String, String>,
) -> (Option<Event>, Vec<Event>) {
    if let Some((mods_str, event_str)) = input.rsplit_once('-') {
        let str_modifiers: Vec<&str> = mods_str.split('-').collect();
        let mut modifiers: Vec<Event> = Vec::new();
        for m in str_modifiers.iter().filter(|&&m| !m.is_empty()) {
            match parse_event_name(resolve_alias(m, aliases)) {
                Some(event) => modifiers.push(event),
                // Dropping just the modifier would leave the binding firing
                // unmodified, so `L1-A` would quietly hijack a bare A press.
                // A combo whose modifier is unknown is no binding at all.
                None => return (None, Vec::new()),
            }
        }
        modifiers.sort();
        modifiers.dedup();
        for modifier in &modifiers {
            if !mapped_modifiers.default.contains(modifier) {
                mapped_modifiers.custom.push(modifier.clone());
            }
        }
        if str_modifiers.first().map(|s| s.is_empty()).unwrap_or(false) {
            modifiers.push(Event::Hold);
        }
        (parse_event_name(resolve_alias(event_str, aliases)), modifiers)
    } else {
        (parse_event_name(resolve_alias(input, aliases)), Vec::new())
    }
}

fn parse_raw_config(
    raw_config: RawConfig,
    aliases: &HashMap<String, String>,
) -> (Bindings, HashMap<String, String>, MappedModifiers) {
    let remap: HashMap<String, RemapValue> = raw_config.remap;
    let commands: HashMap<String, CommandValue> = raw_config.commands;
    let movements: HashMap<String, String> = raw_config.movements;
    let settings: HashMap<String, String> = raw_config.settings;
    let mut bindings: Bindings = Default::default();
    let default_modifiers = vec![
        Event::Key(Key::KEY_LEFTSHIFT),
        Event::Key(Key::KEY_LEFTCTRL),
        Event::Key(Key::KEY_LEFTALT),
        Event::Key(Key::KEY_RIGHTSHIFT),
        Event::Key(Key::KEY_RIGHTCTRL),
        Event::Key(Key::KEY_RIGHTALT),
        Event::Key(Key::KEY_LEFTMETA),
    ];
    let mut mapped_modifiers = MappedModifiers {
        default: default_modifiers,
        custom: Vec::new(),
        all: Vec::new(),
    };
    let custom_modifiers: Vec<Event> = parse_modifiers(&settings, "CUSTOM_MODIFIERS", aliases);
    let lstick_activation_modifiers: Vec<Event> =
        parse_modifiers(&settings, "LSTICK_ACTIVATION_MODIFIERS", aliases);
    let rstick_activation_modifiers: Vec<Event> =
        parse_modifiers(&settings, "RSTICK_ACTIVATION_MODIFIERS", aliases);

    mapped_modifiers.custom.extend(custom_modifiers);
    mapped_modifiers.custom.extend(lstick_activation_modifiers);
    mapped_modifiers.custom.extend(rstick_activation_modifiers);

    for (input, value) in remap {
        let (output, np, wg, lbl, sl) = match value {
            RemapValue::Simple(keys) => (keys, false, false, None, false),
            RemapValue::WithAttrs { keys, no_pause, while_gaming, label, silent } => (keys, no_pause, while_gaming, label, silent),
        };
        let (Some(evt), modifiers) = parse_binding_input(&input, &mut mapped_modifiers, aliases) else { continue; };
        if np { bindings.no_pause.insert((evt, modifiers.clone())); }
        if wg { bindings.while_gaming.insert((evt, modifiers.clone())); }
        if let Some(l) = lbl { bindings.labels.insert((evt, modifiers.clone()), l); }
        if sl { bindings.silent.insert((evt, modifiers.clone())); }
        bindings.remap.entry(evt).or_default().insert(modifiers, output);
    }

    for (input, value) in commands {
        let (output, np, wg, lbl, sl) = match value {
            CommandValue::Simple(cmds) => (cmds, false, false, None, false),
            CommandValue::WithAttrs { run, no_pause, while_gaming, label, silent } => (run, no_pause, while_gaming, label, silent),
        };
        let (Some(evt), modifiers) = parse_binding_input(&input, &mut mapped_modifiers, aliases) else { continue; };
        if np { bindings.no_pause.insert((evt, modifiers.clone())); }
        if wg { bindings.while_gaming.insert((evt, modifiers.clone())); }
        if let Some(l) = lbl { bindings.labels.insert((evt, modifiers.clone()), l); }
        if sl { bindings.silent.insert((evt, modifiers.clone())); }
        bindings.commands.entry(evt).or_default().insert(modifiers, output);
    }

    for (input, output) in movements {
        let rel = Relative::from_str(output.as_str()).expect("Invalid movement in [movements].");
        let (Some(evt), modifiers) = parse_binding_input(&input, &mut mapped_modifiers, aliases) else { continue; };
        bindings.movements.entry(evt).or_default().insert(modifiers, rel);
    }

    // Hints are stored verbatim and deliberately do NOT go through
    // parse_binding_input: that function registers every modifier it sees in
    // mapped_modifiers.custom, which would turn a hinted button into a real
    // layer key — the exact breakage hints exist to avoid (see docs/hints.md).
    bindings.hints = raw_config.hints;

    mapped_modifiers.custom.sort();
    mapped_modifiers.custom.dedup();
    mapped_modifiers
        .all
        .extend(mapped_modifiers.default.clone());
    mapped_modifiers.all.extend(mapped_modifiers.custom.clone());
    mapped_modifiers.all.sort();
    mapped_modifiers.all.dedup();

    (bindings, settings, mapped_modifiers)
}

pub fn parse_modifiers(
    settings: &HashMap<String, String>,
    parameter: &str,
    aliases: &HashMap<String, String>,
) -> Vec<Event> {
    match settings.get(parameter) {
        // Empty segments are how an intentionally empty list is written
        // (`CUSTOM_MODIFIERS = ""`); parse_binding_input drops them too.
        Some(modifiers) => modifiers
            .split('-')
            .filter(|m| !m.is_empty())
            .filter_map(|m| parse_event_name(resolve_alias(m, aliases)))
            .collect(),
        None => Vec::new(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────


#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
