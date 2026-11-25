//! The dictation service as the policy and the adapter of the pods' attribute channel speak of it: what is asked of the channel, and what it tells back.

mod arm;
#[cfg(test)]
mod tests;

use std::time::Duration;

use pods_proto::dictation::Event;
use pods_proto::doap::ArmFailure;

use super::{Effect, Fact};
use crate::prefs::MicPref;
use crate::state::{MicDictation, State};
use arm::Arm;

/// The delays between attempts to open the attribute channel.
pub const BEARER_RETRY_MS: [u64; 3] = [750, 1500, 3000];
/// Provisional time from setting `0x13` to START, based on sitting A.
pub const MODE_SETTLE_MS: u64 = 150;

/// What the policy asks for; the driver writes `Mode` on the control channel and hands the rest to the attribute channel's adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationRequest {
    /// The setting that lets a start stream.
    Mode,
    /// Open the attribute channel and subscribe to the service, over any channel still open; told back as `Armed`, `ArmFailed` or `BearerFailed`.
    Arm,
    /// Sent only on an armed channel.
    Start,
    /// Sent only on an armed channel.
    Stop { reason: u8 },
    /// Drop the attribute channel; nothing is told back.
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictationFact {
    Armed,
    /// The table or the pods refused the arm, and the channel was dropped.
    ArmFailed(ArmFailure),
    /// The channel did not open, or it ended, went silent or took no write before the arm was done.
    BearerFailed,
    /// The armed channel ended or took no write; told once.
    BearerLost,
    /// The pods answered a start the host asked for with the started score (`Event::STARTED`).
    Answered,
    /// The pods' stop notification; reason 6 refuses a start.
    Refused {
        reason: u8,
    },
    /// The stream ended, after a stop or by the pods.
    Ended,
    /// Any other event of the service.
    PodEvent(Event),
    /// The first audio since the last start went out.
    FirstAudio,
    /// The audio notifications read since the daemon started, at every tick.
    Heard {
        packets: u64,
    },
}

/// Readies the service on the current control link and keeps it ready across recordings.
#[derive(Debug, Default)]
pub struct Dictation {
    arm: Arm,
}

impl Dictation {
    #[must_use]
    pub fn step(&mut self, state: &State, fact: &Fact, now: Duration) -> Vec<Effect> {
        self.arm.step(enabled(state), fact, now)
    }

    #[must_use]
    pub fn deadline(&self) -> Option<Duration> {
        self.arm.deadline()
    }

    #[must_use]
    pub fn armed(&self) -> bool {
        self.arm.armed()
    }

    pub(super) fn accepts_armed(&self, state: &State) -> bool {
        enabled(state) && self.arm.opening()
    }

    pub(super) fn code(&self, state: &State) -> MicDictation {
        self.arm.code(enabled(state))
    }
}

fn enabled(state: &State) -> bool {
    state.connected && state.microphone.preference() != MicPref::Off
}
