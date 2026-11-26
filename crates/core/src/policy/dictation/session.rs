//! The `DoAP` stream inside the armed channel, from the mode write to the end.

use std::time::Duration;

use pods_proto::dictation::Event;
use pods_proto::doap::{STOP_REASON_HOST, STOP_REASON_UNSOLICITED};

use super::{
    DOAP_START_ANSWER_MS, DOAP_START_TRIES, DOAP_STOP_ANSWER_MS, DictationFact, DictationRequest,
    INHIBIT_WAIT_MS, MODE_SETTLE_MS,
};
use crate::policy::{Effect, Fact};
use crate::state::MicDictation;

#[derive(Debug, Clone, Copy)]
pub(super) enum Stop {
    Left,
    Sleep,
    Exit,
}

impl Stop {
    fn word(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Sleep => "sleep",
            Self::Exit => "exit",
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct Session {
    phase: Phase,
    inhibit: Inhibit,
    stray_logged: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Inhibit {
    #[default]
    Resolved,
    Pending,
    TimedOut,
}

#[derive(Debug, Clone, Copy, Default)]
enum Phase {
    #[default]
    Idle,
    Settling {
        since: Duration,
        tried: u32,
    },
    Starting {
        since: Duration,
        tried: u32,
        owed: Option<Stop>,
    },
    Streaming {
        since: Duration,
        tried: u32,
    },
    Stopping {
        since: Duration,
    },
    Spent,
}

impl Phase {
    fn deadline(self, inhibit: Inhibit) -> Option<Duration> {
        let (since, wait) = match self {
            Self::Settling { since, .. } if inhibit != Inhibit::Resolved => {
                (since, MODE_SETTLE_MS + INHIBIT_WAIT_MS)
            }
            Self::Settling { since, .. } => (since, MODE_SETTLE_MS),
            Self::Starting { since, .. } => (since, DOAP_START_ANSWER_MS),
            Self::Stopping { since } => (since, DOAP_STOP_ANSWER_MS),
            Self::Idle | Self::Streaming { .. } | Self::Spent => return None,
        };
        Some(since.saturating_add(Duration::from_millis(wait)))
    }
}

impl Session {
    pub(super) fn code(&self) -> Option<MicDictation> {
        match self.phase {
            Phase::Idle | Phase::Spent => None,
            Phase::Settling { .. } | Phase::Starting { owed: None, .. } => {
                Some(MicDictation::Starting)
            }
            Phase::Streaming { .. } => Some(MicDictation::Streaming),
            Phase::Starting { owed: Some(_), .. } | Phase::Stopping { .. } => {
                Some(MicDictation::Stopping)
            }
        }
    }

    pub(super) fn deadline(&self) -> Option<Duration> {
        self.phase.deadline(self.inhibit)
    }

    pub(super) fn set_inhibit_pending(&mut self, pending: bool) {
        self.inhibit = if pending {
            Inhibit::Pending
        } else {
            Inhibit::Resolved
        };
    }

    pub(super) fn reset(&mut self, effects: &mut Vec<Effect>) {
        if self.code().is_some() {
            request(effects, DictationRequest::ModeOff);
        }
        self.phase = Phase::Idle;
        self.inhibit = Inhibit::Resolved;
    }

    /// A live span falls back once when its last try is spent.
    pub(super) fn step(
        &mut self,
        fact: &Fact,
        now: Duration,
        stop: Option<Stop>,
        effects: &mut Vec<Effect>,
    ) -> bool {
        use DictationFact::{Ended, FirstAudio, PodEvent};
        let told = if let Fact::Dictation(told) = fact {
            Some(told)
        } else {
            None
        };
        let timed_out = self.deadline().is_some_and(|at| now >= at);
        self.note_event(told, effects);
        let was_spent = matches!(self.phase, Phase::Spent);
        self.phase = match self.phase {
            Phase::Idle | Phase::Spent if stop.is_some() => Phase::Idle,
            Phase::Settling { .. } if stop.is_some() => mode_off(effects),
            Phase::Settling { tried, .. } if timed_out => {
                if self.inhibit == Inhibit::Pending {
                    log(
                        effects,
                        "dictation: logind inhibitor timed out, starting without it",
                    );
                    self.inhibit = Inhibit::TimedOut;
                }
                send_start(tried, now, effects)
            }
            Phase::Starting { since, tried, owed } => {
                Self::starting(since, tried, owed.or(stop), told, now, effects)
            }
            Phase::Streaming { tried, .. }
                if matches!(told, Some(Ended | PodEvent(Event::SiriCancel))) =>
            {
                log(effects, "dictation: stopped by the pods");
                mode_off(effects);
                if stop.is_none() && tried < DOAP_START_TRIES {
                    mode_on(tried.saturating_add(1), now, effects)
                } else {
                    Phase::Spent
                }
            }
            Phase::Streaming { .. } if let Some(why) = stop => send_stop(why, now, effects),
            Phase::Streaming { since, .. } if matches!(told, Some(FirstAudio)) => {
                first_audio(effects, now.saturating_sub(since).as_millis());
                self.phase
            }
            Phase::Stopping { .. } if timed_out => {
                log(effects, "dictation: stop not confirmed");
                mode_off(effects)
            }
            Phase::Stopping { since } if matches!(told, Some(Ended)) => {
                let after = now.saturating_sub(since).as_millis();
                log(effects, format!("dictation: stopped after {after} ms"));
                mode_off(effects)
            }
            phase @ (Phase::Idle
            | Phase::Settling { .. }
            | Phase::Streaming { .. }
            | Phase::Stopping { .. }
            | Phase::Spent) => phase,
        };
        if matches!(self.phase, Phase::Idle) && stop.is_none() {
            self.phase = mode_on(1, now, effects);
        } else if matches!(self.phase, Phase::Idle | Phase::Spent) {
            self.inhibit = Inhibit::Resolved;
        }
        !was_spent && stop.is_none() && matches!(self.phase, Phase::Spent)
    }

    fn starting(
        since: Duration,
        tried: u32,
        owed: Option<Stop>,
        told: Option<&DictationFact>,
        now: Duration,
        effects: &mut Vec<Effect>,
    ) -> Phase {
        use DictationFact::{Answered, FirstAudio, Refused};
        let after = now.saturating_sub(since);
        match told {
            Some(Answered | FirstAudio) => {
                let after = after.as_millis();
                log(effects, format!("dictation: started after {after} ms"));
                if matches!(told, Some(FirstAudio)) {
                    first_audio(effects, after);
                }
                owed.map_or(Phase::Streaming { since, tried }, |why| {
                    send_stop(why, now, effects)
                })
            }
            _ if after >= Duration::from_millis(DOAP_START_ANSWER_MS) => {
                log(effects, "dictation: start not confirmed");
                request(
                    effects,
                    DictationRequest::Stop {
                        reason: STOP_REASON_HOST,
                    },
                );
                if owed.is_some() {
                    mode_off(effects)
                } else if tried < DOAP_START_TRIES {
                    mode_on(tried.saturating_add(1), now, effects)
                } else {
                    mode_off(effects);
                    Phase::Spent
                }
            }
            Some(Refused {
                reason: STOP_REASON_UNSOLICITED,
            }) if owed.is_none() => {
                if tried < DOAP_START_TRIES {
                    log(
                        effects,
                        format!(
                            "dictation: refused (reason {STOP_REASON_UNSOLICITED}), mode sent again"
                        ),
                    );
                    mode_on(tried.saturating_add(1), now, effects)
                } else {
                    mode_off(effects);
                    log(effects, "dictation: last start refused");
                    Phase::Spent
                }
            }
            Some(Refused { .. }) if owed.is_some() => mode_off(effects),
            _ => Phase::Starting { since, tried, owed },
        }
    }

    fn note_event(&mut self, fact: Option<&DictationFact>, effects: &mut Vec<Effect>) {
        if let Some(DictationFact::PodEvent(event)) = fact {
            log(effects, format!("dictation: event {event:?}"));
        } else if let Some(DictationFact::Refused { reason }) = fact
            && *reason != STOP_REASON_UNSOLICITED
        {
            log(effects, format!("dictation: refused (reason {reason})"));
        } else if matches!(fact, Some(DictationFact::UnsolicitedAudio)) {
            request(
                effects,
                DictationRequest::Stop {
                    reason: STOP_REASON_UNSOLICITED,
                },
            );
            if !self.stray_logged {
                log(effects, "dictation: audio with no session, stopped");
                self.stray_logged = true;
            }
        }
    }
}

fn request(effects: &mut Vec<Effect>, request: DictationRequest) {
    effects.push(Effect::Dictation(request));
}

fn log(effects: &mut Vec<Effect>, line: impl Into<String>) {
    effects.push(Effect::Log(line.into()));
}

fn mode_on(tried: u32, now: Duration, effects: &mut Vec<Effect>) -> Phase {
    request(effects, DictationRequest::Mode);
    Phase::Settling { since: now, tried }
}

fn mode_off(effects: &mut Vec<Effect>) -> Phase {
    request(effects, DictationRequest::ModeOff);
    Phase::Idle
}

fn send_start(tried: u32, now: Duration, effects: &mut Vec<Effect>) -> Phase {
    request(effects, DictationRequest::Start);
    log(effects, format!("dictation: start sent (try {tried})"));
    Phase::Starting {
        since: now,
        tried,
        owed: None,
    }
}

fn send_stop(why: Stop, now: Duration, effects: &mut Vec<Effect>) -> Phase {
    request(
        effects,
        DictationRequest::Stop {
            reason: STOP_REASON_HOST,
        },
    );
    log(effects, format!("dictation: stop sent ({})", why.word()));
    Phase::Stopping { since: now }
}

fn first_audio(effects: &mut Vec<Effect>, after: u128) {
    log(effects, format!("dictation: first audio after {after} ms"));
}
