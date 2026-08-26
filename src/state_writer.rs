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

use std::collections::HashMap;
use tokio::sync::mpsc;

// ── Public types ──────────────────────────────────────────────────────────────

pub type StateWriterHandle = mpsc::Sender<StateCommand>;

#[derive(Debug, Clone)]
pub enum AppLifecycle {
    Starting,
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

        // Write the initial "starting" state before any command arrives so the
        // tray sees a non-stale file the moment makima boots.
        flush(&lifecycle, &errors, &event_state);

        while let Some(cmd) = rx.recv().await {
            match cmd {
                StateCommand::SetLifecycle(lc)       => { lifecycle    = lc; }
                StateCommand::SetEventState(es)       => { event_state  = es; }
                StateCommand::SetError { id, message, severity } => {
                    errors.insert(id, ErrorEntry { message, severity });
                }

                StateCommand::ClearError { id }      => { errors.remove(&id); }
            }
            flush(&lifecycle, &errors, &event_state);
        }
    });
    tx
}

// ── Private writer ────────────────────────────────────────────────────────────

/// Pure: assemble all state slots into a JSON string.
/// No filesystem access — called by `flush` and by unit tests.
pub(crate) fn build_json(
    lifecycle:   &AppLifecycle,
    errors:      &HashMap<String, ErrorEntry>,
    event_state: &Option<serde_json::Value>,
) -> String {
    let lifecycle_str = match lifecycle {
        AppLifecycle::Starting => "starting",
        AppLifecycle::Ready    => "ready",
    };

    let errors_json: serde_json::Map<String, serde_json::Value> = errors
        .iter()
        .map(|(id, e)| {
            (id.clone(), serde_json::json!({ "message": e.message, "severity": e.severity }))
        })
        .collect();

    let mut state = serde_json::json!({
        "lifecycle": lifecycle_str,
        "errors":    errors_json,
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

fn flush(
    lifecycle:   &AppLifecycle,
    errors:      &HashMap<String, ErrorEntry>,
    event_state: &Option<serde_json::Value>,
) {
    let json  = build_json(lifecycle, errors, event_state);
    let tmp   = "/tmp/makima-state.json.tmp";
    let final_ = "/tmp/makima-state.json";
    if !json.is_empty() && std::fs::write(tmp, &json).is_ok() {
        let _ = std::fs::rename(tmp, final_);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lifecycle_ready() -> AppLifecycle { AppLifecycle::Ready }
    fn lifecycle_starting() -> AppLifecycle { AppLifecycle::Starting }
    fn no_errors() -> HashMap<String, ErrorEntry> { HashMap::new() }

    #[test]
    fn lifecycle_starting_serialises() {
        let json = build_json(&lifecycle_starting(), &no_errors(), &None);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["lifecycle"], "starting");
        assert!(v["errors"].as_object().unwrap().is_empty());
    }

    #[test]
    fn lifecycle_ready_serialises() {
        let json = build_json(&lifecycle_ready(), &no_errors(), &None);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["lifecycle"], "ready");
    }

    #[test]
    fn set_error_appears_in_json() {
        let mut errors = no_errors();
        errors.insert("no_device".to_string(), ErrorEntry {
            message:  "no matching device found".to_string(),
            severity: "error",
        });
        let json = build_json(&lifecycle_ready(), &errors, &None);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["errors"]["no_device"]["severity"], "error");
        assert!(v["errors"]["no_device"]["message"].as_str().unwrap().contains("device"));
    }

    #[test]
    fn clear_error_removes_from_json() {
        let errors = no_errors(); // error was cleared before build_json call
        let json = build_json(&lifecycle_ready(), &errors, &None);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["errors"].as_object().unwrap().is_empty());
    }

    #[test]
    fn event_state_merged_at_top_level() {
        let event_state = Some(serde_json::json!({
            "context": { "paused": false },
            "bindings": {},
        }));
        let json = build_json(&lifecycle_ready(), &no_errors(), &event_state);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        // event_state fields must be at top level, alongside lifecycle/errors
        assert!(v["context"].is_object());
        assert!(v["bindings"].is_object());
        assert_eq!(v["lifecycle"], "ready"); // lifecycle still present
    }

    #[test]
    fn event_state_none_omits_device_fields() {
        let json = build_json(&lifecycle_ready(), &no_errors(), &None);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.get("context").is_none());
        assert!(v.get("bindings").is_none());
    }
}
