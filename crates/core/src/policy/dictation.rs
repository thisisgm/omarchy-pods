//! The dictation service as the policy and the adapter of the pods' attribute channel speak of it: what is asked of the channel, and what it tells back.

mod arm;
mod session;
#[cfg(test)]
mod tests;

use std::time::Duration;

use pods_proto::dictation::Event;
use pods_proto::doap::ArmFailure;

use super::route::Route;
use super::{Effect, Fact, SleepEdge};
use crate::link::Signal;
use crate::prefs::MicPref;
use crate::state::{MicDictation, State};
use arm::Arm;
use session::{Session, Stop};

/// The delays between attempts to open the attribute channel.
pub const BEARER_RETRY_MS: [u64; 3] = [750, 1500, 3000];
/// Sitting C found 70 ms was the shortest wait that streamed 10 of 10 starts, so 90 ms leaves one 20 ms frame of margin.
pub const MODE_SETTLE_MS: u64 = 90;
/// The most START may wait for logind after mode settling.
pub const INHIBIT_WAIT_MS: u64 = 100;
/// The longest wait for the started score or first audio.
pub const DOAP_START_ANSWER_MS: u64 = 500;
/// The longest wait for the streaming end after STOP.
pub const DOAP_STOP_ANSWER_MS: u64 = 500;
/// The initial START and one retry.
pub const DOAP_START_TRIES: u32 = 2;

/// What the policy asks for; the driver writes mode changes on the control channel and hands the rest to the attribute channel's adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationRequest {
    /// The setting that lets a start stream.
    Mode,
    /// Return the stem to its normal action when the stream ends.
    ModeOff,
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
    /// Audio on an armed bearer with no session open.
    UnsolicitedAudio,
    /// The audio notifications read since the daemon started, at every tick.
    Heard {
        packets: u64,
    },
}

/// Readies the service on the current control link and keeps it ready across recordings.
#[derive(Debug, Default)]
pub struct Dictation {
    arm: Arm,
    session: Session,
    asleep: bool,
    exiting: bool,
}

impl Dictation {
    #[must_use]
    pub fn step(&mut self, state: &State, fact: &Fact, now: Duration) -> Vec<Effect> {
        if let Fact::Sleep(edge) = fact {
            self.asleep = *edge == SleepEdge::Sleeping;
        }
        self.exiting |= matches!(fact, Fact::Exit);
        if self.exiting {
            if !self.active() {
                return self.arm.step(enabled(state), &Fact::Exit, now);
            }
            if matches!(fact, Fact::Exit) {
                return Vec::new();
            }
        }
        self.arm.step(enabled(state), fact, now)
    }

    pub(super) fn session(
        &mut self,
        state: &State,
        fact: &Fact,
        now: Duration,
        route: Option<Route>,
        effects: &mut Vec<Effect>,
    ) -> bool {
        if !self.armed() {
            self.session.reset(effects);
            if matches!(fact, Fact::Link(Signal::Down | Signal::Lost { .. })) {
                self.session = Session::default();
            }
            // The adapter closes the bearer before reporting its loss.
            return route == Some(Route::Dictation)
                && matches!(fact, Fact::Dictation(DictationFact::BearerLost));
        }
        let stop = if self.exiting {
            Some(Stop::Exit)
        } else if self.asleep {
            Some(Stop::Sleep)
        } else if route != Some(Route::Dictation) {
            Some(Stop::Left)
        } else {
            None
        };
        let fallback = self.session.step(fact, now, stop, effects);
        if self.exiting && !self.active() {
            effects.extend(self.arm.step(enabled(state), &Fact::Exit, now));
        }
        fallback
    }

    pub(super) fn active(&self) -> bool {
        self.session.code().is_some()
    }

    pub(super) fn set_inhibit_pending(&mut self, pending: bool) {
        self.session.set_inhibit_pending(pending);
    }

    #[must_use]
    pub fn deadline(&self) -> Option<Duration> {
        self.arm
            .deadline()
            .into_iter()
            .chain(self.session.deadline())
            .min()
    }

    #[must_use]
    pub fn armed(&self) -> bool {
        self.arm.armed()
    }

    pub(super) fn accepts_armed(&self, state: &State) -> bool {
        enabled(state) && self.arm.opening()
    }

    pub(super) fn code(&self, state: &State) -> MicDictation {
        self.session
            .code()
            .unwrap_or_else(|| self.arm.code(enabled(state)))
    }
}

fn enabled(state: &State) -> bool {
    state.connected && state.microphone.preference() != MicPref::Off
}
