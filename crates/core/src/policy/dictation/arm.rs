//! Whether the attribute channel is ready and when to retry a failed bearer.

use std::time::Duration;

use pods_proto::doap::ArmFailure;

use super::{BEARER_RETRY_MS, DictationFact, DictationRequest};
use crate::link::Signal;
use crate::policy::{Effect, Fact};
use crate::state::MicDictation;

const BEARER_LOST: &str = "dictation: bearer lost, arming again";

#[derive(Debug, Default, Clone, Copy)]
pub(super) enum Arm {
    #[default]
    Offline,
    Disabled,
    Opening {
        since: Duration,
        retry: usize,
    },
    Waiting {
        since: Duration,
        retry: usize,
        at: Duration,
    },
    Armed,
    Unavailable,
    Exited,
}

impl Arm {
    pub(super) fn code(self, enabled: bool) -> MicDictation {
        if !enabled {
            return MicDictation::Off;
        }
        match self {
            Self::Offline | Self::Disabled | Self::Exited => MicDictation::Off,
            Self::Opening { .. } | Self::Waiting { .. } => MicDictation::Arming,
            Self::Armed => MicDictation::Armed,
            Self::Unavailable => MicDictation::Unavailable,
        }
    }

    pub(super) fn deadline(self) -> Option<Duration> {
        match self {
            Self::Waiting { at, .. } => Some(at),
            Self::Offline
            | Self::Disabled
            | Self::Opening { .. }
            | Self::Armed
            | Self::Unavailable
            | Self::Exited => None,
        }
    }

    pub(super) fn opening(self) -> bool {
        matches!(self, Self::Opening { .. })
    }

    pub(super) fn armed(self) -> bool {
        matches!(self, Self::Armed)
    }

    pub(super) fn step(&mut self, enabled: bool, fact: &Fact, now: Duration) -> Vec<Effect> {
        use DictationFact::{ArmFailed, Armed, BearerFailed, BearerLost};
        let close = matches!(self, Self::Opening { .. } | Self::Armed)
            .then_some(Effect::Dictation(DictationRequest::Close));
        let mut effects = Vec::new();
        *self = match (*self, fact) {
            (Self::Exited, _) | (_, Fact::Exit) => {
                effects.extend(close);
                Self::Exited
            }
            (_, Fact::Link(Signal::Down | Signal::Lost { .. })) => Self::Offline,
            (Self::Offline, Fact::Link(Signal::Up { .. })) if !enabled => Self::Disabled,
            (Self::Offline, _) if !enabled => Self::Offline,
            (Self::Unavailable, _) => Self::Unavailable,
            (_, _) if !enabled => {
                effects.extend(close);
                Self::Disabled
            }
            (Self::Offline, Fact::Link(Signal::Up { .. })) | (Self::Disabled, Fact::Changed) => {
                Self::start(now, 0, &mut effects)
            }
            (Self::Opening { since, .. }, Fact::Dictation(Armed)) => {
                let elapsed = now.saturating_sub(since).as_millis();
                effects.push(Effect::Log(format!("dictation: armed after {elapsed} ms")));
                Self::Armed
            }
            (Self::Opening { .. }, Fact::Dictation(ArmFailed(failure))) => {
                Self::unavailable(failure_word(*failure), &mut effects)
            }
            (Self::Opening { since, retry }, Fact::Dictation(BearerFailed)) => {
                Self::wait(since, retry, now, &mut effects)
            }
            (Self::Armed, Fact::Dictation(BearerLost)) => {
                effects.push(Effect::Log(BEARER_LOST.to_owned()));
                Self::wait(now, 0, now, &mut effects)
            }
            (Self::Waiting { since, retry, at }, Fact::Tick) if now >= at => {
                Self::start(since, retry, &mut effects)
            }
            (phase, _) => phase,
        };
        effects
    }

    fn start(since: Duration, retry: usize, effects: &mut Vec<Effect>) -> Self {
        effects.push(Effect::Dictation(DictationRequest::Arm));
        Self::Opening { since, retry }
    }

    fn wait(since: Duration, retry: usize, now: Duration, effects: &mut Vec<Effect>) -> Self {
        let Some(&delay) = BEARER_RETRY_MS.get(retry) else {
            return Self::unavailable("bearer", effects);
        };
        Self::Waiting {
            since,
            retry: retry.saturating_add(1),
            at: now.saturating_add(Duration::from_millis(delay)),
        }
    }

    fn unavailable(word: &str, effects: &mut Vec<Effect>) -> Self {
        effects.push(Effect::Log(format!("dictation: unavailable ({word})")));
        Self::Unavailable
    }
}

const fn failure_word(failure: ArmFailure) -> &'static str {
    match failure {
        ArmFailure::NoService | ArmFailure::MissingCharacteristic | ArmFailure::NoCccd => {
            "no_service"
        }
        ArmFailure::NoCodec => "no_codec",
        ArmFailure::Refused { .. } | ArmFailure::Garbled | ArmFailure::TooLong => "refused",
    }
}
