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
pub const STATUS_VERB: &str = "status";

/// A verb as the C++ dispatch compares it with the whole message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    Whole(&'static str),
    /// `<family>:<word>`, as `noise:anc`.
    Family(&'static str, &'static str),
}

impl Verb {
    // Sample input: `noise:anc`, which is `Family("noise", "anc")`; `disconnect`, which is `Whole("disconnect")`.
    fn is(self, text: &str) -> bool {
        match self {
            Self::Whole(verb) => verb == text,
            Self::Family(family, word) => text.split_once(':') == Some((family, word)),
        }
    }
}

/// Every whole-word verb of the dispatch in `daemon/main.cpp`, in its order.
pub(crate) const VERBS: [(Verb, LegacyRequest); 17] = [
    (
        Verb::Whole(REOPEN_VERB),
        LegacyRequest::Command(Command::Reopen),
    ),
    noise(NoiseTarget::Mode(NoiseMode::Off)),
    noise(NoiseTarget::Mode(NoiseMode::Anc)),
    noise(NoiseTarget::Mode(NoiseMode::Transparency)),
    noise(NoiseTarget::Mode(NoiseMode::Adaptive)),
    noise(NoiseTarget::Cycle),
    ear(EarDetectionBehavior::Disabled),
    ear(EarDetectionBehavior::PauseWhenOneRemoved),
    ear(EarDetectionBehavior::PauseWhenBothRemoved),
    (
        Verb::Whole("forget"),
        LegacyRequest::Command(Command::Forget),
    ),
    (
        Verb::Family("ca", "on"),
        LegacyRequest::Command(Command::SetConversationalAwareness(true)),
    ),
    (
        Verb::Family("ca", "off"),
        LegacyRequest::Command(Command::SetConversationalAwareness(false)),
    ),
    (
        Verb::Whole("disconnect"),
        LegacyRequest::Command(Command::Disconnect),
    ),
    (
        Verb::Whole("connect"),
        LegacyRequest::Command(Command::Connect),
    ),
    (
        Verb::Family("onebud", "on"),
        LegacyRequest::Command(Command::SetOneBudAnc(true)),
    ),
    (
        Verb::Family("onebud", "off"),
        LegacyRequest::Command(Command::SetOneBudAnc(false)),
    ),
    (Verb::Whole(STATUS_VERB), LegacyRequest::Status),
];

const ADAPTIVE_PREFIX: &str = "adaptive:";

/// Qt's `toInt` skips these after the digits, and `parseIntVerb` refuses them before.
const QT_TRAILING_SPACE: [char; 6] = [' ', '\t', '\n', '\u{b}', '\u{c}', '\r'];

const fn noise(target: NoiseTarget) -> (Verb, LegacyRequest) {
    (
        Verb::Family("noise", target.word()),
        LegacyRequest::Command(Command::SetNoise(target)),
    )
}

const fn ear(behavior: EarDetectionBehavior) -> (Verb, LegacyRequest) {
    (
        Verb::Family("ear", behavior.word()),
        LegacyRequest::Command(Command::SetEarDetection(behavior)),
    )
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
        .find(|(verb, _)| verb.is(text))
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
        Err(_) => return Err(CommandError::BadValue),
    };
    let Ok(percent) = u8::try_from(value) else {
        return Err(CommandError::OutOfRange);
    };
    AdaptiveLevel::new(percent)
}

// Sample input: family `noise` with word `anc`, which is the verb `noise:anc`.
pub(crate) fn family_command(family: &str, word: &str) -> Option<Command> {
    VERBS
        .iter()
        .find_map(|&(verb, request)| match (verb, request) {
            (Verb::Family(at, said), LegacyRequest::Command(command))
                if (at, said) == (family, word) =>
            {
                Some(command)
            }
            (
                Verb::Family(..) | Verb::Whole(_),
                LegacyRequest::Command(_) | LegacyRequest::Status,
            ) => None,
        })
}

// Sample input: `disconnect`.
pub(crate) fn word_command(word: &str) -> Option<Command> {
    VERBS.iter().find_map(|&(verb, request)| match request {
        LegacyRequest::Command(command) if verb.is(word) => Some(command),
        LegacyRequest::Command(_) | LegacyRequest::Status => None,
    })
}

#[cfg(test)]
mod tests;
