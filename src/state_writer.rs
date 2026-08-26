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
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone)]
pub struct ErrorEntry {
    pub message:  String,
    pub severity: Severity,
}

#[derive(Debug)]
pub enum StateCommand {
    /// Full event-reader snapshot; `None` means no device is active.
    SetEventState(Option<serde_json::Value>),
    /// Lifecycle transition.
    SetLifecycle(AppLifecycle),
    /// Report a named error (overwrites any previous entry with the same id).
    SetError { id: String, message: String, severity: Severity },
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

fn flush(
    lifecycle:   &AppLifecycle,
    errors:      &HashMap<String, ErrorEntry>,
    event_state: &Option<serde_json::Value>,
) {
    let lifecycle_str = match lifecycle {
        AppLifecycle::Starting => "starting",
        AppLifecycle::Ready    => "ready",
    };

    let errors_json: serde_json::Map<String, serde_json::Value> = errors
        .iter()
        .map(|(id, e)| {
            let sev = match e.severity {
                Severity::Error   => "error",
                Severity::Warning => "warning",
                Severity::Info    => "info",
            };
            (id.clone(), serde_json::json!({ "message": e.message, "severity": sev }))
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

    let tmp   = "/tmp/makima-state.json.tmp";
    let final_ = "/tmp/makima-state.json";
    if let Ok(json) = serde_json::to_string_pretty(&state) {
        if std::fs::write(tmp, &json).is_ok() {
            let _ = std::fs::rename(tmp, final_);
        }
    }
}
