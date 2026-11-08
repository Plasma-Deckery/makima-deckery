//! The typed form of a config's `[settings]` section.
//!
//! `Config.settings` is a `HashMap<String, String>` — whatever the TOML said,
//! unexamined. Turning those strings into numbers, flags and modifier lists used
//! to happen in `EventReader::new`, which meant two things worth fixing.
//!
//! The parsing was in the wrong layer: a config is supposed to be a config by
//! the time it leaves the config code, and eighteen of its values were still
//! strings. And the defaults were in the wrong layer with it, so what a value
//! falls back to was knowledge the config layer did not have.
//!
//! The second thing mattered more. Every one of those parses was an `.expect`,
//! so a mistyped number did not produce a warning — it panicked the daemon at
//! construction. Every other kind of config mistake is reported as a ⚠ in the
//! tray with the shipped file kept in effect. These eighteen were the exception,
//! and for no reason other than where the code sat.
//!
//! Now a bad value is a message: the default is used and the problem is
//! returned alongside, for the caller to report the way it reports every other
//! config error.

use crate::config::{parse_modifiers, Config, Event};

/// What one stick is for.
#[derive(Debug, Clone, PartialEq)]
pub struct Stick {
    /// `"cursor"`, `"scroll"`, `"bind"`, or `"disabled"`.
    pub function: String,
    /// Milliseconds between ticks of the cursor/scroll loop. Zero switches the
    /// loop off entirely rather than running it at full speed.
    pub sensitivity: u64,
    pub deadzone: i32,
    /// The stick only acts while exactly these are held. Empty means always.
    pub activation_modifiers: Vec<Event>,
}

/// A key-driven cursor or scroll ramp.
#[derive(Debug, Clone, PartialEq)]
pub struct Movement {
    /// Zero switches the loop off.
    pub speed: i32,
    pub acceleration: f32,
}

/// Everything `[settings]` says, typed.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub lstick: Stick,
    pub rstick: Stick,
    pub invert_cursor_axis: bool,
    pub invert_scroll_axis: bool,
    /// The controller reports 16-bit axes. An 8-bit one is centred at 128
    /// instead of 0, so the same raw number means the opposite thing.
    pub axis_16_bit: bool,
    /// A Stadia controller reports its right stick on `ABS_Z`/`ABS_RZ`, where
    /// every other controller reports its triggers.
    pub stadia: bool,
    pub cursor: Movement,
    pub scroll: Movement,
    pub chain_only: bool,
    /// Cursor and scroll loops keep running while makima is paused.
    pub cursor_when_paused: bool,
    /// Take the evdev device exclusively, so nothing else sees its events.
    pub grab_device: bool,
    /// Which parts of the controller's own firmware behaviour to suppress.
    ///
    /// A free string, parsed by `LizardModeSuppression::from_setting`, so there
    /// is no spelling of it this layer could call wrong.
    pub suppress_lizard_mode: String,
}

/// What a value falls back to when it is absent — and when it is wrong.
///
/// These live here rather than at the point of use, because a default is part of
/// what a setting *means*, and the layer that owns the setting should be the one
/// that knows it.
mod default {
    pub const STICK_FUNCTION: &str = "disabled";
    pub const STICK_SENSITIVITY: u64 = 0;
    pub const STICK_DEADZONE: i32 = 5;
    pub const INVERT_CURSOR_AXIS: bool = false;
    pub const INVERT_SCROLL_AXIS: bool = false;
    pub const AXIS_16_BIT: bool = false;
    pub const STADIA: bool = false;
    pub const CURSOR_SPEED: i32 = 0;
    pub const CURSOR_ACCEL: f32 = 1.0;
    pub const SCROLL_SPEED: i32 = 0;
    pub const SCROLL_ACCEL: f32 = 1.0;
    /// True, which is easy to get backwards: chain-only means a combo fires
    /// *only* as a chain, and that is the behaviour makima has always had.
    pub const CHAIN_ONLY: bool = true;
    /// True, so that a paused makima still moves the pointer — pausing is for
    /// handing the buttons to something else, not for losing the mouse.
    pub const CURSOR_WHEN_PAUSED: bool = true;
    pub const GRAB_DEVICE: bool = false;
    pub const SUPPRESS_LIZARD_MODE: &str = "buttons,mouse";
}

/// Every key that can produce a problem.
///
/// A caller reports the problems and clears the rest, which is the only way a
/// warning disappears once the value is fixed — an error that is set and never
/// cleared outlives the mistake that caused it, and that is not a hypothetical:
/// it is what the first version of this did.
///
/// `LSTICK`, `RSTICK` and `SUPPRESS_LIZARD_MODE` are deliberately absent. They
/// are free strings with no parse to fail here, so they can never be reported
/// and never need clearing.
pub const REPORTABLE_KEYS: &[&str] = &[
    "LSTICK_SENSITIVITY",
    "LSTICK_DEADZONE",
    "RSTICK_SENSITIVITY",
    "RSTICK_DEADZONE",
    "INVERT_CURSOR_AXIS",
    "INVERT_SCROLL_AXIS",
    "16_BIT_AXIS",
    "STADIA",
    "CURSOR_SPEED",
    "CURSOR_ACCEL",
    "SCROLL_SPEED",
    "SCROLL_ACCEL",
    "CHAIN_ONLY",
    "CURSOR_WHEN_PAUSED",
    "GRAB_DEVICE",
];

/// The state-file id a problem with this key is reported under.
pub fn error_id(key: &str) -> String {
    format!("setting:{key}")
}

/// One unusable value, named so it can be reported.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingProblem {
    pub key: String,
    pub value: String,
    pub expected: &'static str,
}

impl std::fmt::Display for SettingProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} = {:?} is not {} — the default is used instead",
               self.key, self.value, self.expected)
    }
}

/// Read one value, or say why it could not be read.
///
/// Absent is not a problem; present and unparseable is. That distinction is the
/// whole point: a missing setting is the normal case, a mistyped one is
/// something the user wants to know about.
fn number<T: std::str::FromStr>(
    config: &Config,
    key: &str,
    expected: &'static str,
    fallback: T,
    problems: &mut Vec<SettingProblem>,
) -> T {
    match config.settings.get(key) {
        None => fallback,
        Some(raw) => match raw.parse::<T>() {
            Ok(value) => value,
            Err(_) => {
                problems.push(SettingProblem {
                    key: key.to_string(),
                    value: raw.clone(),
                    expected,
                });
                fallback
            }
        },
    }
}

/// Read a flag, reporting anything that is neither `true` nor `false`.
///
/// It would be easier to treat every other spelling as false, and worse: a
/// `CHAIN_ONLY = "yes"` would then quietly mean the opposite of what it says.
/// The old code panicked on it, which at least said something. This says the
/// same thing without taking the daemon down.
fn flag(
    config: &Config,
    key: &str,
    fallback: bool,
    problems: &mut Vec<SettingProblem>,
) -> bool {
    number(config, key, "true or false", fallback, problems)
}

fn stick(config: &Config, prefix: &str, problems: &mut Vec<SettingProblem>) -> Stick {
    Stick {
        function: config
            .settings
            .get(prefix)
            .cloned()
            .unwrap_or_else(|| default::STICK_FUNCTION.to_string()),
        sensitivity: number(
            config, &format!("{prefix}_SENSITIVITY"),
            "a whole number of milliseconds, 0 or more",
            default::STICK_SENSITIVITY, problems,
        ),
        deadzone: number(
            config, &format!("{prefix}_DEADZONE"),
            "a whole number between 0 and 128",
            default::STICK_DEADZONE, problems,
        ),
        activation_modifiers: parse_modifiers(
            &config.settings, &format!("{prefix}_ACTIVATION_MODIFIERS"), &config.aliases,
        ),
    }
}

impl Settings {
    /// Read `[settings]`, keeping every value that parses and naming every one
    /// that does not.
    ///
    /// Never fails. A config with eighteen broken values still yields a usable
    /// `Settings` and eighteen problems to report — which is what lets a typo be
    /// a warning rather than a crash.
    pub fn from_config(config: &Config) -> (Self, Vec<SettingProblem>) {
        let mut problems = Vec::new();
        let settings = Self {
            lstick: stick(config, "LSTICK", &mut problems),
            rstick: stick(config, "RSTICK", &mut problems),
            invert_cursor_axis: flag(config, "INVERT_CURSOR_AXIS",
                                     default::INVERT_CURSOR_AXIS, &mut problems),
            invert_scroll_axis: flag(config, "INVERT_SCROLL_AXIS",
                                     default::INVERT_SCROLL_AXIS, &mut problems),
            axis_16_bit: flag(config, "16_BIT_AXIS", default::AXIS_16_BIT, &mut problems),
            stadia: flag(config, "STADIA", default::STADIA, &mut problems),
            cursor: Movement {
                speed: number(config, "CURSOR_SPEED", "a whole number",
                              default::CURSOR_SPEED, &mut problems),
                acceleration: number(config, "CURSOR_ACCEL", "a decimal number",
                                     default::CURSOR_ACCEL, &mut problems),
            },
            scroll: Movement {
                speed: number(config, "SCROLL_SPEED", "a whole number",
                              default::SCROLL_SPEED, &mut problems),
                acceleration: number(config, "SCROLL_ACCEL", "a decimal number",
                                     default::SCROLL_ACCEL, &mut problems),
            },
            chain_only: flag(config, "CHAIN_ONLY", default::CHAIN_ONLY, &mut problems),
            cursor_when_paused: flag(config, "CURSOR_WHEN_PAUSED",
                                     default::CURSOR_WHEN_PAUSED, &mut problems),
            grab_device: flag(config, "GRAB_DEVICE", default::GRAB_DEVICE, &mut problems),
            suppress_lizard_mode: config
                .settings
                .get("SUPPRESS_LIZARD_MODE")
                .cloned()
                .unwrap_or_else(|| default::SUPPRESS_LIZARD_MODE.to_string()),
        };
        (settings, problems)
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
