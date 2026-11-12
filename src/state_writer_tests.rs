use super::*;
use crate::config_registry::ConfigSummary;

fn lifecycle_ready() -> AppLifecycle { AppLifecycle::Ready }
fn lifecycle_starting() -> AppLifecycle { AppLifecycle::Starting }
fn no_errors() -> HashMap<String, ErrorEntry> { HashMap::new() }
fn no_configs() -> Vec<ConfigSummary> { Vec::new() }

fn summary(name: &str, kind: &'static str, parent: Option<&str>, enabled: bool) -> ConfigSummary {
    ConfigSummary {
        name:    name.to_string(),
        kind,
        parent:  parent.map(str::to_string),
        exclusive_group: None,
        enabled,
        errors:  vec![],
    }
}

#[test]
fn lifecycle_starting_serialises() {
    let json = build_json(&lifecycle_starting(), &no_errors(), &None, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["lifecycle"], "starting");
    assert!(v["errors"].as_object().unwrap().is_empty());
}

#[test]
fn lifecycle_ready_serialises() {
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["lifecycle"], "ready");
}

#[test]
fn lifecycle_reinitializing_serialises() {
    let json = build_json(&AppLifecycle::Reinitializing, &no_errors(), &None, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["lifecycle"], "reinitializing");
}

#[test]
fn set_error_appears_in_json() {
    let mut errors = no_errors();
    errors.insert("no_device".to_string(), ErrorEntry {
        message:  "no matching device found".to_string(),
        severity: "error",
    });
    let json = build_json(&lifecycle_ready(), &errors, &None, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["errors"]["no_device"]["severity"], "error");
    assert!(v["errors"]["no_device"]["message"].as_str().unwrap().contains("device"));
}

#[test]
fn base_config_error_appears_in_json() {
    // Verify the base_config error slot — used by the tray to show the red icon
    // when the base config fails to parse.
    let mut errors = no_errors();
    errors.insert("base_config".to_string(), ErrorEntry {
        message:  "TOML error in \"Steam Deck\": expected `.`, `=`".to_string(),
        severity: "error",
    });
    let json = build_json(&lifecycle_ready(), &errors, &None, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["errors"]["base_config"]["severity"], "error");
    assert!(v["errors"]["base_config"]["message"].as_str().unwrap().contains("TOML error"));
}

#[test]
fn clear_error_removes_from_json() {
    let errors = no_errors();
    let json = build_json(&lifecycle_ready(), &errors, &None, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["errors"].as_object().unwrap().is_empty());
}

#[test]
fn event_state_merged_at_top_level() {
    let event_state = Some(serde_json::json!({
        "context": { "paused": false },
        "bindings": {},
    }));
    let json = build_json(&lifecycle_ready(), &no_errors(), &event_state, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["context"].is_object());
    assert!(v["bindings"].is_object());
    assert_eq!(v["lifecycle"], "ready");
}

#[test]
fn event_state_none_omits_device_fields() {
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v.get("context").is_none());
    assert!(v.get("bindings").is_none());
}

#[test]
fn configs_always_present_in_json() {
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["configs"].is_array());
}

#[test]
fn loaded_configs_appear_with_enabled_flag() {
    let configs = vec![
        summary("Steam Deck", "base", None, true),
        summary("Firefox", "app", None, false),
    ];
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &configs, &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let arr = v["configs"].as_array().unwrap();
    assert_eq!(arr.len(), 2);
    let deck = arr.iter().find(|e| e["name"] == "Steam Deck").unwrap();
    assert_eq!(deck["enabled"], true);
    assert_eq!(deck["status"], "ok");
    let firefox = arr.iter().find(|e| e["name"] == "Firefox").unwrap();
    assert_eq!(firefox["enabled"], false);
}

#[test]
fn kind_and_parent_are_serialised() {
    let configs = vec![
        summary("Steam Deck",  "base",   None,               true),
        summary("KDE Desktop", "module", Some("Steam Deck"), true),
        summary("Firefox",     "app",    None,               true),
    ];
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &configs, &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let arr = v["configs"].as_array().unwrap();
    let by = |n: &str| arr.iter().find(|e| e["name"] == n).unwrap().clone();
    assert_eq!(by("Steam Deck")["kind"],  "base");
    assert!(by("Steam Deck")["parent"].is_null());
    assert_eq!(by("KDE Desktop")["kind"],   "module");
    assert_eq!(by("KDE Desktop")["parent"], "Steam Deck");
    assert_eq!(by("Firefox")["kind"], "app");
    assert!(by("Firefox")["parent"].is_null());
}

#[test]
fn config_roots_are_null_until_reported() {
    // The tray keys its two folder items off this field, and an item that opens
    // a guessed path is worse than no item at all.
    let json = build_json(&lifecycle_starting(), &no_errors(), &None, &no_configs(), &None);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["config_roots"].is_null());
}

#[test]
fn config_roots_are_serialised() {
    let roots = Some(crate::config_registry::ConfigRoots {
        system: std::path::PathBuf::from("/usr/share/deckery/configs"),
        user:   std::path::PathBuf::from("/home/u/.config/deckery"),
    });
    let json = build_json(&lifecycle_ready(), &no_errors(), &None, &no_configs(), &roots);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["config_roots"]["system"], "/usr/share/deckery/configs");
    assert_eq!(v["config_roots"]["user"],   "/home/u/.config/deckery");
}

#[test]
fn identical_state_serialises_identically() {
    // flush() skips the write when the document matches the last one, which is
    // what keeps a key press that changed nothing observable off the disk. That
    // only works while build_json is deterministic — in particular the errors
    // map, which is a HashMap here and would serialise in a different order on
    // every call if serde_json were built with `preserve_order`.
    let mut errors = HashMap::new();
    for id in ["zulu", "alpha", "mike", "bravo"] {
        errors.insert(id.to_string(), ErrorEntry {
            message:  format!("{id} failed"),
            severity: "error",
        });
    }
    let configs = vec![
        summary("Steam Deck", "base", None, true),
        summary("KDE Desktop", "module", Some("Steam Deck"), true),
    ];

    let first  = build_json(&lifecycle_ready(), &errors, &None, &configs, &None);
    let second = build_json(&lifecycle_ready(), &errors, &None, &configs, &None);
    assert_eq!(first, second);
}

#[test]
fn a_changed_field_changes_the_document() {
    // The other half of the dedup contract: a real change must not be skipped.
    let before = build_json(&lifecycle_ready(), &no_errors(), &None,
                            &vec![summary("KDE Desktop", "module", None, true)], &None);
    let after  = build_json(&lifecycle_ready(), &no_errors(), &None,
                            &vec![summary("KDE Desktop", "module", None, false)], &None);
    assert_ne!(before, after);
}

// ── Where the file goes, and that it actually gets there ──────────────────────

#[test]
fn the_runtime_directory_is_where_the_state_goes() {
    assert_eq!(runtime_dir(Some("/run/user/1000"), 1000),
               std::path::PathBuf::from("/run/user/1000"));
}

#[test]
fn a_missing_variable_is_derived_from_the_uid_not_from_tmp() {
    // Never /tmp: that would downgrade to the squattable path in exactly the
    // situation where something is already unusual. Same rule as the socket.
    assert_eq!(runtime_dir(None, 1000), std::path::PathBuf::from("/run/user/1000"));
    assert_eq!(runtime_dir(Some(""), 42), std::path::PathBuf::from("/run/user/42"));
}

/// A throwaway directory to write state into.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn flush_writes_the_document_to_the_given_path() {
    // Until the path was a parameter this could only be verified by running
    // makima and looking — see makima-deckery#42.
    let dir = scratch("deckery-flush-writes");
    let path = dir.join("makima-state.json");
    let mut written = String::new();

    flush(&AppLifecycle::Ready, &HashMap::new(), &None, &[], &None, &path, &mut written);

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(doc["lifecycle"], "ready");
    assert!(!written.is_empty(), "the written cache must hold what went to disk");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flush_leaves_no_temporary_file_behind() {
    // The write goes to a sibling and is renamed, so a reader never sees a
    // half-written document. The sibling must not survive the rename.
    let dir = scratch("deckery-flush-tmp");
    let path = dir.join("makima-state.json");
    let mut written = String::new();

    flush(&AppLifecycle::Ready, &HashMap::new(), &None, &[], &None, &path, &mut written);

    let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n != "makima-state.json")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flush_recreates_a_file_that_was_swept_away() {
    // Unchanged content is not written again — but only while last time's file
    // is still there. Without the existence check a swept runtime directory
    // would leave the tray reading nothing for as long as the state is calm.
    let dir = scratch("deckery-flush-recreate");
    let path = dir.join("makima-state.json");
    let mut written = String::new();

    flush(&AppLifecycle::Ready, &HashMap::new(), &None, &[], &None, &path, &mut written);
    std::fs::remove_file(&path).unwrap();
    flush(&AppLifecycle::Ready, &HashMap::new(), &None, &[], &None, &path, &mut written);

    assert!(path.exists(), "an unchanged document must still restore a missing file");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flush_skips_an_unchanged_document() {
    // State is reported on every key press; most presses change nothing a
    // reader could see, and rewriting the same bytes is disk traffic on the
    // input path.
    let dir = scratch("deckery-flush-skip");
    let path = dir.join("makima-state.json");
    let mut written = String::new();

    flush(&AppLifecycle::Ready, &HashMap::new(), &None, &[], &None, &path, &mut written);
    let first = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    flush(&AppLifecycle::Ready, &HashMap::new(), &None, &[], &None, &path, &mut written);

    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), first,
               "the file was rewritten although nothing changed");
    let _ = std::fs::remove_dir_all(&dir);
}

// ── The task itself, end to end ───────────────────────────────────────────────
//
// `flush` is tested above against a path; these run the whole task the same
// way, through the handle every module holds, so that the parts which only
// exist in the task — the drain that merges a burst, the dedup across commands,
// shutting down — are covered too. Everything runs on the default
// current-thread test runtime, which is what makes `settled` below exact.

/// Generous upper bound for anything the task has to do. Never waited out in a
/// passing run; it only turns a hang into a failure.
const TASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Wait until the task has taken every queued command *and* finished acting on
/// them.
///
/// A slot frees up when the task receives a command. On a current-thread
/// runtime the task then runs apply and flush — both synchronous — up to its
/// next `recv().await` before anything else gets to run, so once the test is
/// running again with the channel empty, the write for that command is done.
async fn settled(tx: &StateWriterHandle) {
    tokio::time::timeout(TASK_TIMEOUT, async {
        while tx.capacity() < tx.max_capacity() {
            tokio::task::yield_now().await;
        }
        // One more turn so the task is certainly parked again, not merely past
        // the receive.
        tokio::task::yield_now().await;
    })
    .await
    .expect("the state writer never caught up with its queue");
}

/// Poll the file until `ready` accepts its document, and return that document.
async fn wait_for_doc(
    path: &std::path::Path,
    ready: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    tokio::time::timeout(TASK_TIMEOUT, async {
        loop {
            if let Ok(text) = std::fs::read_to_string(path) {
                if let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) {
                    if ready(&doc) {
                        return doc;
                    }
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the state file never reached the expected state")
}

/// Start the task at a fresh path and wait for its initial "starting" write,
/// so every test begins from a file that is known to be there.
async fn started(name: &str)
    -> (std::path::PathBuf, std::path::PathBuf, StateWriterHandle, tokio::task::JoinHandle<()>)
{
    let dir = scratch(name);
    let path = dir.join("makima-state.json");
    let (tx, task) = spawn_state_writer_at(path.clone());
    wait_for_doc(&path, |d| d["lifecycle"] == "starting").await;
    (dir, path, tx, task)
}

#[tokio::test]
async fn the_task_writes_every_kind_of_command_to_its_path() {
    // Every slot a module can fill, sent the way modules send them, and read
    // back the way the tray reads them.
    let (dir, path, tx, _task) = started("deckery-task-all-slots").await;

    tx.send(StateCommand::SetLifecycle(AppLifecycle::Ready)).await.unwrap();
    tx.send(StateCommand::SetError {
        id: "no_device".into(), message: "no matching device found".into(), severity: "error",
    }).await.unwrap();
    tx.send(StateCommand::SetError {
        id: "base_config".into(), message: "TOML error".into(), severity: "warning",
    }).await.unwrap();
    tx.send(StateCommand::ClearError { id: "no_device".into() }).await.unwrap();
    tx.send(StateCommand::SetEventState(Some(serde_json::json!({
        "context":  { "paused": true },
        "bindings": { "BTN_SOUTH": { "action": ["KEY_ENTER"] } },
    })))).await.unwrap();
    tx.send(StateCommand::SetLoadedConfigs(vec![
        summary("Steam Deck",  "base",   None,               true),
        summary("KDE Desktop", "module", Some("Steam Deck"), false),
    ])).await.unwrap();
    tx.send(StateCommand::SetConfigRoots(crate::config_registry::ConfigRoots {
        system: std::path::PathBuf::from("/usr/share/deckery/configs"),
        user:   std::path::PathBuf::from("/home/u/.config/deckery"),
    })).await.unwrap();

    // The last command decides when the document is complete; the earlier ones
    // may have landed in the same write or in their own.
    let doc = wait_for_doc(&path, |d| !d["config_roots"].is_null()).await;

    assert_eq!(doc["lifecycle"], "ready");
    let errors = doc["errors"].as_object().unwrap();
    assert_eq!(errors.len(), 1, "the cleared error must be gone: {errors:?}");
    assert_eq!(doc["errors"]["base_config"]["severity"], "warning");
    assert_eq!(doc["errors"]["base_config"]["message"], "TOML error");
    assert_eq!(doc["context"]["paused"], true);
    assert_eq!(doc["bindings"]["BTN_SOUTH"]["action"][0], "KEY_ENTER");
    let configs = doc["configs"].as_array().unwrap();
    assert_eq!(configs.len(), 2);
    assert_eq!(configs[1]["name"], "KDE Desktop");
    assert_eq!(configs[1]["enabled"], false);
    assert_eq!(configs[1]["parent"], "Steam Deck");
    assert_eq!(doc["config_roots"]["user"], "/home/u/.config/deckery");

    // And the device going away takes its fields with it, rather than leaving
    // the last device's bindings on screen.
    tx.send(StateCommand::SetEventState(None)).await.unwrap();
    let doc = wait_for_doc(&path, |d| d.get("context").is_none()).await;
    assert!(doc.get("bindings").is_none());
    assert_eq!(doc["lifecycle"], "ready", "the other slots must survive");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Something only an outside writer could have put there. If it survives a
/// command, the task did not write; if it is gone, it did.
const MARKER: &str = "not written by the state writer";

#[tokio::test]
async fn an_unchanged_document_is_not_rewritten() {
    // A key press that changes nothing visible still sends a snapshot; the task
    // must not turn that into a write.
    let (dir, path, tx, _task) = started("deckery-task-unchanged").await;
    settled(&tx).await;
    std::fs::write(&path, MARKER).unwrap();

    tx.send(StateCommand::SetLifecycle(AppLifecycle::Starting)).await.unwrap();
    settled(&tx).await;
    assert_eq!(std::fs::read_to_string(&path).unwrap(), MARKER,
               "a command that changed nothing rewrote the file");

    // The control: a real change does reach the file, so the marker surviving
    // above was the dedup and not a task that had stopped writing.
    tx.send(StateCommand::SetLifecycle(AppLifecycle::Ready)).await.unwrap();
    let doc = wait_for_doc(&path, |d| d["lifecycle"] == "ready").await;
    assert_eq!(doc["lifecycle"], "ready");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn commands_queued_together_become_one_write() {
    // A burst has one outcome, and the states in between are never written.
    // Observable through the dedup: a burst that goes somewhere and comes back
    // ends where the file already is, so merged it writes nothing, whereas
    // written one by one its first step would replace the marker.
    let (dir, path, tx, _task) = started("deckery-task-burst").await;
    settled(&tx).await;
    std::fs::write(&path, MARKER).unwrap();

    // try_send cannot yield, so on this single thread the task gets no chance
    // to run between the two — they are queued together by construction.
    tx.try_send(StateCommand::SetLifecycle(AppLifecycle::Ready)).unwrap();
    tx.try_send(StateCommand::SetError {
        id: "transient".into(), message: "gone again".into(), severity: "info",
    }).unwrap();
    tx.try_send(StateCommand::ClearError { id: "transient".into() }).unwrap();
    tx.try_send(StateCommand::SetLifecycle(AppLifecycle::Starting)).unwrap();
    settled(&tx).await;

    assert_eq!(std::fs::read_to_string(&path).unwrap(), MARKER,
               "an intermediate state of the burst was written");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_task_exits_once_the_last_handle_is_dropped() {
    // The writer has no shutdown command; it ends when nobody can send to it
    // any more. A task that outlived its senders would be a leak per restart.
    let (dir, _path, tx, task) = started("deckery-task-exit").await;

    drop(tx);
    tokio::time::timeout(TASK_TIMEOUT, task)
        .await
        .expect("the state writer outlived its last handle")
        .expect("the state writer panicked on the way out");

    let _ = std::fs::remove_dir_all(&dir);
}
