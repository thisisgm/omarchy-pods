//! Protocol 1 and D-Bus settings, read through the legacy verb table so every surface agrees.

use pods_proto::noise::NoiseMode;

use crate::command::{Command, CommandError, NoiseTarget};
use crate::legacy;
use crate::state::EarDetectionBehavior;

#[derive(Debug, Clone, Copy)]
enum Family {
    /// The legacy family and the words it takes, as `noise` and `anc` for the verb `noise:anc`.
    Words(&'static str, &'static [&'static str]),
    Switch(&'static str),
    /// A switch the daemon keeps itself, with no legacy verb.
    DaemonSwitch(fn(bool) -> Command),
    Level,
}

pub const NOISE: &str = "noise";

pub const EAR_DETECTION: &str = "ear_detection";

pub const CONVERSATIONAL_AWARENESS: &str = "conversational_awareness";

pub const ONE_BUD_ANC: &str = "one_bud_anc";

pub const ADAPTIVE_LEVEL: &str = "adaptive_level";

pub const NOTIFICATIONS: &str = "notifications";

const NOISE_WORDS: [&str; 5] = [
    NoiseTarget::Mode(NoiseMode::Off).word(),
    NoiseTarget::Mode(NoiseMode::Anc).word(),
    NoiseTarget::Mode(NoiseMode::Transparency).word(),
    NoiseTarget::Mode(NoiseMode::Adaptive).word(),
    NoiseTarget::Cycle.word(),
];

const EAR_WORDS: [&str; 3] = [
    EarDetectionBehavior::Disabled.word(),
    EarDetectionBehavior::PauseWhenOneRemoved.word(),
    EarDetectionBehavior::PauseWhenBothRemoved.word(),
];

const KEYS: [(&str, Family); 6] = [
    (NOISE, Family::Words("noise", &NOISE_WORDS)),
    (EAR_DETECTION, Family::Words("ear", &EAR_WORDS)),
    (CONVERSATIONAL_AWARENESS, Family::Switch("ca")),
    (ONE_BUD_ANC, Family::Switch("onebud")),
    (ADAPTIVE_LEVEL, Family::Level),
    (
        NOTIFICATIONS,
        Family::DaemonSwitch(Command::SetNotifications),
    ),
];

/// What a switch takes, each with the legacy word that sets it.
const SWITCH_WORDS: [(&str, &str); 2] = [("true", "on"), ("false", "off")];

const SWITCH_VALUES: [&str; 2] = [SWITCH_WORDS[0].0, SWITCH_WORDS[1].0];

// Sample input: key `noise` with value `anc`, or key `adaptive_level` with value `37`.
pub fn setting(key: &str, value: &str) -> Result<Command, CommandError> {
    match family(key).ok_or(CommandError::UnknownKey)? {
        Family::Words(prefix, words) => words
            .contains(&value)
            .then(|| legacy::family_command(prefix, value))
            .flatten()
            .ok_or(CommandError::BadValue),
        Family::Switch(prefix) => SWITCH_WORDS
            .iter()
            .find_map(|&(word, legacy_word)| (word == value).then_some(legacy_word))
            .and_then(|legacy_word| legacy::family_command(prefix, legacy_word))
            .ok_or(CommandError::BadValue),
        // `bool::from_str` takes exactly the words of `SWITCH_VALUES`.
        Family::DaemonSwitch(command) => value
            .parse::<bool>()
            .ok()
            .map(command)
            .ok_or(CommandError::BadValue),
        Family::Level => legacy::adaptive_level(value).map(Command::SetAdaptiveLevel),
    }
}

/// `None` for a key that is not a setting and for `adaptive_level`, which takes a number.
#[must_use]
pub fn values(key: &str) -> Option<&'static [&'static str]> {
    match family(key)? {
        Family::Words(_, words) => Some(words),
        Family::Switch(_) | Family::DaemonSwitch(_) => Some(&SWITCH_VALUES),
        Family::Level => None,
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
            | Command::SetNotifications(_)
            | Command::Reopen,
        )
        | None => Err(CommandError::BadRequest),
    }
}

#[cfg(test)]
mod tests;
