//! What a client can ask of the daemon, and what asking does to the state while there is no link.

use pods_proto::noise::NoiseMode;

use crate::state::{EarDetectionBehavior, State};

/// The highest adaptive noise level, the upper bound of the C++ `parseIntVerb` and `qBound`.
pub(crate) const ADAPTIVE_LEVEL_MAX: u8 = 100;

/// One request to change the pods or the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    SetNoise(NoiseTarget),
    SetEarDetection(EarDetectionBehavior),
    SetConversationalAwareness(bool),
    SetOneBudAnc(bool),
    /// Percent from 0 to 100; a larger value is taken as 100, as the C++ `qBound` takes it.
    SetAdaptiveLevel(u8),
    Connect,
    Disconnect,
    Forget,
    /// Open the window, which a headless daemon does not have.
    Reopen,
}

/// The listening mode a noise command asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseTarget {
    Mode(NoiseMode),
    /// The mode after the current one, in the order of `NoiseMode::next`.
    Cycle,
}

/// Why the daemon did not carry out a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandError {
    NotConnected,
    NotSupported,
    NotWorn,
    OutOfRange,
    UnknownKey,
    BadRequest,
    Timeout,
    Headless,
}

impl CommandError {
    /// The snake case name a client sees.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotConnected => "not_connected",
            Self::NotSupported => "not_supported",
            Self::NotWorn => "not_worn",
            Self::OutOfRange => "out_of_range",
            Self::UnknownKey => "unknown_key",
            Self::BadRequest => "bad_request",
            Self::Timeout => "timeout",
            Self::Headless => "headless",
        }
    }

    /// The sentence a person reads for this refusal, beside the code a program reads.
    #[must_use]
    pub fn message(&self) -> &'static str {
        match self {
            Self::NotConnected => "The AirPods are not connected.",
            Self::NotSupported => "These AirPods do not support that.",
            Self::NotWorn => "The AirPods are not in an ear.",
            Self::OutOfRange => "The value is outside the range the setting takes.",
            Self::UnknownKey => "There is no setting by that name.",
            Self::BadRequest => "The daemon could not read the request.",
            Self::Timeout => "The AirPods did not answer in time.",
            Self::Headless => "This daemon runs headless and has no window.",
        }
    }
}

/// How a command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Refused(CommandError),
    /// Handed to the pods, with no reply yet.
    Sent,
    /// Carried out, and the state shows it.
    Confirmed,
}

/// Applies `command` to `state` with no link: values and counters change only where the C++ setters change them.
pub fn apply(state: &mut State, command: &Command) -> Outcome {
    let counters = &mut state.counters;
    match *command {
        Command::SetNoise(target) => {
            let mode = match target {
                NoiseTarget::Mode(mode) => mode,
                NoiseTarget::Cycle => state.noise_mode.next(),
            };
            // The C++ counts the change before it learns that no packet went out.
            if mode != state.noise_mode {
                raise(&mut counters.noise_control_changes);
            }
            Outcome::Refused(CommandError::NotConnected)
        }
        Command::SetEarDetection(behavior) => {
            if behavior != state.ear_detection_behavior {
                state.ear_detection_behavior = behavior;
                raise(&mut counters.ear_detection_changes);
            }
            Outcome::Confirmed
        }
        Command::SetConversationalAwareness(enabled) => {
            if enabled != state.conversational_awareness {
                // corner: the C++ stores even when the send fails, and the pods' own report overwrites it on connect.
                state.conversational_awareness = enabled;
                raise(&mut counters.conversational_awareness_changes);
            }
            Outcome::Refused(CommandError::NotConnected)
        }
        Command::SetAdaptiveLevel(level) => {
            let level = level.min(ADAPTIVE_LEVEL_MAX);
            if level != state.adaptive_noise_level && state.noise_mode == NoiseMode::Adaptive {
                // corner: the C++ stores even when the send fails, and the pods' own report overwrites it on connect.
                state.adaptive_noise_level = level;
                raise(&mut counters.adaptive_level_changes);
            }
            Outcome::Refused(CommandError::NotConnected)
        }
        // The C++ counts one bud only after a packet goes out, and the rest only with a device address.
        Command::SetOneBudAnc(_) | Command::Connect | Command::Disconnect | Command::Forget => {
            Outcome::Refused(CommandError::NotConnected)
        }
        Command::Reopen => {
            raise(&mut counters.reopen_calls);
            Outcome::Refused(CommandError::Headless)
        }
    }
}

fn raise(counter: &mut u64) {
    *counter = counter.saturating_add(1);
}

#[cfg(test)]
mod tests;
