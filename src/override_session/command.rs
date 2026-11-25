//! What follows `override` on a control-socket line.
//!
//! Unlike the other commands, every override command is answered: the tour
//! waits for the reply, so a line it got wrong comes back as an error instead
//! of being ignored without a word. That is what `Malformed` is for.

/// One override command, as the control socket received it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverrideCommand {
    /// Open a session. Answered with its id.
    Begin,
    /// Set `path` in the override document to the JSON `value`. The value is
    /// kept as text here and parsed by the planner, so a broken one is answered
    /// with the parser's error.
    Set { path: String, value: String },
    /// Take every binding out, the base config's included.
    Clear,
    /// End the session and go back to the configs on disk.
    End,
    /// An `override` line that is none of the above, with what is wrong.
    Malformed(String),
}

const USAGE: &str = "usage: override begin | set <path> <json> | clear | end";

/// Parse what follows `override` — `rest` starts with the separating space, or
/// is empty for a bare `override`.
///
/// Matching follows the rest of the grammar: the line was trimmed already, and
/// the keywords are exact and case sensitive. The JSON is everything after the
/// path, so it may contain spaces; the path itself may not.
pub fn parse(rest: &str) -> OverrideCommand {
    let Some(rest) = rest.strip_prefix(' ') else {
        return OverrideCommand::Malformed(USAGE.to_string());
    };
    match rest {
        "begin" => OverrideCommand::Begin,
        "clear" => OverrideCommand::Clear,
        "end" => OverrideCommand::End,
        _ => match rest.strip_prefix("set ").and_then(|args| args.split_once(' ')) {
            Some((path, value)) if !path.is_empty() && !value.trim().is_empty() => {
                OverrideCommand::Set { path: path.to_string(), value: value.trim().to_string() }
            }
            _ => OverrideCommand::Malformed(USAGE.to_string()),
        },
    }
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
