//! The `DoAP` stream inside the armed channel, from the mode write to the end.

use std::time::Duration;

use pods_proto::dictation::Event;
use pods_proto::doap::{STOP_REASON_HOST, STOP_REASON_UNSOLICITED};

use super::{
    DOAP_START_ANSWER_MS, DOAP_START_TRIES, DOAP_STOP_ANSWER_MS, DictationFact, DictationRequest,
    MODE_SETTLE_MS,
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
    refused_once: bool,
    stray_logged: bool,
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
    fn deadline(self) -> Option<Duration> {
        let (since, wait) = match self {
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
        self.phase.deadline()
    }

    pub(super) fn reset(&mut self, effects: &mut Vec<Effect>) {
        if self.code().is_some() {
            request(effects, DictationRequest::ModeOff);
        }
        self.phase = Phase::Idle;
    }

    /// Returns true only when a second refusal hands this edge to the uplink.
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
        let mut fallback = false;
        self.phase = match self.phase {
            Phase::Idle | Phase::Spent if stop.is_some() => Phase::Idle,
            Phase::Settling { .. } if stop.is_some() => mode_off(effects),
            Phase::Settling { tried, .. } if timed_out => send_start(tried, now, effects),
            Phase::Starting { since, tried, owed } => {
                let (next, refused_twice) =
                    self.starting(since, tried, owed.or(stop), told, now, effects);
                fallback = refused_twice;
                next
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
            self.refused_once = false;
            self.phase = mode_on(1, now, effects);
        }
        fallback
    }

    fn starting(
        &mut self,
        since: Duration,
        tried: u32,
        owed: Option<Stop>,
        told: Option<&DictationFact>,
        now: Duration,
        effects: &mut Vec<Effect>,
    ) -> (Phase, bool) {
        use DictationFact::{Answered, FirstAudio, Refused};
        let after = now.saturating_sub(since);
        let mut fallback = false;
        let phase = match told {
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
                    self.refused_once = true;
                    log(
                        effects,
                        format!(
                            "dictation: refused (reason {STOP_REASON_UNSOLICITED}), mode sent again"
                        ),
                    );
                    mode_on(tried.saturating_add(1), now, effects)
                } else {
                    mode_off(effects);
                    fallback = self.refused_once;
                    let line = if fallback {
                        "dictation: refused twice, this recording takes the uplink"
                    } else {
                        "dictation: last start refused"
                    };
                    log(effects, line);
                    Phase::Spent
                }
            }
            Some(Refused { .. }) if owed.is_some() => mode_off(effects),
            _ => Phase::Starting { since, tried, owed },
        };
        (phase, fallback)
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
