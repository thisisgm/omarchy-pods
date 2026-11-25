//! The microphone uplink machine: when a START and a STOP go out, what the pods' markers move, the restore after a STOP and the exit; stepped only by facts and its own deadline.

mod gate;
mod node;
mod tries;

use std::time::Duration;

use pods_proto::uplink::{self, UplinkState};

use super::audio::{
    AUDIO_CALL_TIMEOUT_MS, AudioFact, AudioRequest, RESTORE_HOLD_MS, Recorders, RestoreId,
    SinkState,
};
use super::{Effect, Fact, SleepEdge};
use crate::link::Signal;
use crate::prefs::MicPref;
use crate::state::{MicCode, State};
pub use gate::{BUD_GRACE_MS, STOP_OWED_PLAYING_LINE, STOP_WAITS_LINE, UPLINK_LINGER_MS};
use gate::{Hold, Next};
use node::Node;
pub use node::{MicFact, MicRequest, OpenId, SOURCE_BROKEN_LINE, SOURCE_UP_LINE, withdrawn_line};
use tries::{Read, Try, Watch};
pub use tries::{START_TRIES, UPLINK_STALL_MS, stalled_line, start_sent_line, unanswered_line};

/// The slowest answer to a START measured on the pods.
const SLOWEST_START_ANSWER_MS: u64 = 510;
/// About three times `SLOWEST_START_ANSWER_MS`.
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
pub const STOP_QUIET_LINE: &str = "mic: stop sent (quiet)";
pub const STOP_NOT_WORN_LINE: &str = "mic: stop sent (not_worn)";
pub const STOP_SLEEP_LINE: &str = "mic: stop sent (sleep)";
/// The start of the line dating the running marker after a START, which ends `<ms> ms`.
pub const RUNNING_AFTER_PREFIX: &str = "mic: uplink running after ";
/// The start of the line dating the first audio after a START, which ends `<ms> ms`.
pub const FIRST_AUDIO_PREFIX: &str = "mic: first audio after ";

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
    Running(Hold, Watch),
    /// A STOP went out at `since` and the pods have not said they stopped; `restore` says whether to restore the sink, and `spent` what the stop ends in.
    Stopping {
        since: Duration,
        restore: bool,
        spent: Option<Spent>,
    },
    /// The sink's restore `id` was asked for at `since`; `spent` is what it ends in, `Unasked` while the pods stopped on their own and the want has not been absent since.
    Restoring {
        since: Duration,
        id: RestoreId,
        spent: Option<Spent>,
    },
}

/// Why a STOP went out; `Quiet` needs no restore because the sink was already suspended or absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Word {
    Quiet,
    NotWorn,
    Sleep,
}

impl Word {
    const fn line(self) -> &'static str {
        match self {
            Self::Quiet => STOP_QUIET_LINE,
            Self::NotWorn => STOP_NOT_WORN_LINE,
            Self::Sleep => STOP_SLEEP_LINE,
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "Sink state, exit, sleep, and pending STOP confirmation are independent."
)]
pub(super) struct Mic {
    phase: Phase,
    /// A STOP whose deadline passed and whose stopped marker can still arrive.
    unconfirmed_stop: bool,
    /// The followed sink is known to be suspended or absent; unknown counts as running.
    quiet: bool,
    exiting: bool,
    next_restore: u32,
    /// From logind's edge into sleep until its wake.
    asleep: bool,
    /// Every START whose answer deadline passed, and every stall, since the daemon started.
    start_failures: u32,
    /// When the last START went out, until the first audio after it.
    armed: Option<Duration>,
    node: Node,
}

impl Default for Mic {
    fn default() -> Self {
        Self {
            phase: Phase::Stopped { spent: None },
            unconfirmed_stop: false,
            quiet: false,
            exiting: false,
            next_restore: 0,
            asleep: false,
            start_failures: 0,
            armed: None,
            node: Node::default(),
        }
    }
}

impl Mic {
    /// `state` is the state after the step the fact comes from.
    pub(super) fn step(&mut self, state: &State, fact: &Fact, now: Duration) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.node.step(state, fact, now, &mut effects);
        match fact {
            Fact::Link(Signal::Down) => {
                self.phase = Phase::Stopped { spent: None };
                self.unconfirmed_stop = false;
            }
            Fact::Link(Signal::Lost { .. }) => {
                self.phase = Phase::Stopped { spent: None };
                self.unconfirmed_stop = false;
                self.quiet = false;
            }
            Fact::Link(Signal::Uplink(marker)) => self.marker(*marker, now, &mut effects),
            Fact::Tick => self.tick(state, now, &mut effects),
            Fact::Audio(fact) => self.audio(*fact, &mut effects),
            Fact::Sleep(edge) => self.sleep(*edge),
            Fact::Exit => self.exiting = true,
            Fact::Mic(MicFact::FirstAudio) => self.first_audio(now, &mut effects),
            Fact::Mic(MicFact::Heard { packets }) => self.heard(state, *packets, now, &mut effects),
            Fact::Link(Signal::Up { .. } | Signal::Keys(_))
            | Fact::Changed
            | Fact::Paused(..)
            | Fact::PauseCut(_)
            | Fact::Advert(_)
            | Fact::Monitor(_)
            | Fact::Mic(_) => {}
        }
        self.follow(state, now, &mut effects);
        effects
    }

    /// The pods' word is the truth of whether the uplink runs, except that a START waits only for the running marker.
    fn marker(&mut self, marker: UplinkState, now: Duration, effects: &mut Vec<Effect>) {
        self.phase = match (self.phase, marker) {
            (Phase::Starting { since, tried, .. }, UplinkState::Running) => {
                self.unconfirmed_stop = false;
                let after = now.saturating_sub(since).as_millis();
                effects.push(Effect::Log(format!("{RUNNING_AFTER_PREFIX}{after} ms")));
                Phase::Running(Hold::Wanted, Watch::new(tried, now))
            }
            (Phase::Stopped { .. }, UplinkState::Running) => {
                self.armed = None;
                self.unconfirmed_stop = false;
                Phase::Running(Hold::Late, Watch::new(Try::FIRST, now))
            }
            (Phase::Restoring { .. }, UplinkState::Running) => {
                self.armed = None;
                self.unconfirmed_stop = false;
                effects.push(Effect::Audio(AudioRequest::CancelRestore));
                Phase::Running(Hold::Late, Watch::new(Try::FIRST, now))
            }
            (
                Phase::Stopping {
                    since,
                    restore,
                    spent,
                },
                UplinkState::Stopped,
            ) => {
                let after = now.saturating_sub(since).as_millis();
                effects.push(Effect::Log(format!("mic: uplink stopped after {after} ms")));
                self.stopped(restore, spent, now, effects)
            }
            // A pod is silent already, so the restore runs whatever the gate says.
            (Phase::Running(..), UplinkState::Stopped) => {
                effects.push(log(STOPPED_BY_THE_PODS_LINE));
                self.restore(Some(Spent::Unasked), now, effects)
            }
            (Phase::Stopped { spent } | Phase::Restoring { spent, .. }, UplinkState::Stopped)
                if self.unconfirmed_stop =>
            {
                self.unconfirmed_stop = false;
                if !self.asleep && !self.quiet {
                    self.restore(spent, now, effects)
                } else {
                    self.phase
                }
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

    fn audio(&mut self, fact: AudioFact, effects: &mut Vec<Effect>) {
        match fact {
            AudioFact::Up => {}
            AudioFact::Down => self.quiet = false,
            AudioFact::Sink(sink) => {
                self.quiet = sink.is_none_or(|sink| sink.state == SinkState::Suspended);
            }
            AudioFact::Restored(answered) => {
                if let Phase::Restoring { id, spent, .. } = self.phase
                    && answered == id
                {
                    self.phase = Phase::Stopped { spent };
                }
            }
            AudioFact::Recorders(Recorders { dictation, other }) => {
                effects.push(Effect::Log(format!(
                    "mic: recorders (dictation {dictation}, other {other})"
                )));
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
            Phase::Stopping { restore, spent, .. } => {
                effects.push(log(STOP_UNCONFIRMED_LINE));
                self.unconfirmed_stop = true;
                self.stopped(restore, spent, now, effects)
            }
            Phase::Restoring { spent, .. } => Phase::Stopped { spent },
            phase @ (Phase::Stopped { .. } | Phase::Running(..)) => phase,
        };
    }

    /// What the state after the fact asks of the uplink.
    fn follow(&mut self, state: &State, now: Duration, effects: &mut Vec<Effect>) {
        let off = state.microphone.preference() == MicPref::Off;
        match self.phase {
            Phase::Stopped { .. } if self.exiting => effects.push(Effect::Exited),
            Phase::Stopped {
                spent: Some(Spent::Unasked),
            } if !self.wants(state) => self.phase = Phase::Stopped { spent: None },
            Phase::Stopped { spent: None } if self.wants(state) && !self.asleep => {
                self.phase = self.start(Try::FIRST, now, effects);
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
                spent: Some(Spent::Unasked),
            } if !self.wants(state) => {
                self.phase = Phase::Restoring {
                    since,
                    id,
                    spent: None,
                };
            }
            Phase::Running(hold, watch) => {
                let watch = watch.follow(state.ear.any_in_ear(), now);
                self.running(hold, watch, state, off, now, effects);
            }
            Phase::Stopped { .. }
            | Phase::Starting { .. }
            | Phase::Stopping { .. }
            | Phase::Restoring { .. } => {}
        }
    }

    /// `off` and the exit owe the STOP to the gate with no linger, a sleep sends one at once through an open gate, and any other end of the want lingers first.
    fn running(
        &mut self,
        hold: Hold,
        watch: Watch,
        state: &State,
        off: bool,
        now: Duration,
        effects: &mut Vec<Effect>,
    ) {
        let in_ear = state.ear.any_in_ear();
        let next = if off || self.exiting {
            hold.owed(in_ear, self.quiet, now)
        } else if self.asleep && gate::open(in_ear, self.quiet).is_some() {
            Next::Stop(Word::Sleep)
        } else {
            hold.follow(self.wants(state), in_ear, self.quiet, now)
        };
        self.phase = match next {
            // No STOP overtakes an unanswered START sent by the stall check.
            Next::Stop(_) if watch.retrying() => {
                if !watch.owes() {
                    effects.push(log(STOP_OWED_LINE));
                }
                Phase::Running(hold, watch.owe())
            }
            Next::Hold(Hold::Waiting { unworn: None }) if !matches!(hold, Hold::Waiting { .. }) => {
                let owed = off || self.exiting;
                effects.push(log(if owed {
                    STOP_OWED_PLAYING_LINE
                } else {
                    STOP_WAITS_LINE
                }));
                Phase::Running(Hold::Waiting { unworn: None }, watch)
            }
            Next::Hold(hold) => Phase::Running(hold, watch),
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
                    spent: None,
                }
            }
        };
    }

    /// The end of a STOP: the restore it is owed, or nothing.
    fn stopped(
        &mut self,
        restore: bool,
        spent: Option<Spent>,
        now: Duration,
        effects: &mut Vec<Effect>,
    ) -> Phase {
        if !restore {
            return Phase::Stopped { spent };
        }
        self.restore(spent, now, effects)
    }

    /// A restore under an id no earlier restore had.
    fn restore(&mut self, spent: Option<Spent>, now: Duration, effects: &mut Vec<Effect>) -> Phase {
        let id = RestoreId(self.next_restore);
        self.next_restore = self.next_restore.wrapping_add(1);
        effects.push(Effect::Audio(AudioRequest::Restore(id)));
        Phase::Restoring {
            since: now,
            id,
            spent,
        }
    }

    /// Another try while the want holds and one is left; else the tries end, spent only after the last.
    fn unanswered(
        &mut self,
        state: &State,
        tried: Try,
        now: Duration,
        effects: &mut Vec<Effect>,
    ) -> Phase {
        if !self.wants(state) || self.exiting {
            return Phase::Stopped { spent: None };
        }
        match tried.next() {
            Some(next) => self.start(next, now, effects),
            None => Phase::Stopped {
                spent: Some(spent(state, effects)),
            },
        }
    }

    fn start(&mut self, tried: Try, now: Duration, effects: &mut Vec<Effect>) -> Phase {
        self.unconfirmed_stop = false;
        effects.push(Effect::Audio(AudioRequest::CancelRestore));
        self.send_start(tried, now, effects);
        Phase::Starting {
            since: now,
            tried,
            owed: false,
        }
    }

    fn send_start(&mut self, tried: Try, now: Duration, effects: &mut Vec<Effect>) {
        effects.extend([
            Effect::Uplink(UplinkRequest::Start),
            Effect::Log(start_sent_line(tried.number())),
        ]);
        self.armed = Some(now);
    }

    fn first_audio(&mut self, now: Duration, effects: &mut Vec<Effect>) {
        if let Some(sent) = self.armed.take() {
            let after = now.saturating_sub(sent).as_millis();
            effects.push(Effect::Log(format!("{FIRST_AUDIO_PREFIX}{after} ms")));
        }
    }

    /// The stall check: while the uplink is wanted, a count that did not move is a try gone unanswered, as `unanswered` counts one.
    fn heard(&mut self, state: &State, packets: u64, now: Duration, effects: &mut Vec<Effect>) {
        let Phase::Running(hold, watch) = self.phase else {
            return;
        };
        let Some(read) = watch.read(packets, now) else {
            return;
        };
        self.phase = match read {
            Read::Stalled {
                watch,
                for_ms,
                next,
            } if self.wants(state) && !self.asleep && !self.exiting => {
                effects.push(Effect::Log(stalled_line(for_ms)));
                self.start_failures = self.start_failures.saturating_add(1);
                if let Some(next) = next {
                    self.send_start(next, now, effects);
                    Phase::Running(hold, watch.retry(next, now))
                } else {
                    // The pods said they run, so the uplink is stopped and the sink restored before `mic` reads the tries spent.
                    let spent = spent(state, effects);
                    effects.push(Effect::Uplink(UplinkRequest::Stop));
                    Phase::Stopping {
                        since: now,
                        restore: true,
                        spent: Some(spent),
                    }
                }
            }
            // Only a wanted uplink on an awake, running daemon can spend a try on a stall.
            Read::Moved(watch) | Read::Stalled { watch, .. } => Phase::Running(hold, watch),
        };
    }

    pub(super) fn deadline(&self) -> Option<Duration> {
        let uplink = match self.phase {
            Phase::Starting { since, .. } => {
                Some(since.saturating_add(Duration::from_millis(START_ANSWER_MS)))
            }
            Phase::Stopping { since, .. } => {
                Some(since.saturating_add(Duration::from_millis(STOP_ANSWER_MS)))
            }
            Phase::Restoring { since, .. } => {
                Some(since.saturating_add(Duration::from_millis(RESTORE_WAIT_MS)))
            }
            Phase::Running(hold, watch) => {
                hold.deadline().into_iter().chain(watch.deadline()).min()
            }
            Phase::Stopped { .. } => None,
        };
        uplink.into_iter().chain(self.node.deadline()).min()
    }

    /// A recorder linked to the node wants the uplink as `always` does.
    fn wants(&self, state: &State) -> bool {
        wanted(state) || (self.node.recording() && state.ear.any_in_ear())
    }

    /// What `mic` shows while the preference is not `off`, which `Microphone::show` alone decides.
    pub(super) fn code(&self, state: &State) -> MicCode {
        if self.node.broken() {
            return MicCode::LibraryError;
        }
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
            Phase::Running(..) if self.node.recording() => MicCode::Recording,
            Phase::Running(..) => MicCode::Live,
            Phase::Stopped {
                spent: None | Some(Spent::Unasked),
            } => self.node.resting(),
        }
    }

    pub(super) fn recording(&self) -> bool {
        self.node.recording()
    }

    /// Whether a due STOP still waits for the gate, which an exit out of its bound says in its line.
    pub(super) fn stop_waits(&self) -> bool {
        matches!(self.phase, Phase::Running(Hold::Waiting { .. }, _))
    }

    pub(super) fn show(&self, state: &mut State) {
        let code = self.code(state);
        state
            .microphone
            .show(code, self.start_failures, self.node.up());
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
