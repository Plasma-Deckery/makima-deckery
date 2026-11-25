use super::*;

fn ok(_: &Layer) -> Result<(), String> {
    Ok(())
}

fn set(path: &str, value: &str) -> OverrideCommand {
    OverrideCommand::Set { path: path.into(), value: value.into() }
}

fn decide_cmd(state: &State, command: &OverrideCommand) -> Decision {
    decide(state, Input::Command(command), 7, ok)
}

/// The state a decision leaves, applied.
fn after(state: &State, decision: &Decision) -> State {
    decision.next.clone().unwrap_or_else(|| state.clone())
}

fn open_state() -> State {
    State::Open { id: 7, layer: Layer::default() }
}

#[test]
fn begin_opens_a_session_with_the_next_id() {
    let d = decide_cmd(&State::Closed, &OverrideCommand::Begin);
    assert_eq!(d.reply, Reply::Began(7));
    assert_eq!(d.next, Some(State::Open { id: 7, layer: Layer::default() }));
    assert_eq!(d.effect, Effect::Refresh);
}

#[test]
fn a_second_begin_is_refused_and_changes_nothing() {
    let d = decide_cmd(&open_state(), &OverrideCommand::Begin);
    assert_eq!(d.reply, Reply::Error("a session is already open (id 7)".into()));
    assert_eq!(d.next, None);
    assert_eq!(d.effect, Effect::None);
}

#[test]
fn every_other_command_needs_an_open_session() {
    for command in [OverrideCommand::Clear, OverrideCommand::End, set("remap.BTN_SOUTH", "[]")] {
        let d = decide_cmd(&State::Closed, &command);
        assert_eq!(d.reply, Reply::Error("no session is open".into()), "{command:?}");
        assert_eq!((d.next, d.effect), (None, Effect::None));
    }
}

#[test]
fn a_malformed_line_is_answered_whatever_the_state() {
    for state in [State::Closed, open_state()] {
        let d = decide_cmd(&state, &OverrideCommand::Malformed("usage".into()));
        assert_eq!(d.reply, Reply::Error("usage".into()));
        assert_eq!(d.next, None);
    }
}

#[test]
fn end_closes_and_reloads() {
    let d = decide_cmd(&open_state(), &OverrideCommand::End);
    assert_eq!((d.next, d.reply, d.effect), (Some(State::Closed), Reply::Ok, Effect::End));
}

#[test]
fn the_emergency_exit_ends_an_open_session_and_is_a_no_op_otherwise() {
    let d = decide(&open_state(), Input::EmergencyExit, 1, ok);
    assert_eq!((d.next, d.effect), (Some(State::Closed), Effect::End));
    let d = decide(&State::Closed, Input::EmergencyExit, 1, ok);
    assert_eq!((d.next, d.effect), (None, Effect::None));
}

/// A binding is a refresh: the readers resolve it per event.
#[test]
fn setting_a_binding_refreshes() {
    let d = decide_cmd(&open_state(), &set("remap.BTN_SOUTH", r#"["KEY_ENTER"]"#));
    assert_eq!(d.reply, Reply::Ok);
    assert_eq!(d.effect, Effect::Refresh);
    let State::Open { layer, .. } = after(&open_state(), &d) else { panic!() };
    assert_eq!(layer, Layer::default().with_value("remap.BTN_SOUTH",
        toml::Value::Array(vec![toml::Value::String("KEY_ENTER".into())])).unwrap());
}

/// A trackpad mode or a setting is read when a device session starts, so it
/// rebuilds the device tasks. Setting it again to the same thing does not.
#[test]
fn a_device_section_relaunches_once() {
    let d = decide_cmd(&open_state(), &set("trackpad.right.mode", r#""trackball""#));
    assert_eq!(d.effect, Effect::Relaunch);
    let state = after(&open_state(), &d);
    let again = decide_cmd(&state, &set("trackpad.right.mode", r#""trackball""#));
    assert_eq!(again.effect, Effect::Refresh);

    let d = decide_cmd(&state, &set("settings.LSTICK", r#""cursor""#));
    assert_eq!(d.effect, Effect::Relaunch);
}

#[test]
fn invalid_json_is_refused_before_validation() {
    let d = decide(&open_state(), Input::Command(&set("remap.BTN_SOUTH", "[KEY_ENTER")), 1,
        |_| panic!("validated a value that is not JSON"));
    assert!(matches!(&d.reply, Reply::Error(m) if m.contains("invalid JSON")), "{:?}", d.reply);
    assert_eq!((d.next, d.effect), (None, Effect::None));
}

#[test]
fn a_path_outside_the_sections_is_refused_before_validation() {
    let d = decide(&open_state(), Input::Command(&set("device.names", "[]")), 1,
        |_| panic!("validated a path that cannot be set"));
    assert!(matches!(d.reply, Reply::Error(_)));
    assert_eq!(d.next, None);
}

/// All or nothing: a layer the validator refuses never becomes the state.
#[test]
fn a_refused_layer_leaves_the_state_alone() {
    let state = after(&open_state(), &decide_cmd(&open_state(), &set("remap.BTN_SOUTH", r#"["KEY_ENTER"]"#)));
    let d = decide(&state, Input::Command(&set("remap.BTN_EAST", r#"["KEY_ESC"]"#)), 1,
        |_| Err("no".into()));
    assert_eq!(d.reply, Reply::Error("remap.BTN_EAST: no".into()));
    assert_eq!((d.next, d.effect), (None, Effect::None));
}

#[test]
fn clear_marks_the_layer_cleared() {
    let d = decide_cmd(&open_state(), &OverrideCommand::Clear);
    let State::Open { layer, id } = after(&open_state(), &d) else { panic!() };
    assert_eq!(id, 7);
    assert!(layer.is_cleared());
    assert_eq!(d.effect, Effect::Refresh);
}

#[test]
fn a_reply_is_one_line() {
    assert_eq!(Reply::Ok.to_string(), "ok");
    assert_eq!(Reply::Began(3).to_string(), "ok 3");
    assert_eq!(Reply::Error("TOML error\n  at line 1\n".into()).to_string(), "error TOML error at line 1");
}
