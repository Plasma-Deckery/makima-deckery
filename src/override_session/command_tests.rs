use super::*;

#[test]
fn the_fixed_commands_parse() {
    assert_eq!(parse(" begin"), OverrideCommand::Begin);
    assert_eq!(parse(" clear"), OverrideCommand::Clear);
    assert_eq!(parse(" end"), OverrideCommand::End);
}

/// The JSON is the rest of the line, spaces and all.
#[test]
fn set_takes_a_path_and_the_rest_of_the_line() {
    assert_eq!(
        parse(r#" set remap.BTN_SOUTH { "keys": ["KEY_ENTER"], "label": "Enter key" }"#),
        OverrideCommand::Set {
            path: "remap.BTN_SOUTH".into(),
            value: r#"{ "keys": ["KEY_ENTER"], "label": "Enter key" }"#.into(),
        },
    );
}

/// Everything else is answered with the usage, not ignored.
#[test]
fn anything_else_is_malformed() {
    for rest in ["", " ", " Begin", " begin now", " set", " set remap", " set remap ", "  begin", " heartbeat"] {
        assert!(matches!(parse(rest), OverrideCommand::Malformed(_)), "{rest:?}");
    }
}
