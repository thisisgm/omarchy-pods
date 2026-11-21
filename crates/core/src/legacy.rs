//! The verbs the C++ daemon accepts on its control socket, one verb per connection.

use std::num::IntErrorKind;

use pods_proto::noise::NoiseMode;

use crate::command::{AdaptiveLevel, Command, CommandError, NoiseTarget};
use crate::state::EarDetectionBehavior;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyRequest {
    Status,
    Command(Command),
}

/// A second daemon sends this to the first to open its window.
pub const REOPEN_VERB: &str = "reopen";

/// Every whole-word verb of the dispatch in `daemon/main.cpp`, in its order.
pub(crate) const VERBS: [(&str, LegacyRequest); 17] = [
    (REOPEN_VERB, LegacyRequest::Command(Command::Reopen)),
    ("noise:off", noise(NoiseTarget::Mode(NoiseMode::Off))),
    ("noise:anc", noise(NoiseTarget::Mode(NoiseMode::Anc))),
    (
        "noise:transparency",
        noise(NoiseTarget::Mode(NoiseMode::Transparency)),
    ),
    (
        "noise:adaptive",
        noise(NoiseTarget::Mode(NoiseMode::Adaptive)),
    ),
    ("noise:cycle", noise(NoiseTarget::Cycle)),
    ("ear:off", ear(EarDetectionBehavior::Disabled)),
    ("ear:one", ear(EarDetectionBehavior::PauseWhenOneRemoved)),
    ("ear:both", ear(EarDetectionBehavior::PauseWhenBothRemoved)),
    ("forget", LegacyRequest::Command(Command::Forget)),
    (
        "ca:on",
        LegacyRequest::Command(Command::SetConversationalAwareness(true)),
    ),
    (
        "ca:off",
        LegacyRequest::Command(Command::SetConversationalAwareness(false)),
    ),
    ("disconnect", LegacyRequest::Command(Command::Disconnect)),
    ("connect", LegacyRequest::Command(Command::Connect)),
    (
        "onebud:on",
        LegacyRequest::Command(Command::SetOneBudAnc(true)),
    ),
    (
        "onebud:off",
        LegacyRequest::Command(Command::SetOneBudAnc(false)),
    ),
    ("status", LegacyRequest::Status),
];

const ADAPTIVE_PREFIX: &str = "adaptive:";

/// Qt's `toInt` skips these after the digits, and `parseIntVerb` refuses them before.
const QT_TRAILING_SPACE: [char; 6] = [' ', '\t', '\n', '\u{b}', '\u{c}', '\r'];

const fn noise(target: NoiseTarget) -> LegacyRequest {
    LegacyRequest::Command(Command::SetNoise(target))
}

const fn ear(behavior: EarDetectionBehavior) -> LegacyRequest {
    LegacyRequest::Command(Command::SetEarDetection(behavior))
}

// Sample input: `noise:anc`, `status`, `adaptive:37`; `None` for what C++ logs as unknown.
#[must_use]
pub fn parse_verb(bytes: &[u8]) -> Option<LegacyRequest> {
    let text = std::str::from_utf8(bytes).ok()?;
    if let Some(payload) = text.strip_prefix(ADAPTIVE_PREFIX) {
        // The legacy dialect has no error reply, so a refused level is only an unknown request.
        let level = adaptive_level(payload).ok()?;
        return Some(LegacyRequest::Command(Command::SetAdaptiveLevel(level)));
    }
    VERBS
        .iter()
        .find(|(verb, _)| *verb == text)
        .map(|&(_, request)| request)
}

// `parseIntVerb` over Qt `toInt`. Sample input: `+37`, `37\n`; `37x` bad, `101` out of range.
pub(crate) fn adaptive_level(payload: &str) -> Result<AdaptiveLevel, CommandError> {
    let value = match payload.trim_end_matches(QT_TRAILING_SPACE).parse::<i32>() {
        Ok(value) => value,
        Err(error)
            if matches!(
                error.kind(),
                IntErrorKind::PosOverflow | IntErrorKind::NegOverflow
            ) =>
        {
            return Err(CommandError::OutOfRange);
        }
        Err(_) => return Err(CommandError::BadRequest),
    };
    let Ok(percent) = u8::try_from(value) else {
        return Err(CommandError::OutOfRange);
    };
    AdaptiveLevel::new(percent)
}

// Sample input: family `noise` with word `anc`, which is the verb `noise:anc`.
pub(crate) fn family_command(family: &str, word: &str) -> Option<Command> {
    VERBS.iter().find_map(|&(verb, request)| match request {
        LegacyRequest::Command(command) if verb.split_once(':') == Some((family, word)) => {
            Some(command)
        }
        LegacyRequest::Command(_) | LegacyRequest::Status => None,
    })
}

// Sample input: family `noise` with `SetNoise(Mode(Anc))`, which is the verb `noise:anc`, so `anc`.
pub(crate) fn family_word(family: &str, command: Command) -> Option<&'static str> {
    VERBS
        .iter()
        .find_map(|&(verb, request)| match verb.split_once(':') {
            Some((verb_family, word))
                if verb_family == family && request == LegacyRequest::Command(command) =>
            {
                Some(word)
            }
            Some(_) | None => None,
        })
}

// Sample input: `disconnect`.
pub(crate) fn word_command(word: &str) -> Option<Command> {
    VERBS.iter().find_map(|&(verb, request)| match request {
        LegacyRequest::Command(command) if verb == word => Some(command),
        LegacyRequest::Command(_) | LegacyRequest::Status => None,
    })
}

#[cfg(test)]
mod tests;
