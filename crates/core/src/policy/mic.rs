//! The microphone uplink machine: when a START and a STOP go out, what the pods' markers move, the restore after a STOP and the exit; stepped only by facts and its own deadline.

mod gate;
mod tries;

use std::time::Duration;

use pods_proto::uplink::{self, UplinkState};

use super::audio::{
    AUDIO_CALL_TIMEOUT_MS, AudioFact, AudioRequest, RESTORE_HOLD_MS, RestoreId, SinkState,
};
use super::{Effect, Fact, SleepEdge};
use crate::link::Signal;
use crate::prefs::MicPref;
use crate::state::{MicCode, State};
pub use gate::{BUD_GRACE_MS, STOP_WAITS_LINE, UPLINK_LINGER_MS};
use gate::{Hold, Next};
use tries::Try;
pub use tries::{START_TRIES, start_sent_line, unanswered_line};

/// About three times the slowest answer to a START measured on the pods.
pub const START_ANSWER_MS: u64 = 1500;
/// About three times the slowest answer to a STOP measured on the pods, rounded up.
pub const STOP_ANSWER_MS: u64 = 1500;
/// The restore is two audio server calls with the hold between them.
pub const RESTORE_WAIT_MS: u64 = 2 * AUDIO_CALL_TIMEOUT_MS + RESTORE_HOLD_MS;

pub const FAILED_LINE: &str = "mic: failed, the pods did not answer";
pub const NO_UPLINK_LINE: &str = "mic: this model has no uplink";
pub const STOP_OWED_LINE: &str = "mic: stop owed, the start is unanswered";
pub const STOP_UNCONFIRMED_LINE: &str = "mic: stop not confirmed";
pub const STOPPED_BY_THE_PODS_LINE: &str = "mic: stopped by the pods";

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
    /// `spent` says why no START goes out again.
    Stopped {
        spent: Option<Spent>,
    },
    /// The START of try `tried` went out at `since` and the pods have not said they run; `owed` once `off` or the exit asked for its STOP.
    Starting {
        since: Duration,
        tried: Try,
        owed: bool,
    },
    Running(Hold),
    /// A STOP went out at `since` and the pods have not said they stopped; `restore` unless the gate let it out with a bud worn.
    Stopping {
        since: Duration,
        restore: bool,
    },
    /// The sink's restore `id` was asked for at `since`; `unasked` while the pods stopped on their own and the want has not been absent since.
    Restoring {
        since: Duration,
        id: RestoreId,
        unasked: bool,
    },
}

/// Why a STOP went out; only `Quiet` needs no restore, as the sink of a worn bud was already suspended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Word {
    Quiet,
    NotWorn,
    Off,
    Sleep,
    Exit,
}

impl Word {
    const fn line(self) -> &'static str {
        match self {
            Self::Quiet => "mic: stop sent (quiet)",
            Self::NotWorn => "mic: stop sent (not_worn)",
            Self::Off => "mic: stop sent (off)",
            Self::Sleep => "mic: stop sent (sleep)",
            Self::Exit => "mic: stop sent (exit)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spent {
    /// A confirmed model did not answer; until the link is lost.
    Failed,
    /// A model never confirmed did not answer; until the link is lost.
    Unsupported,
    /// The pods stopped on their own; until the want is absent once.
    Unasked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Mic {
    phase: Phase,
    /// The followed sink is known to be suspended or absent; unknown counts as running.
    quiet: bool,
    exiting: bool,
    next_restore: u32,
    /// From logind's edge into sleep until its wake.
    asleep: bool,
    /// Every START whose answer deadline passed since the daemon started.
    start_failures: u32,
}

impl Default for Mic {
    fn default() -> Self {
        Self {
            phase: Phase::Stopped { spent: None },
            quiet: false,
            exiting: false,
            next_restore: 0,
            asleep: false,
            start_failures: 0,
        }
    }
}

impl Mic {
    /// `state` is the state after the step the fact comes from.
    pub(super) fn step(&mut self, state: &State, fact: &Fact, now: Duration) -> Vec<Effect> {
        let mut effects = Vec::new();
        match fact {
            Fact::Link(Signal::Down) => self.phase = Phase::Stopped { spent: None },
            Fact::Link(Signal::Lost { .. }) => {
                self.phase = Phase::Stopped { spent: None };
                self.quiet = false;
            }
            Fact::Link(Signal::Uplink(marker)) => self.marker(*marker, now, &mut effects),
            Fact::Tick => self.tick(state, now, &mut effects),
            Fact::Audio(fact) => self.audio(*fact),
            Fact::Sleep(edge) => self.sleep(*edge),
            Fact::Exit => self.exiting = true,
            Fact::Link(Signal::Up { .. } | Signal::Keys(_))
            | Fact::Changed
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
                Phase::Running(Hold::Wanted)
            }
            (Phase::Stopped { .. }, UplinkState::Running) => Phase::Running(Hold::Late),
            (Phase::Stopping { since, restore }, UplinkState::Stopped) => {
                let after = now.saturating_sub(since).as_millis();
                effects.push(Effect::Log(format!("mic: uplink stopped after {after} ms")));
                self.stopped(restore, now, effects)
            }
            // A pod is silent already, so the restore runs whatever the gate says.
            (Phase::Running(_), UplinkState::Stopped) => {
                effects.push(log(STOPPED_BY_THE_PODS_LINE));
                self.restore(true, now, effects)
            }
            (phase, _) => phase,
        };
    }

    /// A START out at the edge into sleep is dropped unanswered, as the suspend comes before its answer could.
    fn sleep(&mut self, edge: SleepEdge) {
        self.asleep = edge == SleepEdge::Sleeping;
        if self.asleep && matches!(self.phase, Phase::Starting { .. }) {
            self.phase = Phase::Stopped { spent: None };
        }
    }

    fn audio(&mut self, fact: AudioFact) {
        match fact {
            AudioFact::Up => {}
            AudioFact::Down => self.quiet = false,
            AudioFact::Sink(sink) => {
                self.quiet = sink.is_none_or(|sink| sink.state == SinkState::Suspended);
            }
            AudioFact::Restored(answered) => {
                if let Phase::Restoring { id, unasked, .. } = self.phase
                    && answered == id
                {
                    self.phase = restored(unasked);
                }
            }
        }
    }

    fn tick(&mut self, state: &State, now: Duration, effects: &mut Vec<Effect>) {
        if self.deadline().is_none_or(|deadline| now < deadline) {
            return;
        }
        self.phase = match self.phase {
            Phase::Starting { tried, .. } => {
                effects.push(Effect::Log(unanswered_line(tried.number())));
                self.start_failures = self.start_failures.saturating_add(1);
                self.unanswered(state, tried, now, effects)
            }
            Phase::Stopping { restore, .. } => {
                effects.push(log(STOP_UNCONFIRMED_LINE));
                self.stopped(restore, now, effects)
            }
            Phase::Restoring { unasked, .. } => restored(unasked),
            phase @ (Phase::Stopped { .. } | Phase::Running(_)) => phase,
        };
    }

    /// What the state after the fact asks of the uplink.
    fn follow(&mut self, state: &State, now: Duration, effects: &mut Vec<Effect>) {
        let off = state.microphone.preference() == MicPref::Off;
        match self.phase {
            Phase::Stopped { .. } if self.exiting => effects.push(Effect::Exited),
            Phase::Stopped {
                spent: Some(Spent::Unasked),
            } if !wanted(state) => self.phase = Phase::Stopped { spent: None },
            Phase::Stopped { spent: None } if wanted(state) && !self.asleep => {
                self.phase = start(Try::FIRST, now, effects);
            }
            // No STOP is ever written while a START is unanswered: the running marker sends it.
            Phase::Starting {
                since,
                tried,
                owed: false,
            } if off || self.exiting => {
                effects.push(log(STOP_OWED_LINE));
                self.phase = Phase::Starting {
                    since,
                    tried,
                    owed: true,
                };
            }
            Phase::Restoring {
                since,
                id,
                unasked: true,
            } if !wanted(state) => {
                self.phase = Phase::Restoring {
                    since,
                    id,
                    unasked: false,
                };
            }
            Phase::Running(hold) => self.running(hold, state, off, now, effects),
            Phase::Stopped { .. }
            | Phase::Starting { .. }
            | Phase::Stopping { .. }
            | Phase::Restoring { .. } => {}
        }
    }

    /// `off`, the exit and a sleep stop at once; any other end of the want goes through the linger and the gate.
    fn running(
        &mut self,
        hold: Hold,
        state: &State,
        off: bool,
        now: Duration,
        effects: &mut Vec<Effect>,
    ) {
        let next = if off {
            Next::Stop(Word::Off)
        } else if self.exiting {
            Next::Stop(Word::Exit)
        } else if self.asleep {
            Next::Stop(Word::Sleep)
        } else {
            hold.follow(wanted(state), state.ear.any_in_ear(), self.quiet, now)
        };
        self.phase = match next {
            Next::Hold(Hold::Waiting { unworn: None }) if !matches!(hold, Hold::Waiting { .. }) => {
                effects.push(log(STOP_WAITS_LINE));
                Phase::Running(Hold::Waiting { unworn: None })
            }
            Next::Hold(hold) => Phase::Running(hold),
            // The suspend comes before any answer could, so nothing waits for one.
            Next::Stop(Word::Sleep) => {
                effects.extend([Effect::Uplink(UplinkRequest::Stop), log(Word::Sleep.line())]);
                Phase::Stopped { spent: None }
            }
            Next::Stop(word) => {
                effects.extend([Effect::Uplink(UplinkRequest::Stop), log(word.line())]);
                Phase::Stopping {
                    since: now,
                    restore: word != Word::Quiet,
                }
            }
        };
    }

    /// The end of a STOP: the restore it is owed, or nothing.
    fn stopped(&mut self, restore: bool, now: Duration, effects: &mut Vec<Effect>) -> Phase {
        if !restore {
            return Phase::Stopped { spent: None };
        }
        self.restore(false, now, effects)
    }

    /// A restore under an id no earlier restore had.
    fn restore(&mut self, unasked: bool, now: Duration, effects: &mut Vec<Effect>) -> Phase {
        let id = RestoreId(self.next_restore);
        self.next_restore = self.next_restore.wrapping_add(1);
        effects.push(Effect::Audio(AudioRequest::Restore(id)));
        Phase::Restoring {
            since: now,
            id,
            unasked,
        }
    }

    /// Another try while the want holds and one is left; else the tries end, spent only after the last.
    fn unanswered(
        &self,
        state: &State,
        tried: Try,
        now: Duration,
        effects: &mut Vec<Effect>,
    ) -> Phase {
        if !wanted(state) || self.exiting {
            return Phase::Stopped { spent: None };
        }
        match tried.next() {
            Some(next) => start(next, now, effects),
            None => Phase::Stopped {
                spent: Some(spent(state, effects)),
            },
        }
    }

    pub(super) fn deadline(&self) -> Option<Duration> {
        match self.phase {
            Phase::Starting { since, .. } => {
                Some(since.saturating_add(Duration::from_millis(START_ANSWER_MS)))
            }
            Phase::Stopping { since, .. } => {
                Some(since.saturating_add(Duration::from_millis(STOP_ANSWER_MS)))
            }
            Phase::Restoring { since, .. } => {
                Some(since.saturating_add(Duration::from_millis(RESTORE_WAIT_MS)))
            }
            Phase::Running(hold) => hold.deadline(),
            Phase::Stopped { .. } => None,
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
            Phase::Running(_) => MicCode::Live,
            Phase::Stopped {
                spent: None | Some(Spent::Unasked),
            } => MicCode::Idle,
        }
    }

    /// Hands `Microphone::show` the code `mic` shows and the failures `mic_start_failures` counts.
    pub(super) fn show(&self, state: &mut State) {
        let code = self.code(state);
        state.microphone.show(code, self.start_failures);
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

fn start(tried: Try, now: Duration, effects: &mut Vec<Effect>) -> Phase {
    effects.extend([
        Effect::Uplink(UplinkRequest::Start),
        Effect::Audio(AudioRequest::CancelRestore),
        Effect::Log(start_sent_line(tried.number())),
    ]);
    Phase::Starting {
        since: now,
        tried,
        owed: false,
    }
}

fn restored(unasked: bool) -> Phase {
    Phase::Stopped {
        spent: unasked.then_some(Spent::Unasked),
    }
}

/// Why the last try went unanswered, and its line.
fn spent(state: &State, effects: &mut Vec<Effect>) -> Spent {
    if state.mic_models.contains(&state.model_number) {
        effects.push(log(FAILED_LINE));
        Spent::Failed
    } else {
        effects.push(log(NO_UPLINK_LINE));
        Spent::Unsupported
    }
}

fn log(line: &str) -> Effect {
    Effect::Log(line.to_owned())
}

#[cfg(test)]
mod tests;
