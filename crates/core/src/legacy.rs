//! The verbs the C++ daemon accepts on its control socket, one verb per connection.

use pods_proto::noise::NoiseMode;

use crate::command::{ADAPTIVE_LEVEL_MAX, Command, NoiseTarget};
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
const VERBS: [(&str, LegacyRequest); 17] = [
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
        let level = adaptive_level(payload)?;
        return Some(LegacyRequest::Command(Command::SetAdaptiveLevel(level)));
    }
    VERBS
        .iter()
        .find(|(verb, _)| *verb == text)
        .map(|&(_, request)| request)
}

/// The level in an `adaptive:` payload, by the rule of `parseIntVerb` over Qt's `toInt`.
// Sample input: `37`, `+37`, `-0` or `37\n`; ` 37`, `37x` and `101` are refused.
fn adaptive_level(payload: &str) -> Option<u8> {
    let value: i32 = payload.trim_end_matches(QT_TRAILING_SPACE).parse().ok()?;
    u8::try_from(value)
        .ok()
        .filter(|level| *level <= ADAPTIVE_LEVEL_MAX)
}

#[cfg(test)]
mod tests;
