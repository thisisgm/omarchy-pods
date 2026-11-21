//! The settings and actions a protocol 1 client names, read through the legacy verb table so both dialects mean the same commands.

use crate::command::{Command, CommandError};
use crate::legacy;

/// How a protocol 1 key finds its command in the legacy verbs.
#[derive(Debug, Clone, Copy)]
enum Family {
    /// The value is the word after `<family>:` in a legacy verb, as `anc` in `noise:anc`.
    Words(&'static str),
    /// The value is `true` or `false`, which the legacy verbs of this family spell `on` and `off`.
    Switch(&'static str),
    /// The value is the adaptive noise level, read by the legacy integer rule.
    Level,
}

/// Every key `set` takes, with the legacy verb family it reads its value through.
const KEYS: [(&str, Family); 5] = [
    ("noise", Family::Words("noise")),
    ("ear_detection", Family::Words("ear")),
    ("conversational_awareness", Family::Switch("ca")),
    ("one_bud_anc", Family::Switch("onebud")),
    ("adaptive_level", Family::Level),
];

/// The protocol 1 spelling of a switch, with the legacy word it stands for.
const SWITCH_WORDS: [(&str, &str); 2] = [("true", "on"), ("false", "off")];

/// The command one `set` asks for, or why it is refused.
// Sample input: key `noise` with value `anc`, or key `adaptive_level` with value `37`.
pub fn setting(key: &str, value: &str) -> Result<Command, CommandError> {
    let family = KEYS
        .iter()
        .find_map(|&(name, family)| (name == key).then_some(family))
        .ok_or(CommandError::UnknownKey)?;
    match family {
        Family::Words(prefix) => {
            legacy::family_command(prefix, value).ok_or(CommandError::BadRequest)
        }
        Family::Switch(prefix) => SWITCH_WORDS
            .iter()
            .find_map(|&(word, legacy_word)| (word == value).then_some(legacy_word))
            .and_then(|legacy_word| legacy::family_command(prefix, legacy_word))
            .ok_or(CommandError::BadRequest),
        Family::Level => legacy::adaptive_level(value).map(Command::SetAdaptiveLevel),
    }
}

/// The command one `do` asks for: only `connect`, `disconnect` and `forget` are actions.
// Sample input: `disconnect`.
pub fn action(name: &str) -> Result<Command, CommandError> {
    match legacy::word_command(name) {
        Some(command @ (Command::Connect | Command::Disconnect | Command::Forget)) => Ok(command),
        Some(
            Command::SetNoise(_)
            | Command::SetEarDetection(_)
            | Command::SetConversationalAwareness(_)
            | Command::SetOneBudAnc(_)
            | Command::SetAdaptiveLevel(_)
            | Command::Reopen,
        )
        | None => Err(CommandError::BadRequest),
    }
}

#[cfg(test)]
mod tests;
