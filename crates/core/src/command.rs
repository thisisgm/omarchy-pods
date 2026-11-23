//! Client commands, and what each does to the state while there is no link.

use pods_proto::noise::NoiseMode;

use crate::prefs::MicPref;
use crate::state::{EarDetectionBehavior, State};

/// The upper bound of the C++ `parseIntVerb` and `qBound`.
pub(crate) const ADAPTIVE_LEVEL_MAX: u8 = 100;

/// The C++ `m_adaptiveNoiseLevel` initial value.
const INITIAL_ADAPTIVE_LEVEL: u8 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdaptiveLevel(u8);

impl AdaptiveLevel {
    pub fn new(percent: u8) -> Result<Self, CommandError> {
        if percent <= ADAPTIVE_LEVEL_MAX {
            Ok(Self(percent))
        } else {
            Err(CommandError::OutOfRange)
        }
    }

    #[must_use]
    pub fn percent(self) -> u8 {
        self.0
    }
}

impl Default for AdaptiveLevel {
    fn default() -> Self {
        Self(INITIAL_ADAPTIVE_LEVEL)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    SetNoise(NoiseTarget),
    SetEarDetection(EarDetectionBehavior),
    SetConversationalAwareness(bool),
    SetOneBudAnc(bool),
    SetAdaptiveLevel(AdaptiveLevel),
    SetNotifications(bool),
    SetMicrophone(MicPref),
    Connect,
    Disconnect,
    Forget,
    /// Open the window; a headless daemon has none.
    Reopen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseTarget {
    Mode(NoiseMode),
    /// The next mode, in the order of `NoiseMode::next`.
    Cycle,
}

impl NoiseTarget {
    /// The word after `noise:`, which `set noise` also takes and the `NoiseMode` property shows.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Mode(NoiseMode::Off) => "off",
            Self::Mode(NoiseMode::Anc) => "anc",
            Self::Mode(NoiseMode::Transparency) => "transparency",
            Self::Mode(NoiseMode::Adaptive) => "adaptive",
            Self::Cycle => "cycle",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandError {
    NotConnected,
    NotSupported,
    NotWorn,
    OutOfRange,
    BadValue,
    UnknownKey,
    BadRequest,
    Timeout,
    Headless,
    NoDevice,
}

impl CommandError {
    pub const ALL: [Self; 10] = [
        Self::NotConnected,
        Self::NotSupported,
        Self::NotWorn,
        Self::OutOfRange,
        Self::BadValue,
        Self::UnknownKey,
        Self::BadRequest,
        Self::Timeout,
        Self::Headless,
        Self::NoDevice,
    ];

    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotConnected => "not_connected",
            Self::NotSupported => "not_supported",
            Self::NotWorn => "not_worn",
            Self::OutOfRange => "out_of_range",
            Self::BadValue => "bad_value",
            Self::UnknownKey => "unknown_key",
            Self::BadRequest => "bad_request",
            Self::Timeout => "timeout",
            Self::Headless => "headless",
            Self::NoDevice => "no_device",
        }
    }

    #[must_use]
    pub fn message(&self) -> &'static str {
        match self {
            Self::NotConnected => "The AirPods are not connected.",
            Self::NotSupported => "These AirPods do not support that.",
            Self::NotWorn => "The AirPods are not in an ear.",
            Self::OutOfRange => "The value is outside the range the setting takes.",
            Self::BadValue => "That setting does not take this value.",
            Self::UnknownKey => "There is no setting by that name.",
            Self::BadRequest => "The daemon could not read the request.",
            Self::Timeout => "The AirPods did not answer in time.",
            Self::Headless => "This daemon runs headless and has no window.",
            Self::NoDevice => "There are no AirPods to act on.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Refused(CommandError),
    /// Handed to the pods, with no reply yet.
    Sent,
    /// Carried out, and the state shows it.
    Confirmed,
}

impl Outcome {
    pub const fn name(self) -> Result<&'static str, CommandError> {
        match self {
            Self::Sent => Ok("sent"),
            Self::Confirmed => Ok("confirmed"),
            Self::Refused(error) => Err(error),
        }
    }
}

/// With no link, values and counters change only where the C++ setters change them.
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
        Command::SetNotifications(enabled) => {
            state.notifications = enabled;
            Outcome::Confirmed
        }
        Command::SetMicrophone(preference) => {
            state.microphone.set_preference(preference);
            Outcome::Confirmed
        }
        Command::SetConversationalAwareness(enabled) => {
            if enabled != state.conversational_awareness {
                // corner: stored even on a failed send, as in C++; the pods' report replaces it.
                state.conversational_awareness = enabled;
                raise(&mut counters.conversational_awareness_changes);
            }
            Outcome::Refused(CommandError::NotConnected)
        }
        Command::SetAdaptiveLevel(level) => {
            if level != state.adaptive_noise_level && state.noise_mode == NoiseMode::Adaptive {
                // corner: stored even on a failed send, as in C++; the pods' report replaces it.
                state.adaptive_noise_level = level;
                raise(&mut counters.adaptive_level_changes);
            }
            Outcome::Refused(CommandError::NotConnected)
        }
        // The C++ counts one bud only after a send, and the rest only with a device address.
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
pub(crate) mod tests;
