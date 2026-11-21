//! The verbs the C++ daemon accepts on its control socket, one verb per connection.

use std::num::IntErrorKind;

use pods_proto::noise::NoiseMode;

use crate::command::{ADAPTIVE_LEVEL_MAX, Command, CommandError, NoiseTarget};
use crate::state::EarDetectionBehavior;

/// What one legacy request asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyRequest {
    /// Reply with the status line.
    Status,
    Command(Command),
}

/// The verb that asks the daemon to open its window, also what a second daemon sends the first.
pub const REOPEN_VERB: &str = "reopen";

/// Every whole-word verb of the C++ dispatch in `daemon/main.cpp`, in its order, with what it asks for.
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

/// The verb family whose payload is the adaptive noise level.
const ADAPTIVE_PREFIX: &str = "adaptive:";

/// Qt's `toInt` skips these after the digits, and `parseIntVerb` refuses them before.
const QT_TRAILING_SPACE: [char; 6] = [' ', '\t', '\n', '\u{b}', '\u{c}', '\r'];

const fn noise(target: NoiseTarget) -> LegacyRequest {
    LegacyRequest::Command(Command::SetNoise(target))
}

const fn ear(behavior: EarDetectionBehavior) -> LegacyRequest {
    LegacyRequest::Command(Command::SetEarDetection(behavior))
}

/// The request in one connection's bytes, or `None` for anything the C++ daemon would log as unknown.
// Sample input: `noise:anc`, `status` or `adaptive:37`, with no newline.
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

/// The level in an `adaptive:` payload, by the rule of `parseIntVerb` over Qt's `toInt`: not a whole number is a bad request, outside 0 to 100 is out of range.
// Sample input: `37`, `+37`, `-0` or `37\n`; ` 37` and `37x` are bad, `101` and `3000000000` are out of range.
pub(crate) fn adaptive_level(payload: &str) -> Result<u8, CommandError> {
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
    u8::try_from(value)
        .ok()
        .filter(|level| *level <= ADAPTIVE_LEVEL_MAX)
        .ok_or(CommandError::OutOfRange)
}

/// The command of the table verb `<family>:<word>`, the way protocol 1 names a setting and its value.
// Sample input: family `noise` with word `anc`, which is the verb `noise:anc`.
pub(crate) fn family_command(family: &str, word: &str) -> Option<Command> {
    VERBS.iter().find_map(|&(verb, request)| match request {
        LegacyRequest::Command(command) if verb.split_once(':') == Some((family, word)) => {
            Some(command)
        }
        LegacyRequest::Command(_) | LegacyRequest::Status => None,
    })
}

/// The command of a table verb that is one whole word, the way protocol 1 names an action.
// Sample input: `disconnect`.
pub(crate) fn word_command(word: &str) -> Option<Command> {
    VERBS.iter().find_map(|&(verb, request)| match request {
        LegacyRequest::Command(command) if verb == word => Some(command),
        LegacyRequest::Command(_) | LegacyRequest::Status => None,
    })
}

#[cfg(test)]
mod tests;
