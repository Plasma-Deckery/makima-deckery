use deckery_controller::{HapticChain, HapticPulse};
use crate::udev_monitor::Client;
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

#[derive(Debug, PartialEq, Eq, Default, Clone)]
pub struct Associations {
    pub client: Client,
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
    fn from_raw(raw: Option<RawGamingModeConfig>) -> Self {
        let Some(raw) = raw else { return Self::default(); };

        // Parse trigger: absent → default, key="disabled" → None, invalid → None + warning.
        let trigger: Option<DoubleclickTrigger> = match raw.trigger {
            None => Self::default().trigger,
            Some(ref t) if t.key == "disabled" => None,
            Some(ref t) => match Key::from_str(&t.key) {
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

#[derive(serde::Deserialize, Debug, Clone)]
pub struct RawConfig {
    #[serde(default)]
    pub remap: HashMap<String, RemapValue>,
    #[serde(default)]
    pub commands: HashMap<String, CommandValue>,
    #[serde(default)]
    pub movements: HashMap<String, String>,
    #[serde(default)]
    pub settings: HashMap<String, String>,
    #[serde(default)]
    pub trackpad: RawTrackpadConfig,
    #[serde(default)]
    pub gaming_mode: Option<RawGamingModeConfig>,
}

impl RawConfig {
    fn new_from_file(file: &str) -> Self {
        println!(
            "Parsing config file:\n{:?}\n",
            file.rsplit_once("/").unwrap().1
        );
        let file_content: String = std::fs::read_to_string(file).unwrap();
        let raw_config: RawConfig =
            toml::from_str(&file_content).expect("Couldn't parse config file.");
        let remap = raw_config.remap;
        let commands = raw_config.commands;
        let movements = raw_config.movements;
        let settings = raw_config.settings;
        let trackpad = raw_config.trackpad;
        let gaming_mode = raw_config.gaming_mode;
        Self {
            remap,
            commands,
            movements,
            settings,
            trackpad,
            gaming_mode,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
    pub associations: Associations,
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
}

impl Config {
    /// Parse a config file, returning `Err(message)` instead of panicking on
    /// read or TOML errors.  Associations are left at default — the caller
    /// (ConfigRegistry::load) sets them from the filename.
    pub fn try_from_file(file: &str, file_name: String) -> Result<Self, String> {
        println!("Parsing config file:\n{:?}\n",
            file.rsplit_once('/').map(|(_, f)| f).unwrap_or(file));
        let content = std::fs::read_to_string(file)
            .map_err(|e| format!("Cannot read {:?}: {}", file, e))?;
        let raw: RawConfig = toml::from_str(&content)
            .map_err(|e| format!("TOML error in {:?}: {}", file_name, e))?;
        Ok(Self::from_raw(raw, file_name))
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
    /// Associations are left at default — set them separately after construction.
    fn from_raw(raw_config: RawConfig, file_name: String) -> Self {
        let raw_trackpad    = raw_config.trackpad.clone();
        let raw_gaming_mode = raw_config.gaming_mode.clone();
        let (bindings, settings, mapped_modifiers) = parse_raw_config(raw_config);
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
        let gaming_mode_config = GamingModeConfig::from_raw(raw_gaming_mode);
        Self {
            name: file_name,
            associations: Default::default(),
            bindings,
            override_bindings: None,
            settings,
            mapped_modifiers,
            trackpad,
            gaming_mode_config,
        }
    }

    pub fn new_from_file(file: &str, file_name: String) -> Self {
        let raw_config = RawConfig::new_from_file(file);
        Self::from_raw(raw_config, file_name)
    }

    pub fn new_empty(file_name: String) -> Self {
        Self {
            name: file_name,
            associations: Default::default(),
            bindings: Default::default(),
            override_bindings: None,
            settings: Default::default(),
            mapped_modifiers: Default::default(),
            trackpad: Default::default(),
            gaming_mode_config: Default::default(),
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
}

/// Parse a single event name like "BTN_MODE" or "LSTICK_UP" into an Event.
/// Returns None and logs a warning if the name is not recognized.
pub fn parse_event_name(name: &str) -> Option<Event> {
    if let Ok(axis) = Axis::from_str(name) {
        return Some(Event::Axis(axis));
    }
    if let Ok(key) = evdev::Key::from_str(name) {
        return Some(Event::Key(key));
    }
    eprintln!("[makima] WARNING: unknown binding name {:?} — skipping (typo in config?)", name);
    None
}

/// Parse a binding input string (e.g. "BTN_TL-BTN_SOUTH" or "BTN_EAST") into
/// the trigger event and its modifier list. Registers any new custom modifiers.
/// Returns (None, _) if the trigger event name is unrecognized.
fn parse_binding_input(input: &str, mapped_modifiers: &mut MappedModifiers) -> (Option<Event>, Vec<Event>) {
    if let Some((mods_str, event_str)) = input.rsplit_once('-') {
        let str_modifiers: Vec<&str> = mods_str.split('-').collect();
        let mut modifiers: Vec<Event> = str_modifiers.iter()
            .filter(|&&m| !m.is_empty())
            .filter_map(|m| parse_event_name(m))
            .collect();
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
        (parse_event_name(event_str), modifiers)
    } else {
        (parse_event_name(input), Vec::new())
    }
}

fn parse_raw_config(raw_config: RawConfig) -> (Bindings, HashMap<String, String>, MappedModifiers) {
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
    let custom_modifiers: Vec<Event> = parse_modifiers(&settings, "CUSTOM_MODIFIERS");
    let lstick_activation_modifiers: Vec<Event> =
        parse_modifiers(&settings, "LSTICK_ACTIVATION_MODIFIERS");
    let rstick_activation_modifiers: Vec<Event> =
        parse_modifiers(&settings, "RSTICK_ACTIVATION_MODIFIERS");

    mapped_modifiers.custom.extend(custom_modifiers);
    mapped_modifiers.custom.extend(lstick_activation_modifiers);
    mapped_modifiers.custom.extend(rstick_activation_modifiers);

    for (input, value) in remap {
        let (output, np, wg, lbl, sl) = match value {
            RemapValue::Simple(keys) => (keys, false, false, None, false),
            RemapValue::WithAttrs { keys, no_pause, while_gaming, label, silent } => (keys, no_pause, while_gaming, label, silent),
        };
        let (Some(evt), modifiers) = parse_binding_input(&input, &mut mapped_modifiers) else { continue; };
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
        let (Some(evt), modifiers) = parse_binding_input(&input, &mut mapped_modifiers) else { continue; };
        if np { bindings.no_pause.insert((evt, modifiers.clone())); }
        if wg { bindings.while_gaming.insert((evt, modifiers.clone())); }
        if let Some(l) = lbl { bindings.labels.insert((evt, modifiers.clone()), l); }
        if sl { bindings.silent.insert((evt, modifiers.clone())); }
        bindings.commands.entry(evt).or_default().insert(modifiers, output);
    }

    for (input, output) in movements {
        let rel = Relative::from_str(output.as_str()).expect("Invalid movement in [movements].");
        let (Some(evt), modifiers) = parse_binding_input(&input, &mut mapped_modifiers) else { continue; };
        bindings.movements.entry(evt).or_default().insert(modifiers, rel);
    }

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

pub fn parse_modifiers(settings: &HashMap<String, String>, parameter: &str) -> Vec<Event> {
    match settings.get(parameter) {
        Some(modifiers) => modifiers.split('-').filter_map(|m| parse_event_name(m)).collect(),
        None => Vec::new(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────


#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
