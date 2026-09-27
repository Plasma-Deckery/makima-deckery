use super::*;

/// The command's own complaint is what the user needs, not systemd-run's
/// running commentary around it.
#[test]
fn narration_is_dropped_and_the_real_error_kept() {
    let stderr = b"Running as unit: run-p123.scope\n\
                   sh: line 1: nope: command not found\n\
                   Finished with result: exit-code\n";
    assert_eq!(describe_failure(Some(127), stderr),
               "sh: line 1: nope: command not found");
}

/// Nothing but narration means the exit code is all there is to say.
#[test]
fn exit_code_stands_in_when_only_narration_was_written() {
    let stderr = b"Running as unit: run-p1.scope\n\
                   Finished with result: exit-code\n\
                   CPU time consumed: 4ms\n";
    assert_eq!(describe_failure(Some(3), stderr), "exited with 3");
}

/// A command killed by a signal has no exit code at all.
#[test]
fn a_signal_is_named_rather_than_reported_as_an_exit_code() {
    assert_eq!(describe_failure(None, b""), "killed by a signal");
}

/// Blank lines are not a message.
#[test]
fn empty_lines_do_not_count_as_an_error_message() {
    assert_eq!(describe_failure(Some(1), b"\n   \n"), "exited with 1");
}
