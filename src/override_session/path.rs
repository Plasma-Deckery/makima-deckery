//! A dotted path and a JSON value, as a change to the override document.
//!
//! `remap` names the whole `[remap]` table, `remap.BTN_SOUTH` one binding in
//! it, `trackpad.right.mode` one key three tables down. Setting a path replaces
//! whatever the document held there and creates the tables on the way.
//!
//! Nothing here knows what a valid config is. The value only has to be
//! something TOML can hold; whether it is a binding is for the config parser
//! to say once the whole document is put together.

/// The sections an override may set. The rest of a config either describes
/// what a file *is* (`[device]`, `[module]`) or is device-wide state the
/// session switches off anyway (`[gaming_mode]`).
pub const SECTIONS: &[&str] = &["remap", "commands", "movements", "hints", "settings", "trackpad"];

/// The segments of `path`, checked: none empty, and the first one a section
/// an override may set.
pub fn segments(path: &str) -> Result<Vec<&str>, String> {
    let segments: Vec<&str> = path.split('.').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(format!("{path:?} has an empty segment"));
    }
    if !SECTIONS.contains(&segments[0]) {
        return Err(format!(
            "{:?} cannot be overridden — the sections that can are {}",
            segments[0], SECTIONS.join(", ")));
    }
    Ok(segments)
}

/// A JSON value as TOML. `null` has no TOML form and is refused; every other
/// JSON value has exactly one.
pub fn json_to_toml(value: &serde_json::Value) -> Result<toml::Value, String> {
    use serde_json::Value as J;
    Ok(match value {
        J::Null => return Err("null has no TOML form".to_string()),
        J::Bool(b) => toml::Value::Boolean(*b),
        J::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => toml::Value::Integer(i),
            (None, Some(f)) => toml::Value::Float(f),
            _ => return Err(format!("{n} does not fit a TOML number")),
        },
        J::String(s) => toml::Value::String(s.clone()),
        J::Array(items) => toml::Value::Array(
            items.iter().map(json_to_toml).collect::<Result<_, _>>()?),
        J::Object(map) => toml::Value::Table(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), json_to_toml(v)?)))
                .collect::<Result<_, String>>()?),
    })
}

/// Put `value` at `segments` in `doc`, creating the tables on the way.
///
/// A segment on the way that holds something other than a table is an error
/// rather than being replaced: `set remap.BTN_SOUTH.keys …` after
/// `set remap.BTN_SOUTH […]` is almost certainly a mistake in the caller.
pub fn insert(doc: &mut toml::value::Table, segments: &[&str], value: toml::Value) -> Result<(), String> {
    let (last, parents) = segments.split_last().expect("segments() never returns an empty path");
    let mut table = doc;
    for (depth, segment) in parents.iter().enumerate() {
        let entry = table.entry(segment.to_string())
            .or_insert_with(|| toml::Value::Table(Default::default()));
        table = match entry {
            toml::Value::Table(t) => t,
            _ => return Err(format!("{} is not a table", segments[..=depth].join("."))),
        };
    }
    table.insert(last.to_string(), value);
    Ok(())
}

/// Lay `over` onto `under`, table by table: a key in both that is a table on
/// both sides is merged, anything else in `over` replaces what `under` held.
pub fn deep_merge(under: &mut toml::Value, over: &toml::Value) {
    match (under, over) {
        (toml::Value::Table(u), toml::Value::Table(o)) => {
            for (key, value) in o {
                match u.get_mut(key) {
                    Some(existing) => deep_merge(existing, value),
                    None => { u.insert(key.clone(), value.clone()); }
                }
            }
        }
        (under, over) => *under = over.clone(),
    }
}

#[cfg(test)]
#[path = "path_tests.rs"]
mod tests;
