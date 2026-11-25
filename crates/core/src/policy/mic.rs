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
use super::dictation::Dictation;
use super::route::Route;
use super::{Effect, Fact, SleepEdge};
use crate::link::Signal;
use crate::prefs::MicPref;
use crate::state::{AudioServer, MicCode, MicDictation, State};
pub use gate::{BUD_GRACE_MS, STOP_OWED_PLAYING_LINE, STOP_WAITS_LINE, UPLINK_LINGER_MS};
use gate::{Hold, Next};
use node::Node;
pub use node::{MicFact, MicRequest, OpenId, SOURCE_BROKEN_LINE, SOURCE_UP_LINE, withdrawn_line};
use tries::{Read, Try, Watch};
pub use tries::{START_TRIES, UPLINK_STALL_MS, stalled_line, start_sent_line, unanswered_line};

/// The slowest measured START answer, from sitting H1's slow tail.
const SLOWEST_START_ANSWER_MS: u64 = 610;
/// About two and a half times `SLOWEST_START_ANSWER_MS`.
pub const START_ANSWER_MS: u64 = 1500;
/// One stall window of grace after the normal START answer deadline, measured from the send.
const LATE_START_ANSWER_MS: u64 = START_ANSWER_MS + UPLINK_STALL_MS;
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
    Stopped(Stopped),
    /// The START of try `tried` went out at `since` and the pods have not said they run; `owed` once `off` or the exit asked for its STOP.
    Starting {
        since: Duration,
        tried: Try,
        owed: bool,
    },
    Running(Hold, Watch),
    /// The STOP sent at `since` still awaits confirmation. `restore` records whether the gate or a sink wake called for a reset.
    Stopping {
        since: Duration,
        restore: bool,
        spent: Option<Spent>,
    },
    /// The uplink is stopped and the sink restore `id` requested at `since` still awaits completion.
    Restoring {
        since: Duration,
        id: RestoreId,
        stopped: Stopped,
    },
}

impl Phase {
    fn stopped_mut(&mut self) -> Option<&mut Stopped> {
        match self {
            Self::Stopped(stopped) | Self::Restoring { stopped, .. } => Some(stopped),
            Self::Starting { .. } | Self::Running(..) | Self::Stopping { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Stopped {
    /// Why a new START is held back.
    spent: Option<Spent>,
    pending: PendingMarker,
}

impl Stopped {
    fn after_start(spent: Option<Spent>, started_at: Duration) -> Self {
        let until = started_at.saturating_add(Duration::from_millis(LATE_START_ANSWER_MS));
        Self {
            spent,
            pending: PendingMarker::StartUntil(until),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PendingMarker {
    #[default]
    None,
    /// The owned STOP timed out, but a late confirmation can still restore the sink.
    Stop,
    /// A regular START whose wait ended can still own the next span.
    StartUntil(Duration),
    /// A stall's START can own the next span if its running marker arrives before this deadline.
    RetryUntil(Duration),
}

/// Why a STOP went out; only `Quiet` starts without an owed restore because the sink was already suspended or absent.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Audio {
    /// The followed sink is known to be suspended or absent; unknown counts as running.
    quiet: bool,
    hint: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Mic {
    phase: Phase,
    audio: Audio,
    exiting: bool,
    next_restore: u32,
    /// From logind's edge into sleep until its wake.
    asleep: bool,
    /// Every START whose answer deadline passed, and every stall, since the daemon started.
    start_failures: u32,
    /// When the last START went out, until the first audio after it.
    armed: Option<Duration>,
    node: Node,
    route: Option<Route>,
    dictation_armed: bool,
}

impl Default for Mic {
    fn default() -> Self {
        Self {
            phase: Phase::Stopped(Stopped::default()),
            audio: Audio::default(),
            exiting: false,
            next_restore: 0,
            asleep: false,
            start_failures: 0,
            armed: None,
            node: Node::default(),
            route: None,
            dictation_armed: false,
        }
    }
}

impl Mic {
    #[cfg(test)]
    fn step(&mut self, state: &State, fact: &Fact, now: Duration) -> Vec<Effect> {
        self.step_with_dictation(state, fact, now, &Dictation::default())
    }

    #[cfg(test)]
    fn step_with_dictation(
        &mut self,
        state: &State,
        fact: &Fact,
        now: Duration,
        dictation: &Dictation,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.step_node(state, fact, now, &mut effects);
        let route = self.recording().then_some(Route::Uplink);
        self.step_uplink(state, fact, now, dictation, route, &mut effects);
        effects
    }

    pub(super) fn step_node(
        &mut self,
        state: &State,
        fact: &Fact,
        now: Duration,
        effects: &mut Vec<Effect>,
    ) -> (Recorders, bool) {
        self.node.step(state, fact, now, effects);
        self.node.recorders()
    }

    /// `state` is the state after the step the fact comes from.
    pub(super) fn step_uplink(
        &mut self,
        state: &State,
        fact: &Fact,
        now: Duration,
        dictation: &Dictation,
        route: Option<Route>,
        effects: &mut Vec<Effect>,
    ) {
        self.route = route;
        self.dictation_armed = dictation.armed();
        match fact {
            Fact::Link(Signal::Down) => self.down(now, effects),
            Fact::Link(Signal::Lost { .. }) => {
                self.phase = Phase::Stopped(Stopped::default());
                self.audio = Audio::default();
            }
            Fact::Link(Signal::Uplink(marker)) => self.marker(*marker, now, effects),
            Fact::Tick => self.tick(state, now, effects),
            Fact::Audio(fact) => self.audio(*fact, state.audio_server, effects),
            Fact::Sleep(edge) => self.sleep(*edge),
            Fact::Exit => self.exiting = true,
            Fact::Mic(MicFact::FirstAudio) => self.first_audio(now, effects),
            Fact::Mic(MicFact::Heard { packets }) => self.heard(state, *packets, now, effects),
            Fact::Link(Signal::Up { .. } | Signal::Keys(_) | Signal::Awareness(_))
            | Fact::Changed
            | Fact::Paused(..)
            | Fact::PauseCut(_)
            | Fact::Advert(_)
            | Fact::Monitor(_)
            | Fact::Mic(_)
            | Fact::Dictation(_) => {}
        }
        // A successful arm confirms an auto model without the uplink probe.
        let probe_waits = dictation.code(state) == MicDictation::Arming
            && state.microphone.preference() == MicPref::Auto
            && !self.audio.hint
            && !self.node.recording();
        self.follow(state, now, probe_waits, effects);
    }

    /// A channel drop can leave the device and its sink up, so an owed restore still runs.
    fn down(&mut self, now: Duration, effects: &mut Vec<Effect>) {
        let stopped = Stopped::default();
        self.phase = match self.phase {
            Phase::Stopping { restore: true, .. } => self.restore(stopped, now, effects),
            // The pods can start the uplink before the running marker arrives.
            Phase::Starting { .. } | Phase::Running(..) if !self.audio.quiet && !self.asleep => {
                self.restore(stopped, now, effects)
            }
            Phase::Restoring { since, id, .. } => Phase::Restoring { since, id, stopped },
            Phase::Stopped(_)
            | Phase::Starting { .. }
            | Phase::Running(..)
            | Phase::Stopping { restore: false, .. } => Phase::Stopped(stopped),
        };
    }

    /// Running answers a daemon START, including a late answer inside its window.
    fn marker(&mut self, marker: UplinkState, now: Duration, effects: &mut Vec<Effect>) {
        self.phase = match (self.phase, marker) {
            (Phase::Starting { since, tried, .. }, UplinkState::Running) => {
                let after = now.saturating_sub(since).as_millis();
                effects.push(Effect::Log(format!("{RUNNING_AFTER_PREFIX}{after} ms")));
                Phase::Running(Hold::Wanted, Watch::new(tried, now))
            }
            (Phase::Stopped(stopped) | Phase::Restoring { stopped, .. }, UplinkState::Running) => {
                let owned = matches!(
                    stopped.pending,
                    PendingMarker::StartUntil(until) | PendingMarker::RetryUntil(until) if now < until
                );
                if !owned {
                    return;
                }
                self.armed = None;
                if matches!(self.phase, Phase::Restoring { .. }) {
                    effects.push(Effect::Audio(AudioRequest::CancelRestore));
                }
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
                let stopped = Stopped {
                    spent,
                    pending: PendingMarker::None,
                };
                self.finish_stop(restore, stopped, now, effects)
            }
            // The stopped marker bypasses the playback gate, but a headset profile still blocks a restore.
            (Phase::Running(_, watch), UplinkState::Stopped) => {
                effects.push(log(STOPPED_BY_THE_PODS_LINE));
                let stopped = Stopped {
                    spent: Some(Spent::Unasked),
                    pending: watch
                        .answer_by()
                        .map_or(PendingMarker::None, PendingMarker::RetryUntil),
                };
                self.restore(stopped, now, effects)
            }
            (mut phase, UplinkState::Stopped) => {
                let Some(stopped) = phase.stopped_mut() else {
                    return;
                };
                if stopped.pending != PendingMarker::Stop {
                    return;
                }
                stopped.pending = PendingMarker::None;
                if !self.asleep && !self.audio.quiet && !self.node.blocked() {
                    self.restore(*stopped, now, effects)
                } else {
                    phase
                }
            }
            (phase, _) => phase,
        };
    }

    /// A START out at the edge into sleep is dropped unanswered, as the suspend comes before its answer could.
    fn sleep(&mut self, edge: SleepEdge) {
        self.asleep = edge == SleepEdge::Sleeping;
        if self.asleep {
            if let Phase::Starting { since, .. } = self.phase {
                self.phase = Phase::Stopped(Stopped::after_start(None, since));
            } else if let Some(stopped) = self.phase.stopped_mut()
                && matches!(stopped.pending, PendingMarker::RetryUntil(_))
            {
                stopped.pending = PendingMarker::None;
            }
        }
    }

    fn audio(&mut self, fact: AudioFact, server: AudioServer, effects: &mut Vec<Effect>) {
        match fact {
            AudioFact::Up
            | AudioFact::Headset(_)
            | AudioFact::Recording(_)
            | AudioFact::VolumeAdjusted
            | AudioFact::VolumeSkipped(_) => {}
            AudioFact::Down => {
                self.set_quiet(false);
                self.audio.hint = false;
            }
            AudioFact::Hint(hint) => self.audio.hint = server != AudioServer::Idle && hint,
            AudioFact::Sink(sink) => {
                self.set_quiet(sink.is_none_or(|sink| sink.state == SinkState::Suspended));
            }
            AudioFact::Restored(answered) => {
                if let Phase::Restoring { id, stopped, .. } = self.phase
                    && answered == id
                {
                    self.phase = Phase::Stopped(stopped);
                }
            }
            AudioFact::Recorders(Recorders { dictation, other }) => {
                effects.push(Effect::Log(format!(
                    "mic: recorders (dictation {dictation}, other {other})"
                )));
            }
        }
    }

    fn set_quiet(&mut self, quiet: bool) {
        self.audio.quiet = quiet;
        if !quiet && let Phase::Stopping { restore, .. } = &mut self.phase {
            *restore = true;
        }
    }

    fn tick(&mut self, state: &State, now: Duration, effects: &mut Vec<Effect>) {
        if self.deadline().is_none_or(|deadline| now < deadline) {
            return;
        }
        self.phase = match self.phase {
            Phase::Starting { since, tried, .. } => {
                effects.push(Effect::Log(unanswered_line(tried.number())));
                self.start_failures = self.start_failures.saturating_add(1);
                self.unanswered(state, tried, since, now, effects)
            }
            Phase::Stopping { restore, spent, .. } => {
                effects.push(log(STOP_UNCONFIRMED_LINE));
                let stopped = Stopped {
                    spent,
                    pending: PendingMarker::Stop,
                };
                self.finish_stop(restore, stopped, now, effects)
            }
            Phase::Restoring { stopped, .. } => Phase::Stopped(stopped),
            phase @ (Phase::Stopped(_) | Phase::Running(..)) => phase,
        };
    }

    /// What the state after the fact asks of the uplink.
    fn follow(
        &mut self,
        state: &State,
        now: Duration,
        probe_waits: bool,
        effects: &mut Vec<Effect>,
    ) {
        let off = state.microphone.preference() == MicPref::Off;
        match self.phase {
            Phase::Stopped(_) if self.exiting => effects.push(Effect::Exited),
            Phase::Stopped(Stopped { spent: None, .. })
                if !probe_waits && self.wants(state) && !self.asleep =>
            {
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
            Phase::Running(hold, watch) if self.node.blocked() => {
                self.phase = Phase::Running(hold, watch.follow(false, now));
            }
            Phase::Running(hold, watch) => {
                let watch = watch.follow(state.ear.any_in_ear(), now);
                self.running(hold, watch, state, now, effects);
            }
            Phase::Stopped(_) | Phase::Restoring { .. } => {
                if !self.uplink_wanted(state)
                    && let Some(stopped) = self.phase.stopped_mut()
                    && stopped.spent == Some(Spent::Unasked)
                {
                    stopped.spent = None;
                }
            }
            Phase::Starting { .. } | Phase::Stopping { .. } => {}
        }
    }

    /// `off` and the exit owe the STOP to the gate with no linger, a sleep sends one at once through an open gate, and any other end of the want lingers first.
    fn running(
        &mut self,
        hold: Hold,
        watch: Watch,
        state: &State,
        now: Duration,
        effects: &mut Vec<Effect>,
    ) {
        let off = state.microphone.preference() == MicPref::Off;
        let in_ear = state.ear.any_in_ear();
        let next = if off || self.exiting {
            hold.owed(in_ear, self.audio.quiet, now)
        } else if self.asleep && gate::open(in_ear, self.audio.quiet).is_some() {
            Next::Stop(Word::Sleep)
        } else {
            hold.follow(self.uplink_wanted(state), in_ear, self.audio.quiet, now)
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
                Phase::Stopped(Stopped::default())
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
    fn finish_stop(
        &mut self,
        restore: bool,
        stopped: Stopped,
        now: Duration,
        effects: &mut Vec<Effect>,
    ) -> Phase {
        if !restore {
            return Phase::Stopped(stopped);
        }
        self.restore(stopped, now, effects)
    }

    /// A restore for an owned uplink under a fresh id, provided the active profile allows it.
    fn restore(&mut self, stopped: Stopped, now: Duration, effects: &mut Vec<Effect>) -> Phase {
        if self.node.blocked() {
            return Phase::Stopped(stopped);
        }
        let id = RestoreId(self.next_restore);
        self.next_restore = self.next_restore.wrapping_add(1);
        effects.push(Effect::Audio(AudioRequest::Restore(id)));
        Phase::Restoring {
            since: now,
            id,
            stopped,
        }
    }

    /// Another try while the want holds and one is left; else the tries end, spent only after the last.
    fn unanswered(
        &mut self,
        state: &State,
        tried: Try,
        started_at: Duration,
        now: Duration,
        effects: &mut Vec<Effect>,
    ) -> Phase {
        let spent = if !self.uplink_wanted(state) || self.exiting {
            None
        } else {
            match tried.next() {
                Some(next) if !self.node.awaiting_profile(state.audio_server) => {
                    return self.start(next, now, effects);
                }
                Some(_) => None,
                None => Some(spent(state, effects)),
            }
        };
        Phase::Stopped(Stopped::after_start(spent, started_at))
    }

    fn start(&mut self, tried: Try, now: Duration, effects: &mut Vec<Effect>) -> Phase {
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
            } if self.uplink_wanted(state)
                && !self.asleep
                && !self.exiting
                && (next.is_none() || !self.node.awaiting_profile(state.audio_server)) =>
            {
                effects.push(Effect::Log(stalled_line(for_ms)));
                self.start_failures = self.start_failures.saturating_add(1);
                if let Some(next) = next {
                    effects.push(Effect::Audio(AudioRequest::CancelRestore));
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
            Phase::Running(..) if self.node.blocked() => None,
            Phase::Running(hold, watch) => {
                hold.deadline().into_iter().chain(watch.deadline()).min()
            }
            Phase::Stopped(_) => None,
        };
        uplink.into_iter().chain(self.node.deadline()).min()
    }

    fn wants(&self, state: &State) -> bool {
        !self.node.awaiting_profile(state.audio_server) && self.uplink_wanted(state)
    }

    /// An uplink recording, or a fast-capture hint with dictation neither armed nor routed, can hold the uplink under `auto`.
    fn uplink_wanted(&self, state: &State) -> bool {
        let hint = self.audio.hint && !self.dictation_armed && self.route != Some(Route::Dictation);
        !self.node.blocked()
            && (wanted(state, hint)
                || (self.route == Some(Route::Uplink) && state.ear.any_in_ear()))
    }

    /// The uplink's status, including a pending STOP after the preference is turned off.
    pub(super) fn code(&self, state: &State) -> MicCode {
        if state.connected
            && state.microphone.preference() == MicPref::Off
            && matches!(
                self.phase,
                Phase::Starting { owed: true, .. }
                    | Phase::Running(..)
                    | Phase::Stopping { .. }
                    | Phase::Restoring { .. }
            )
        {
            return MicCode::Stopping;
        }
        if self.node.blocked() && state.connected {
            return MicCode::Blocked;
        }
        if self.node.broken() {
            return MicCode::LibraryError;
        }
        if !state.connected {
            return MicCode::Idle;
        }
        if self.route == Some(Route::Dictation) && self.node.recording() {
            return MicCode::Recording;
        }
        match self.phase {
            Phase::Stopped(Stopped {
                spent: Some(Spent::Unsupported),
                ..
            }) => MicCode::Unsupported,
            Phase::Stopped(Stopped {
                spent: Some(Spent::Failed),
                ..
            }) => MicCode::Failed,
            Phase::Stopping { .. } | Phase::Restoring { .. } => MicCode::Stopping,
            _ if !state.ear.any_in_ear() => MicCode::NotWorn,
            Phase::Starting { .. } => MicCode::Starting,
            Phase::Running(..) if self.node.recording() => MicCode::Recording,
            Phase::Running(..) => MicCode::Live,
            Phase::Stopped(Stopped {
                spent: None | Some(Spent::Unasked),
                ..
            }) => self.node.resting(),
        }
    }

    pub(super) fn recording(&self) -> bool {
        self.node.recording()
    }

    /// Whether a due STOP still waits for the gate, which an exit out of its bound says in its line.
    pub(super) fn stop_waits(&self) -> bool {
        matches!(self.phase, Phase::Running(Hold::Waiting { .. }, ..))
    }

    pub(super) fn show(&self, state: &mut State) {
        let code = self.code(state);
        state
            .microphone
            .show(code, self.start_failures, self.node.up());
    }
}

/// Whether the uplink is wanted: `auto` starts a model once, to confirm it, and both wait for the model a confirmation is kept under.
fn wanted(state: &State, capture: bool) -> bool {
    let preferred = match state.microphone.preference() {
        MicPref::Off => false,
        MicPref::Always => true,
        MicPref::Auto => capture || !state.mic_models.contains(&state.model_number),
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
