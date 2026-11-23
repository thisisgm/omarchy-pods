//! The microphone uplink machine: when a START and a STOP go out, what the pods' markers move, the restore after a STOP and the exit; stepped only by facts and its own deadline.

use std::time::Duration;

use pods_proto::uplink::{self, UplinkState};

use super::audio::{AUDIO_CALL_TIMEOUT_MS, AudioFact, AudioRequest, RESTORE_HOLD_MS, RestoreId};
use super::{Effect, Fact};
use crate::link::Signal;
use crate::prefs::MicPref;
use crate::state::{MicCode, State};

/// About three times the slowest answer to a START measured on the pods.
pub const START_ANSWER_MS: u64 = 1500;
/// About three times the slowest answer to a STOP measured on the pods, rounded up.
pub const STOP_ANSWER_MS: u64 = 1500;
/// The restore is two audio server calls with the hold between them.
pub const RESTORE_WAIT_MS: u64 = 2 * AUDIO_CALL_TIMEOUT_MS + RESTORE_HOLD_MS;

pub const UNANSWERED_LINE: &str = "mic: start unanswered (try 1 of 1)";
pub const FAILED_LINE: &str = "mic: failed, the pods did not answer";
pub const NO_UPLINK_LINE: &str = "mic: this model has no uplink";
pub const STOP_OWED_LINE: &str = "mic: stop owed, the start is unanswered";
pub const STOP_UNCONFIRMED_LINE: &str = "mic: stop not confirmed";
const START_SENT_LINE: &str = "mic: start sent (try 1)";

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
    /// A START went out at `since` and the pods have not said they run; `owed` once `off` or the exit asked for its STOP.
    Starting {
        since: Duration,
        owed: bool,
    },
    Running,
    /// A STOP went out at `since` and the pods have not said they stopped.
    Stopping {
        since: Duration,
    },
    /// The sink's restore `id` was asked for at `since`.
    Restoring {
        since: Duration,
        id: RestoreId,
    },
}

/// Why a STOP went out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Word {
    Off,
    Exit,
}

impl Word {
    const fn line(self) -> &'static str {
        match self {
            Self::Off => "mic: stop sent (off)",
            Self::Exit => "mic: stop sent (exit)",
        }
    }
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
    exiting: bool,
    next_restore: u32,
}

impl Default for Mic {
    fn default() -> Self {
        Self {
            phase: Phase::Stopped { spent: None },
            exiting: false,
            next_restore: 0,
        }
    }
}

impl Mic {
    /// `state` is the state after the step the fact comes from.
    pub(super) fn step(&mut self, state: &State, fact: &Fact, now: Duration) -> Vec<Effect> {
        let mut effects = Vec::new();
        match fact {
            Fact::Link(Signal::Down | Signal::Lost { .. }) => {
                self.phase = Phase::Stopped { spent: None };
            }
            Fact::Link(Signal::Uplink(marker)) => self.marker(*marker, now, &mut effects),
            Fact::Tick => self.tick(state, now, &mut effects),
            Fact::Audio(fact) => self.audio(*fact),
            Fact::Exit => self.exiting = true,
            Fact::Link(Signal::Up { .. } | Signal::Keys(_))
            | Fact::Changed
            | Fact::Sleep(_)
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
                self.restoring(now, effects)
            }
            (Phase::Running, UplinkState::Stopped) => Phase::Stopped { spent: None },
            (phase, _) => phase,
        };
    }

    fn audio(&mut self, fact: AudioFact) {
        if let (AudioFact::Restored(answered), Phase::Restoring { id, .. }) = (fact, self.phase)
            && answered == id
        {
            self.phase = Phase::Stopped { spent: None };
        }
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
                self.restoring(now, effects)
            }
            Phase::Restoring { .. } => Phase::Stopped { spent: None },
            phase @ (Phase::Stopped { .. } | Phase::Running) => phase,
        };
    }

    /// What the state after the fact asks of the uplink.
    fn follow(&mut self, state: &State, now: Duration, effects: &mut Vec<Effect>) {
        let off = state.microphone.preference() == MicPref::Off;
        match self.phase {
            Phase::Stopped { .. } if self.exiting => effects.push(Effect::Exited),
            Phase::Stopped { spent: None } if wanted(state) => {
                effects.extend([
                    Effect::Uplink(UplinkRequest::Start),
                    Effect::Audio(AudioRequest::CancelRestore),
                    log(START_SENT_LINE),
                ]);
                self.phase = Phase::Starting {
                    since: now,
                    owed: false,
                };
            }
            // No STOP is ever written while a START is unanswered: the running marker sends it.
            Phase::Starting { since, owed: false } if off || self.exiting => {
                effects.push(log(STOP_OWED_LINE));
                self.phase = Phase::Starting { since, owed: true };
            }
            Phase::Running if off || self.exiting => {
                let word = if off { Word::Off } else { Word::Exit };
                effects.extend([Effect::Uplink(UplinkRequest::Stop), log(word.line())]);
                self.phase = Phase::Stopping { since: now };
            }
            Phase::Stopped { .. }
            | Phase::Starting { .. }
            | Phase::Running
            | Phase::Stopping { .. }
            | Phase::Restoring { .. } => {}
        }
    }

    /// The end of a STOP: the sink's restore, under an id no earlier restore had.
    fn restoring(&mut self, now: Duration, effects: &mut Vec<Effect>) -> Phase {
        let id = RestoreId(self.next_restore);
        self.next_restore = self.next_restore.wrapping_add(1);
        effects.push(Effect::Audio(AudioRequest::Restore(id)));
        Phase::Restoring { since: now, id }
    }

    pub(super) fn deadline(&self) -> Option<Duration> {
        match self.phase {
            Phase::Starting { since, .. } => {
                Some(since.saturating_add(Duration::from_millis(START_ANSWER_MS)))
            }
            Phase::Stopping { since } => {
                Some(since.saturating_add(Duration::from_millis(STOP_ANSWER_MS)))
            }
            Phase::Restoring { since, .. } => {
                Some(since.saturating_add(Duration::from_millis(RESTORE_WAIT_MS)))
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
            Phase::Stopping { .. } | Phase::Restoring { .. } => MicCode::Stopping,
            _ if !state.ear.any_in_ear() => MicCode::NotWorn,
            Phase::Starting { .. } => MicCode::Starting,
            Phase::Running => MicCode::Live,
            Phase::Stopped { spent: None } => MicCode::Idle,
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
