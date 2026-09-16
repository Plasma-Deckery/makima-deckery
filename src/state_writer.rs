// ── Deckery State Writer ──────────────────────────────────────────────────────
//
// A dedicated Tokio task that is the sole owner and writer of
// /tmp/makima-state.json.  Every module that needs to contribute state sends
// a `StateCommand` over a cloned `Sender`; the task merges all slots and
// writes atomically on every command.
//
// Slots
//   lifecycle   — always written; lets the tray distinguish "starting" from
//                 "ready with no device" from "ready and active".
//   errors      — keyed by id; any module can set / clear independently.
//   event_state — the per-event snapshot produced by EventReader (context,
//                 bindings, trackpads, …).  None when no device is active.
//   config_roots — the two directories configs are read from. Fixed for the
//                 process lifetime, published so the tray can offer to open
//                 either one without re-implementing ConfigRoots::resolve().

use std::collections::HashMap;
use tokio::sync::mpsc;
use crate::config_registry::{ConfigRoots, ConfigSummary};

// ── Public types ──────────────────────────────────────────────────────────────

pub type StateWriterHandle = mpsc::Sender<StateCommand>;

#[derive(Debug, Clone)]
pub enum AppLifecycle {
    Starting,
    Reinitializing,
    Ready,
}

#[derive(Debug, Clone)]
pub struct ErrorEntry {
    pub message:  String,
    /// "error" | "warning" | "info" — stored as string so the tray can
    /// display it without depending on this crate's types.
    pub severity: &'static str,
}

#[derive(Debug)]
pub enum StateCommand {
    /// Full event-reader snapshot; `None` means no device is active.
    SetEventState(Option<serde_json::Value>),
    /// Lifecycle transition.
    SetLifecycle(AppLifecycle),
    /// Report a named error (overwrites any previous entry with the same id).
    /// `severity` is a string literal: `"error"` | `"warning"` | `"info"`.
    SetError { id: String, message: String, severity: &'static str },
    /// Clear a previously reported error.
    ClearError { id: String },
    /// Full snapshot of the config registry — sent whenever the registry changes.
    SetLoadedConfigs(Vec<ConfigSummary>),
    /// The directories configs were read from. Sent once at startup; the roots
    /// are resolved before the registry loads and never change afterwards.
    SetConfigRoots(ConfigRoots),
}

// ── Spawner ───────────────────────────────────────────────────────────────────

/// Spawn the writer task and return the sender handle.
/// The task writes the initial "starting" state immediately.
pub fn spawn_state_writer() -> StateWriterHandle {
    let (tx, mut rx) = mpsc::channel::<StateCommand>(64);
    tokio::spawn(async move {
        let mut lifecycle   = AppLifecycle::Starting;
        let mut errors: HashMap<String, ErrorEntry> = HashMap::new();
        let mut event_state: Option<serde_json::Value> = None;
        let mut configs: Vec<ConfigSummary> = Vec::new();
        let mut roots: Option<ConfigRoots> = None;

        // The last bytes actually written. State is reported on every key press,
        // but most presses change nothing a reader can see — re-writing the same
        // document would be pure disk traffic on the input path.
        let mut written = String::new();

        // Write the initial "starting" state before any command arrives so the
        // tray sees a non-stale file the moment makima boots.
        flush(&lifecycle, &errors, &event_state, &configs, &roots, &mut written);

        while let Some(cmd) = rx.recv().await {
            apply(cmd, &mut lifecycle, &mut errors, &mut event_state, &mut configs, &mut roots);
            // Anything already queued belongs to the same moment. A burst has one
            // outcome, and the states in between are ones no reader would have
            // observed anyway. Draining does not delay anything: this only takes
            // messages that had already arrived.
            while let Ok(next) = rx.try_recv() {
                apply(next, &mut lifecycle, &mut errors, &mut event_state, &mut configs, &mut roots);
            }
            flush(&lifecycle, &errors, &event_state, &configs, &roots, &mut written);
        }
    });
    tx
}

fn apply(
    cmd:         StateCommand,
    lifecycle:   &mut AppLifecycle,
    errors:      &mut HashMap<String, ErrorEntry>,
    event_state: &mut Option<serde_json::Value>,
    configs:     &mut Vec<ConfigSummary>,
    roots:       &mut Option<ConfigRoots>,
) {
    match cmd {
        StateCommand::SetLifecycle(lc)    => { *lifecycle   = lc; }
        StateCommand::SetEventState(es)   => { *event_state = es; }
        StateCommand::SetLoadedConfigs(c) => { *configs     = c; }
        StateCommand::SetConfigRoots(r)   => { *roots       = Some(r); }
        StateCommand::SetError { id, message, severity } => {
            errors.insert(id, ErrorEntry { message, severity });
        }
        StateCommand::ClearError { id }   => { errors.remove(&id); }
    }
}

// ── Private writer ────────────────────────────────────────────────────────────

/// Pure: assemble all state slots into a JSON string.
/// No filesystem access — called by `flush` and by unit tests.
pub(crate) fn build_json(
    lifecycle:   &AppLifecycle,
    errors:      &HashMap<String, ErrorEntry>,
    event_state: &Option<serde_json::Value>,
    configs:     &[ConfigSummary],
    roots:       &Option<ConfigRoots>,
) -> String {
    let lifecycle_str = match lifecycle {
        AppLifecycle::Starting       => "starting",
        AppLifecycle::Reinitializing => "reinitializing",
        AppLifecycle::Ready          => "ready",
    };

    let errors_json: serde_json::Map<String, serde_json::Value> = errors
        .iter()
        .map(|(id, e)| {
            (id.clone(), serde_json::json!({ "message": e.message, "severity": e.severity }))
        })
        .collect();

    let configs_json: Vec<serde_json::Value> = configs.iter().map(|e| {
        let status = if e.errors.is_empty() { "ok" }
            else if e.errors.iter().any(|err| err.severity == "error") { "error" }
            else { "warning" };
        serde_json::json!({
            "name":    e.name,
            "kind":    e.kind,
            "parent":  e.parent,
            "exclusive_group": e.exclusive_group,
            "enabled": e.enabled,
            "status":  status,
            "errors":  e.errors.iter().map(|err| serde_json::json!({
                "severity": err.severity,
                "message":  err.message,
            })).collect::<Vec<_>>(),
        })
    }).collect();

    // Absent until the startup command arrives. The tray hides the folder items
    // rather than guessing a path it would then fail to open.
    let roots_json = match roots {
        Some(r) => serde_json::json!({
            "system": r.system.to_string_lossy(),
            "user":   r.user.to_string_lossy(),
        }),
        None => serde_json::Value::Null,
    };

    let mut state = serde_json::json!({
        "lifecycle":    lifecycle_str,
        "errors":       errors_json,
        "configs":      configs_json,
        "config_roots": roots_json,
    });

    // Merge the event-reader snapshot fields at the top level (context,
    // bindings, trackpads, …) so existing tray / HUD consumers keep working
    // without any JSON-path changes.
    if let Some(serde_json::Value::Object(map)) = event_state {
        for (k, v) in map {
            state[k] = v.clone();
        }
    }

    serde_json::to_string_pretty(&state).unwrap_or_default()
}

/// Write the state file, unless it would be byte-identical to the last write.
///
/// `written` carries the previous contents and is updated on every successful
/// write. It starts empty, so the first flush always happens.
fn flush(
    lifecycle:   &AppLifecycle,
    errors:      &HashMap<String, ErrorEntry>,
    event_state: &Option<serde_json::Value>,
    configs:     &[ConfigSummary],
    roots:       &Option<ConfigRoots>,
    written:     &mut String,
) {
    let json  = build_json(lifecycle, errors, event_state, configs, roots);
    if json.is_empty() || json == *written {
        return;
    }
    let tmp   = "/tmp/makima-state.json.tmp";
    let final_ = "/tmp/makima-state.json";
    if std::fs::write(tmp, &json).is_ok() && std::fs::rename(tmp, final_).is_ok() {
        *written = json;
    }
}

#[cfg(test)]
#[path = "state_writer_tests.rs"]
mod tests;
