//! Protocol 1 and D-Bus settings, read through the legacy verb table so every surface agrees.

use crate::command::{Command, CommandError};
use crate::legacy;

#[derive(Debug, Clone, Copy)]
enum Family {
    /// The word after `<family>:` in a legacy verb, as `anc` in `noise:anc`.
    Words(&'static str),
    Switch(&'static str),
    Level,
}

pub const NOISE: &str = "noise";

pub const EAR_DETECTION: &str = "ear_detection";

pub const CONVERSATIONAL_AWARENESS: &str = "conversational_awareness";

pub const ONE_BUD_ANC: &str = "one_bud_anc";

pub const ADAPTIVE_LEVEL: &str = "adaptive_level";

const KEYS: [(&str, Family); 5] = [
    (NOISE, Family::Words("noise")),
    (EAR_DETECTION, Family::Words("ear")),
    (CONVERSATIONAL_AWARENESS, Family::Switch("ca")),
    (ONE_BUD_ANC, Family::Switch("onebud")),
    (ADAPTIVE_LEVEL, Family::Level),
];

const SWITCH_WORDS: [(&str, &str); 2] = [("true", "on"), ("false", "off")];

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

fn family(key: &str) -> Option<Family> {
    KEYS.iter()
        .find_map(|&(name, family)| (name == key).then_some(family))
}

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
