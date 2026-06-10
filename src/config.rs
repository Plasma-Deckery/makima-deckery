use crate::udev_monitor::Client;
use evdev::Key;
use serde;
use std::{collections::{HashMap, HashSet}, str::FromStr};

// ── Trackpad configuration ────────────────────────────────────────────────────

/// Which role a trackpad plays — determines both what makima does with the
/// raw ABS_HAT events and what firmware mode the hid-steam driver is set to.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum TrackpadMode {
    /// No virtual MT device created; firmware set to TRACKPAD_NONE.
    /// Default when no `[trackpad]` section is present.
    #[default]
    Disabled,
    /// Virtual uinput MT device created and fed raw position events.
    /// Firmware set to TRACKPAD_NONE (no kernel cursor emulation).
    /// Config value: `"mt-trackpad"`.
    MtTrackpad,
    /// No MT device; firmware set to TRACKPAD_RELATIVE_MOUSE.
    /// The kernel driver handles cursor movement with trackball physics.
    /// Config value: `"trackball"`.
    Trackball,
}

impl TrackpadMode {
    fn from_str_opt(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "disabled"    => Some(TrackpadMode::Disabled),
            "mt-trackpad" => Some(TrackpadMode::MtTrackpad),
            "trackball"   => Some(TrackpadMode::Trackball),
            _             => None,
        }
    }
}

/// Configuration for one trackpad side.
#[derive(Debug, Clone)]
pub struct TrackpadSideConfig {
    pub mode: TrackpadMode,
    /// Physical click pressure threshold sent to firmware via HID.
    /// Raw u16 firmware value. None = firmware default (0xFFFF = disabled).
    pub click_pressure: Option<u16>,
    // Mouse-mode parameters (firmware TRACKPAD_RELATIVE_MOUSE).
    // Parsed and stored; firmware application planned.
    pub momentum_decay: Option<u16>,
    pub min_velocity: Option<u16>,
    pub max_velocity: Option<u16>,
    pub outer_radius: Option<u16>,
}

impl Default for TrackpadSideConfig {
    fn default() -> Self {
        TrackpadSideConfig {
            mode: TrackpadMode::Disabled,
            click_pressure: None,
            momentum_decay: None,
            min_velocity: None,
            max_velocity: None,
            outer_radius: None,
        }
    }
}

/// Configuration for both trackpad sides, parsed from `[trackpad.left]` /
/// `[trackpad.right]` in the TOML config.
#[derive(Debug, Clone, Default)]
pub struct TrackpadConfig {
    pub left: TrackpadSideConfig,
    pub right: TrackpadSideConfig,
    /// When true, a third virtual MT device ("Deckery Gesture Pad") is created.
    /// As soon as both pads are simultaneously touched, events are routed to that
    /// device with left=slot 0 and right=slot 1 — enabling pinch-zoom, two-finger
    /// scroll and pan via libinput. The gesture session persists until both fingers
    /// are fully lifted.
    pub combined_gesture_device: bool,
}

// ── Raw deserialization types ─────────────────────────────────────────────────

#[derive(serde::Deserialize, Default, Debug, Clone)]
pub struct RawTrackpadSideConfig {
    pub mode: Option<String>,
    pub click_pressure: Option<u16>,
    pub momentum_decay: Option<u16>,
    pub min_velocity: Option<u16>,
    pub max_velocity: Option<u16>,
    pub outer_radius: Option<u16>,
}

#[derive(serde::Deserialize, Default, Debug, Clone)]
pub struct RawTrackpadConfig {
    pub left: Option<RawTrackpadSideConfig>,
    pub right: Option<RawTrackpadSideConfig>,
    pub combined_gesture_device: Option<bool>,
}

fn parse_trackpad_side(raw: Option<&RawTrackpadSideConfig>, legacy_setting: Option<&String>) -> TrackpadSideConfig {
    if let Some(r) = raw {
        let mode = r.mode.as_deref()
            .and_then(TrackpadMode::from_str_opt)
            .unwrap_or_default();
        if r.mode.is_some() && mode == TrackpadMode::Disabled
            && r.mode.as_deref() != Some("disabled") {
            eprintln!("Warning: unrecognised trackpad mode {:?}, defaulting to \"disabled\".", r.mode);
        }
        TrackpadSideConfig {
            mode,
            click_pressure: r.click_pressure,
            momentum_decay: r.momentum_decay,
            min_velocity: r.min_velocity,
            max_velocity: r.max_velocity,
            outer_radius: r.outer_radius,
        }
    } else if let Some(s) = legacy_setting {
        // Backward-compat: LPAD/RPAD = "trackpad" / "disabled"
        let mode = TrackpadMode::from_str_opt(s.as_str()).unwrap_or_default();
        TrackpadSideConfig { mode, ..Default::default() }
    } else {
        TrackpadSideConfig::default()
    }
}

/// Value for a `[remap]` entry — simple array or inline table with attributes.
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(untagged)]
enum RemapValue {
    Simple(Vec<Key>),
    WithAttrs { keys: Vec<Key>, #[serde(default)] no_pause: bool, #[serde(default)] label: Option<String>, #[serde(default)] silent: bool },
}

/// Value for a `[commands]` entry — simple array or inline table with attributes.
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(untagged)]
enum CommandValue {
    Simple(Vec<String>),
    WithAttrs { run: Vec<String>, #[serde(default)] no_pause: bool, #[serde(default)] label: Option<String>, #[serde(default)] silent: bool },
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
        println!("  [debug] raw trackpad after toml parse: {:?}", raw_config.trackpad);
        let remap = raw_config.remap;
        let commands = raw_config.commands;
        let movements = raw_config.movements;
        let settings = raw_config.settings;
        let trackpad = raw_config.trackpad;
        Self {
            remap,
            commands,
            movements,
            settings,
            trackpad,
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
    /// Falls back to legacy `LPAD`/`RPAD` settings when `[trackpad]` is absent.
    /// Only meaningful on the base config; app-specific configs inherit from base.
    pub trackpad: TrackpadConfig,
}

impl Config {
    pub fn new_from_file(file: &str, file_name: String) -> Self {
        let raw_config = RawConfig::new_from_file(file);
        let raw_trackpad = raw_config.trackpad.clone();
        let (bindings, settings, mapped_modifiers) = parse_raw_config(raw_config);
        let associations = Default::default();

        // Parse [trackpad] section; fall back to legacy LPAD/RPAD settings.
        let trackpad = TrackpadConfig {
            left: parse_trackpad_side(
                raw_trackpad.left.as_ref(),
                settings.get("LPAD"),
            ),
            right: parse_trackpad_side(
                raw_trackpad.right.as_ref(),
                settings.get("RPAD"),
            ),
            combined_gesture_device: raw_trackpad.combined_gesture_device.unwrap_or(false),
        };

        Self {
            name: file_name,
            associations,
            bindings,
            override_bindings: None,
            settings,
            mapped_modifiers,
            trackpad,
        }
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

        // Override no_pause, labels, and silent win; base values are already in merged.
        for entry in &self.bindings.no_pause {
            merged.no_pause.insert(entry.clone());
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
        if self.trackpad.left.mode == TrackpadMode::Disabled
            && self.trackpad.right.mode == TrackpadMode::Disabled
        {
            self.trackpad = base.trackpad.clone();
        }
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
        let (output, np, lbl, sl) = match value {
            RemapValue::Simple(keys) => (keys, false, None, false),
            RemapValue::WithAttrs { keys, no_pause, label, silent } => (keys, no_pause, label, silent),
        };
        let (Some(evt), modifiers) = parse_binding_input(&input, &mut mapped_modifiers) else { continue; };
        if np { bindings.no_pause.insert((evt, modifiers.clone())); }
        if let Some(l) = lbl { bindings.labels.insert((evt, modifiers.clone()), l); }
        if sl { bindings.silent.insert((evt, modifiers.clone())); }
        bindings.remap.entry(evt).or_default().insert(modifiers, output);
    }

    for (input, value) in commands {
        let (output, np, lbl, sl) = match value {
            CommandValue::Simple(cmds) => (cmds, false, None, false),
            CommandValue::WithAttrs { run, no_pause, label, silent } => (run, no_pause, label, silent),
        };
        let (Some(evt), modifiers) = parse_binding_input(&input, &mut mapped_modifiers) else { continue; };
        if np { bindings.no_pause.insert((evt, modifiers.clone())); }
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
mod tests {
    use super::*;
    use evdev::Key;

    fn key(k: Key) -> Event { Event::Key(k) }

    fn make_remap_bindings(entries: Vec<(Event, Vec<Event>, Vec<Key>)>) -> Bindings {
        let mut remap: HashMap<Event, HashMap<Vec<Event>, Vec<Key>>> = HashMap::new();
        for (trigger, combo, keys) in entries {
            remap.entry(trigger).or_default().insert(combo, keys);
        }
        Bindings { remap, ..Default::default() }
    }

    fn make_command_bindings(entries: Vec<(Event, Vec<Event>, Vec<String>)>) -> Bindings {
        let mut commands: HashMap<Event, HashMap<Vec<Event>, Vec<String>>> = HashMap::new();
        for (trigger, combo, cmds) in entries {
            commands.entry(trigger).or_default().insert(combo, cmds);
        }
        Bindings { commands, ..Default::default() }
    }

    fn config_with(name: &str, bindings: Bindings) -> Config {
        Config {
            name: name.to_string(),
            associations: Default::default(),
            bindings,
            override_bindings: None,
            settings: HashMap::new(),
            mapped_modifiers: Default::default(),
        }
    }

    // ── merge_base: cross-type override prevention ────────────────────────────

    /// Firefox overrides BTN_TL-BTN_DPAD_UP from command (base) → remap (override).
    /// After merge, the base command must NOT appear — only the override remap.
    #[test]
    fn merge_base_command_to_remap_override() {
        let btn_tl = key(Key::BTN_TL);
        let btn_up = key(Key::BTN_DPAD_UP);
        let combo  = vec![btn_tl];

        let base = config_with("Steam Deck", make_command_bindings(vec![
            (btn_up, combo.clone(), vec!["previous-desktop".to_string()]),
        ]));
        let mut app = config_with("firefox", make_remap_bindings(vec![
            (btn_up, combo.clone(), vec![Key::KEY_LEFTCTRL, Key::KEY_R]),
        ]));

        app.merge_base(&base);

        assert!(
            app.bindings.remap.get(&btn_up).and_then(|m| m.get(&combo)).is_some(),
            "override remap must survive merge"
        );
        assert!(
            app.bindings.commands.get(&btn_up).and_then(|m| m.get(&combo)).is_none(),
            "base command must not leak when override defines a remap for the same combo"
        );
    }

    /// Symmetric: override defines a command where base had a remap.
    #[test]
    fn merge_base_remap_to_command_override() {
        let btn_tl = key(Key::BTN_TL);
        let btn_up = key(Key::BTN_DPAD_UP);
        let combo  = vec![btn_tl];

        let base = config_with("Steam Deck", make_remap_bindings(vec![
            (btn_up, combo.clone(), vec![Key::KEY_UP]),
        ]));
        let mut app = config_with("myapp", make_command_bindings(vec![
            (btn_up, combo.clone(), vec!["do-something".to_string()]),
        ]));

        app.merge_base(&base);

        assert!(
            app.bindings.commands.get(&btn_up).and_then(|m| m.get(&combo)).is_some(),
            "override command must survive merge"
        );
        assert!(
            app.bindings.remap.get(&btn_up).and_then(|m| m.get(&combo)).is_none(),
            "base remap must not leak when override defines a command for the same combo"
        );
    }

    /// parse_event_name returns Some for valid names and None for unknown ones.
    #[test]
    fn parse_event_name_known_and_unknown() {
        assert!(parse_event_name("BTN_TL").is_some(), "BTN_TL must be recognised");
        assert!(parse_event_name("BTN_GRIPR2").is_some(), "BTN_GRIPR2 (Steam Deck back paddle) must be recognised");
        assert!(parse_event_name("BTN_TOTALLY_FAKE").is_none(), "unknown name must return None");
        assert!(parse_event_name("").is_none(), "empty string must return None");
    }

    /// Unrelated combos from the base must still be inherited normally.
    #[test]
    fn merge_base_unrelated_combos_inherited() {
        let btn_tl   = key(Key::BTN_TL);
        let btn_up   = key(Key::BTN_DPAD_UP);
        let btn_down = key(Key::BTN_DPAD_DOWN);

        let base = config_with("Steam Deck", make_command_bindings(vec![
            (btn_up,   vec![btn_tl], vec!["prev-desktop".to_string()]),
            (btn_down, vec![btn_tl], vec!["next-desktop".to_string()]),
        ]));
        // Override only changes btn_up; btn_down must be inherited unchanged.
        let mut app = config_with("firefox", make_remap_bindings(vec![
            (btn_up, vec![btn_tl], vec![Key::KEY_LEFTCTRL, Key::KEY_R]),
        ]));

        app.merge_base(&base);

        assert!(
            app.bindings.commands
                .get(&btn_down).and_then(|m| m.get(&vec![btn_tl])).is_some(),
            "unrelated base command (btn_down) must be inherited"
        );
    }
}
