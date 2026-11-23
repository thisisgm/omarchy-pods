//! The microphone uplink machine: when a START goes out, what the pods' markers move, and `off`; stepped only by facts and its own deadline.

use std::time::Duration;

use pods_proto::uplink::{self, UplinkState};

use super::{Effect, Fact};
use crate::link::Signal;
use crate::prefs::MicPref;
use crate::state::{MicCode, State};

/// About three times the slowest answer to a START measured on the pods.
pub const START_ANSWER_MS: u64 = 1500;
/// About three times the slowest answer to a STOP measured on the pods, rounded up.
pub const STOP_ANSWER_MS: u64 = 1500;

pub const UNANSWERED_LINE: &str = "mic: start unanswered (try 1 of 1)";
pub const FAILED_LINE: &str = "mic: failed, the pods did not answer";
pub const NO_UPLINK_LINE: &str = "mic: this model has no uplink";
pub const STOP_OWED_LINE: &str = "mic: stop owed, the start is unanswered";
pub const STOP_UNCONFIRMED_LINE: &str = "mic: stop not confirmed";
const START_SENT_LINE: &str = "mic: start sent (try 1)";
const STOP_SENT_LINE: &str = "mic: stop sent (off)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UplinkRequest {
    Start,
    Stop,
}

impl UplinkRequest {
    #[must_use]
    pub const fn sdu(self) -> &'static [u8] {
        match self {
            Self::Start => &uplink::START,
            Self::Stop => &uplink::STOP,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// `spent` says why no START goes out again before the link is lost.
    Stopped {
        spent: Option<Spent>,
    },
    /// A START went out at `since` and the pods have not said they run; `owed` once `off` asked for its STOP.
    Starting {
        since: Duration,
        owed: bool,
    },
    Running,
    /// A STOP went out at `since` and the pods have not said they stopped.
    Stopping {
        since: Duration,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spent {
    /// A confirmed model did not answer.
    Failed,
    /// A model never confirmed did not answer.
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Mic {
    phase: Phase,
}

impl Default for Mic {
    fn default() -> Self {
        Self {
            phase: Phase::Stopped { spent: None },
        }
    }
}

impl Mic {
    /// `state` is the state after the step the fact comes from.
    pub(super) fn step(&mut self, state: &State, fact: &Fact, now: Duration) -> Vec<Effect> {
        let mut effects = Vec::new();
        match fact {
            Fact::Link(Signal::Down | Signal::Lost { .. }) => *self = Self::default(),
            Fact::Link(Signal::Uplink(marker)) => self.marker(*marker, now, &mut effects),
            Fact::Tick => self.tick(state, now, &mut effects),
            Fact::Link(Signal::Up { .. } | Signal::Keys(_))
            | Fact::Changed
            | Fact::Sleep(_)
            | Fact::Audio(_)
            | Fact::Paused(..)
            | Fact::Advert(_)
            | Fact::Monitor(_) => {}
        }
        self.follow(state, now, &mut effects);
        effects
    }

    /// The pods' word is the truth of whether the uplink runs, except that a START waits only for the running marker.
    fn marker(&mut self, marker: UplinkState, now: Duration, effects: &mut Vec<Effect>) {
        self.phase = match (self.phase, marker) {
            (Phase::Starting { since, .. }, UplinkState::Running) => {
                let after = now.saturating_sub(since).as_millis();
                effects.push(Effect::Log(format!("mic: uplink running after {after} ms")));
                Phase::Running
            }
            (Phase::Stopped { .. }, UplinkState::Running) => Phase::Running,
            (Phase::Stopping { since }, UplinkState::Stopped) => {
                let after = now.saturating_sub(since).as_millis();
                effects.push(Effect::Log(format!("mic: uplink stopped after {after} ms")));
                Phase::Stopped { spent: None }
            }
            (Phase::Running, UplinkState::Stopped) => Phase::Stopped { spent: None },
            (phase, _) => phase,
        };
    }

    fn tick(&mut self, state: &State, now: Duration, effects: &mut Vec<Effect>) {
        if self.deadline().is_none_or(|deadline| now < deadline) {
            return;
        }
        self.phase = match self.phase {
            Phase::Starting { .. } => {
                effects.push(log(UNANSWERED_LINE));
                Phase::Stopped {
                    spent: unanswered(state, effects),
                }
            }
            Phase::Stopping { .. } => {
                effects.push(log(STOP_UNCONFIRMED_LINE));
                Phase::Stopped { spent: None }
            }
            phase @ (Phase::Stopped { .. } | Phase::Running) => phase,
        };
    }

    /// What the state after the fact asks of the uplink.
    fn follow(&mut self, state: &State, now: Duration, effects: &mut Vec<Effect>) {
        let off = state.microphone.preference() == MicPref::Off;
        match self.phase {
            Phase::Stopped { spent: None } if wanted(state) => {
                effects.extend([Effect::Uplink(UplinkRequest::Start), log(START_SENT_LINE)]);
                self.phase = Phase::Starting {
                    since: now,
                    owed: false,
                };
            }
            // No STOP is ever written while a START is unanswered: the running marker sends it.
            Phase::Starting { since, owed: false } if off => {
                effects.push(log(STOP_OWED_LINE));
                self.phase = Phase::Starting { since, owed: true };
            }
            Phase::Running if off => {
                effects.extend([Effect::Uplink(UplinkRequest::Stop), log(STOP_SENT_LINE)]);
                self.phase = Phase::Stopping { since: now };
            }
            Phase::Stopped { .. }
            | Phase::Starting { .. }
            | Phase::Running
            | Phase::Stopping { .. } => {}
        }
    }

    pub(super) fn deadline(&self) -> Option<Duration> {
        match self.phase {
            Phase::Starting { since, .. } => {
                Some(since.saturating_add(Duration::from_millis(START_ANSWER_MS)))
            }
            Phase::Stopping { since } => {
                Some(since.saturating_add(Duration::from_millis(STOP_ANSWER_MS)))
            }
            Phase::Stopped { .. } | Phase::Running => None,
        }
    }

    /// What `mic` shows while the preference is not `off`, which `Microphone::show` alone decides.
    pub(super) fn code(&self, state: &State) -> MicCode {
        if !state.connected {
            return MicCode::Idle;
        }
        match self.phase {
            Phase::Stopped {
                spent: Some(Spent::Unsupported),
            } => MicCode::Unsupported,
            Phase::Stopped {
                spent: Some(Spent::Failed),
            } => MicCode::Failed,
            _ if !state.ear.any_in_ear() => MicCode::NotWorn,
            Phase::Starting { .. } => MicCode::Starting,
            Phase::Running => MicCode::Live,
            Phase::Stopped { spent: None } | Phase::Stopping { .. } => MicCode::Idle,
        }
    }
}

/// Whether the uplink is wanted: `auto` starts a model once, to confirm it, and both wait for the model a confirmation is kept under.
fn wanted(state: &State) -> bool {
    let preferred = match state.microphone.preference() {
        MicPref::Off => false,
        MicPref::Always => true,
        MicPref::Auto => !state.mic_models.contains(&state.model_number),
    };
    preferred && !state.model_number.is_empty() && state.connected && state.ear.any_in_ear()
}

/// Why a START that `off` did not cancel went unanswered, and its line.
fn unanswered(state: &State, effects: &mut Vec<Effect>) -> Option<Spent> {
    if state.microphone.preference() == MicPref::Off {
        return None;
    }
    if state.mic_models.contains(&state.model_number) {
        effects.push(log(FAILED_LINE));
        Some(Spent::Failed)
    } else {
        effects.push(log(NO_UPLINK_LINE));
        Some(Spent::Unsupported)
    }
}

fn log(line: &str) -> Effect {
    Effect::Log(line.to_owned())
}

#[cfg(test)]
mod tests;
