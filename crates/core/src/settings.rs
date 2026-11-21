//! The settings and actions a client names, over protocol 1 or D-Bus, read through the legacy verb table so every surface means the same commands.

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

/// The key of the listening mode.
pub const NOISE: &str = "noise";

/// The key of the ear detection behaviour.
pub const EAR_DETECTION: &str = "ear_detection";

/// The key of conversational awareness.
pub const CONVERSATIONAL_AWARENESS: &str = "conversational_awareness";

/// The key of noise control with one bud in.
pub const ONE_BUD_ANC: &str = "one_bud_anc";

/// The key of the adaptive noise level.
pub const ADAPTIVE_LEVEL: &str = "adaptive_level";

/// Every key `set` takes, with the legacy verb family it reads its value through.
const KEYS: [(&str, Family); 5] = [
    (NOISE, Family::Words("noise")),
    (EAR_DETECTION, Family::Words("ear")),
    (CONVERSATIONAL_AWARENESS, Family::Switch("ca")),
    (ONE_BUD_ANC, Family::Switch("onebud")),
    (ADAPTIVE_LEVEL, Family::Level),
];

/// The protocol 1 spelling of a switch, with the legacy word it stands for.
const SWITCH_WORDS: [(&str, &str); 2] = [("true", "on"), ("false", "off")];

/// The command one `set` asks for, or why it is refused.
// Sample input: key `noise` with value `anc`, or key `adaptive_level` with value `37`.
pub fn setting(key: &str, value: &str) -> Result<Command, CommandError> {
    match family(key).ok_or(CommandError::UnknownKey)? {
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

/// The word `set` takes under `key` to ask for `command`, such as `anc` under `noise`; `None` when the key's values are not words.
// Sample input: key `ear_detection` with `SetEarDetection(PauseWhenBothRemoved)`, which is the verb `ear:both`, so `both`.
#[must_use]
pub fn word(key: &str, command: Command) -> Option<&'static str> {
    match family(key)? {
        Family::Words(prefix) => legacy::family_word(prefix, command),
        Family::Switch(_) | Family::Level => None,
    }
}

fn family(key: &str) -> Option<Family> {
    KEYS.iter()
        .find_map(|&(name, family)| (name == key).then_some(family))
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
