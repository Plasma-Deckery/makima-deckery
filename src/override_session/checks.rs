//! The strict half of validating a layer.
//!
//! A config file is forgiving past the parser: a pad value its handler cannot
//! use falls back to a default with a warning, a hint that can never be shown
//! is reported and dropped, a setting out of range is replaced. An override
//! is all or nothing, and a silent default is a part of the `set` that did not
//! apply — so here those warnings refuse the layer.
//!
//! Each check asks the code that would warn — the handler's own `parse`, the
//! KDE config's deserialisation, `resolve_hints`, `Settings::from_config` —
//! and only about what the layer itself writes. A warning the user's own
//! config already raises underneath is not the layer's to refuse; it is
//! filtered out by asking the same question about the underlay.

use super::layer::Refusal;
use crate::config::{Config, TrackpadSideConfig};

/// The trackpad tables the layer writes: tables where tables belong, a mode
/// the device session knows, handler and KDE values without a fallback.
pub fn trackpad(over: Option<&toml::Value>, underlay: &Config, effective: &Config) -> Result<(), Refusal> {
    let Some(over) = over else { return Ok(()) };
    let Some(over) = over.as_table() else {
        return Err(Refusal::at("trackpad", "must be a table"));
    };
    for (side, under, eff) in [
        ("left", &underlay.trackpad.left, &effective.trackpad.left),
        ("right", &underlay.trackpad.right, &effective.trackpad.right),
    ] {
        if let Some(written) = over.get(side) {
            pad_side(side, written, under, eff)?;
        }
    }
    if let Some(written) = over.get("gestures") {
        let Some(written) = written.as_table() else {
            return Err(Refusal::at("trackpad.gestures", "must be a table"));
        };
        let parse = |t: &toml::Value| crate::gesture_pad::GesturePadConfig::parse(t).1;
        new_warning("trackpad.gestures",
            parse(&effective.trackpad.gesture_handler_config),
            parse(&underlay.trackpad.gesture_handler_config))?;
        if written.contains_key("kde") {
            effective.trackpad.gesture_kde_config.clone()
                .try_into::<crate::kde_input_defaults::GestureKdeConfig>()
                .map_err(|e| Refusal::at("trackpad.gestures.kde", e))?;
        }
    }
    Ok(())
}

fn pad_side(side: &str, written: &toml::Value, under: &TrackpadSideConfig, eff: &TrackpadSideConfig) -> Result<(), Refusal> {
    let Some(written) = written.as_table() else {
        return Err(Refusal::at(format!("trackpad.{side}"), "must be a table"));
    };
    // Core reads `click` itself and keeps the click on for anything that is
    // not a bool; refused here rather than left on.
    if written.get("click").is_some_and(|v| !v.is_bool()) {
        return Err(Refusal::at(format!("trackpad.{side}.click"), "must be true or false"));
    }
    if written.contains_key("mode") && !crate::device_session::is_known_trackpad_mode(&eff.mode) {
        return Err(Refusal::at(
            format!("trackpad.{side}.mode"), format!("{:?} is not a trackpad mode", eff.mode)));
    }
    // Both tables read by the mode that will run, so a key only the old mode
    // knew is compared like for like.
    new_warning(&format!("trackpad.{side}"),
        handler_warnings(&eff.mode, &eff.handler_config),
        handler_warnings(&eff.mode, &under.handler_config))?;
    if written.contains_key("kde") {
        eff.kde_config.clone()
            .try_into::<crate::kde_input_defaults::PadKdeConfig>()
            .map_err(|e| Refusal::at(format!("trackpad.{side}.kde"), e))?;
    }
    Ok(())
}

/// What the handler for `mode` would warn about in `table`. A mode without a
/// handler reads nothing, and so has nothing to warn about.
fn handler_warnings(mode: &str, table: &toml::Value) -> Vec<String> {
    match mode {
        "mt-trackpad" => crate::mt_trackpad::MtTrackpadConfig::parse(table).1,
        "trackball" => crate::trackball::TrackballConfig::parse(table).1,
        _ => Vec::new(),
    }
}

/// Refuse the first warning in `now` that `before` did not have.
fn new_warning(at: &str, now: Vec<String>, before: Vec<String>) -> Result<(), Refusal> {
    match now.into_iter().find(|w| !before.contains(w)) {
        Some(warning) => Err(Refusal::at(at, warning)),
        None => Ok(()),
    }
}

/// The hints the layer writes, each one shown. A hint that can never be shown
/// or that loses its button to another line is refused; one shown on several
/// buttons is not — that is the hint working, the warning only says so.
pub fn hints(over: Option<&toml::Value>, effective: &Config) -> Result<(), Refusal> {
    let Some(toml::Value::Table(written)) = over else { return Ok(()) };
    let warnings = effective.clone().resolve_hints();
    for raw in written.keys() {
        let prefix = format!("hint {raw:?}: ");
        let lost = warnings.iter()
            .filter_map(|w| w.strip_prefix(&prefix))
            .find(|why| why.contains("can never be shown") || why.contains("is shown"));
        if let Some(why) = lost {
            return Err(Refusal::at(format!("hints.{raw}"), why));
        }
    }
    Ok(())
}

/// The settings the layer writes, each one usable as written.
pub fn settings(over: Option<&toml::Value>, effective: &Config) -> Result<(), Refusal> {
    let Some(toml::Value::Table(written)) = over else { return Ok(()) };
    let (_, problems) = crate::settings::Settings::from_config(effective);
    match problems.iter().find(|p| written.contains_key(&p.key)) {
        Some(problem) => Err(Refusal::at(
            format!("settings.{}", problem.key),
            format!("{:?} is not {}", problem.value, problem.expected))),
        None => Ok(()),
    }
}
