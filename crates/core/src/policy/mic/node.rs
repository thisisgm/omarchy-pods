//! The microphone's source node: when the daemon asks the audio server for it, its name, and what its adapter reports of it.

use std::time::Duration;

use super::BUD_GRACE_MS;
use crate::link::Signal;
use crate::policy::audio::{AudioFact, Recorders};
use crate::policy::{Effect, Fact};
use crate::prefs::MicPref;
use crate::state::{AudioServer, MicCode, State};

const DESCRIPTION_SUFFIX: &str = " Microphone";
/// The description of a model with no display name.
const FALLBACK_DESCRIPTION: &str = "AirPods Microphone";
/// Recheck a linked, unworn node after its grace, even when the transport has no timer left.
const SILENCE_CHECK_INTERVAL: Duration = Duration::from_millis(100);

pub const SOURCE_UP_LINE: &str = "mic: source up";
pub const SOURCE_BROKEN_LINE: &str = "mic: source failed in the library";

#[must_use]
pub fn withdrawn_line(word: &str) -> String {
    format!("mic: source withdrawn ({word})")
}

/// Which open a fact answers, so a fact of an earlier source changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OpenId(pub(super) u32);

/// What the node's adapter learned of the node it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicFact {
    Up(OpenId),
    /// The server is absent, refused the node or ended it; the adapter tries again while the node is asked for.
    Down(OpenId),
    /// The library failed before any contact with a server; nothing tries again until the daemon restarts.
    Broken,
    /// Whether the `PipeWire` graph runs the source.
    Recording(OpenId, bool),
    /// The first audio packet since the last START.
    FirstAudio,
    /// The audio packets read since the daemon started, at a tick of the clock.
    Heard {
        packets: u64,
    },
    /// The recorder has received a quiet stretch and no speech is waiting in the chain.
    OutputSilent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicRequest {
    Open {
        id: OpenId,
        name: String,
        description: String,
    },
    Close,
}

/// The node asked for and what the adapter said of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Source {
    /// No `Open` is out.
    #[default]
    Closed,
    Opening(OpenId),
    Up {
        id: OpenId,
        capture: Capture,
    },
    Down(OpenId),
    /// The library failed, so nothing opens again.
    Broken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capture {
    Paused,
    Recording,
    /// The graph runs the source while the recorder list is empty.
    ListEnded,
}

impl Source {
    /// The open a fact must answer to count, while one is out.
    fn id(self) -> Option<OpenId> {
        match self {
            Self::Opening(id) | Self::Up { id, .. } | Self::Down(id) => Some(id),
            Self::Closed | Self::Broken => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Buds {
    #[default]
    Out,
    Worn,
    /// The last bud left at this time, and its grace runs.
    Left(Duration),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum CardProfile {
    #[default]
    Unread,
    Headset,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Node {
    source: Source,
    next_id: u32,
    exiting: bool,
    card_profile: CardProfile,
    buds: Buds,
    recorders: Recorders,
    recorders_fresh: bool,
    silence_check: Option<Duration>,
}

impl Node {
    /// `state` is the state after the step the fact comes from.
    pub(super) fn step(
        &mut self,
        state: &State,
        fact: &Fact,
        now: Duration,
        effects: &mut Vec<Effect>,
    ) {
        let was_recording = self.recording();
        match fact {
            Fact::Mic(fact) => self.heard(*fact, effects),
            Fact::Exit => self.exiting = true,
            Fact::Audio(AudioFact::Recorders(recorders)) => self.listed(*recorders),
            Fact::Audio(AudioFact::Headset(headset)) => {
                self.card_profile = match (state.audio_server, *headset) {
                    (AudioServer::Idle, _) => CardProfile::Unread,
                    (_, true) => CardProfile::Headset,
                    (_, false) => CardProfile::Other,
                };
            }
            Fact::Link(Signal::Lost { .. }) | Fact::Audio(AudioFact::Down) => {
                self.card_profile = CardProfile::Unread;
            }
            Fact::Link(_)
            | Fact::Changed
            | Fact::Sleep(_)
            | Fact::Tick
            | Fact::Audio(_)
            | Fact::Paused(..)
            | Fact::PauseCut(_)
            | Fact::Advert(_)
            | Fact::Monitor(_)
            | Fact::Dictation(_) => {}
        }
        self.follow_buds(state, now);
        let silent = matches!(fact, Fact::Mic(MicFact::OutputSilent));
        match (self.wanted(state, silent), self.source) {
            (true, Source::Closed) => {
                let id = OpenId(self.next_id);
                self.next_id = self.next_id.wrapping_add(1);
                effects.push(Effect::Mic(open(id, state)));
                self.source = Source::Opening(id);
            }
            (false, source @ (Source::Opening(_) | Source::Up { .. } | Source::Down(_))) => {
                effects.push(Effect::Mic(MicRequest::Close));
                if let Source::Up { .. } = source {
                    effects.push(Effect::Log(withdrawn_line(self.why_not(state, silent))));
                }
                self.source = Source::Closed;
            }
            (true, _) | (false, Source::Closed | Source::Broken) => {}
        }
        if matches!(
            fact,
            Fact::Audio(AudioFact::Down) | Fact::Link(Signal::Down | Signal::Lost { .. })
        ) {
            self.recorders = Recorders::default();
            self.recorders_fresh = false;
        } else if was_recording && !self.recording() {
            // The audio adapter only reports a change, so keep the counts for the next deadline.
            self.recorders_fresh = false;
        }
        self.silence_check = match (
            self.buds == Buds::Out && self.recording(),
            fact,
            self.silence_check,
        ) {
            (true, Fact::Tick, _) | (true, _, None) => {
                Some(now.saturating_add(SILENCE_CHECK_INTERVAL))
            }
            (true, _, check) => check,
            (false, _, _) => None,
        };
    }

    fn listed(&mut self, recorders: Recorders) {
        let fresh = recorders != Recorders::default();
        if let Source::Up { capture, .. } = &mut self.source {
            *capture = match (*capture, fresh) {
                (Capture::Recording, false) if self.recorders_fresh => Capture::ListEnded,
                (Capture::ListEnded, true) => Capture::Recording,
                (capture, _) => capture,
            };
        }
        self.recorders = recorders;
        self.recorders_fresh = fresh;
    }

    pub(super) fn recorders(&self) -> (Recorders, bool) {
        (self.recorders, self.recorders_fresh)
    }

    /// A fact of an earlier open is dropped, except the library's failure, which outlives it.
    fn heard(&mut self, fact: MicFact, effects: &mut Vec<Effect>) {
        let up = matches!(self.source, Source::Up { .. });
        match fact {
            MicFact::Broken => {
                if self.source != Source::Broken {
                    effects.push(Effect::Log(SOURCE_BROKEN_LINE.to_owned()));
                }
                self.source = Source::Broken;
            }
            MicFact::FirstAudio | MicFact::Heard { .. } | MicFact::OutputSilent => {}
            MicFact::Up(id) | MicFact::Down(id) | MicFact::Recording(id, _)
                if self.source.id() != Some(id) => {}
            // Every Up is a fresh node with no recorder.
            MicFact::Up(id) => {
                if !up {
                    effects.push(Effect::Log(SOURCE_UP_LINE.to_owned()));
                }
                self.source = Source::Up {
                    id,
                    capture: Capture::Paused,
                };
            }
            MicFact::Down(id) => {
                if up {
                    effects.push(Effect::Log(withdrawn_line("server")));
                }
                self.source = Source::Down(id);
            }
            MicFact::Recording(_, recording) => {
                if let Source::Up { capture, .. } = &mut self.source {
                    *capture = match (recording, *capture) {
                        (false, _) => Capture::Paused,
                        (true, Capture::Paused) => Capture::Recording,
                        (true, capture) => capture,
                    };
                }
            }
        }
    }

    fn follow_buds(&mut self, state: &State, now: Duration) {
        self.buds = match (state.ear.any_in_ear(), self.buds) {
            _ if !state.connected => Buds::Out,
            (true, _) => Buds::Worn,
            (false, Buds::Worn) => Buds::Left(now),
            (false, Buds::Left(left)) if now < grace_end(left) => Buds::Left(left),
            (false, Buds::Left(_) | Buds::Out) => Buds::Out,
        };
    }

    /// An unworn recorder keeps the node until a current silence check confirms its audio is gone.
    fn wanted(&self, state: &State, silent: bool) -> bool {
        let near = self.buds != Buds::Out || (self.recording() && !silent);
        !self.exiting
            && !self.blocked()
            && near
            && state.connected
            && state.microphone.preference() != MicPref::Off
            && !state.model_number.is_empty()
            && state.mic_models.contains(&state.model_number)
    }

    /// The word of a withdrawal, the first that holds.
    fn why_not(&self, state: &State, silent: bool) -> &'static str {
        if self.exiting {
            "exit"
        } else if !state.connected {
            "link"
        } else if self.blocked() {
            "blocked"
        } else if state.microphone.preference() == MicPref::Off {
            "off"
        } else if silent && self.buds == Buds::Out && self.recording() {
            "silent"
        } else {
            "not_worn"
        }
    }

    /// The grace's end or the next check for a linked recorder with no bud worn.
    pub(super) fn deadline(&self) -> Option<Duration> {
        match (self.source.id(), self.buds) {
            (Some(_), Buds::Left(left)) => Some(grace_end(left)),
            (Some(_), Buds::Out) => self.silence_check,
            (None, _) | (Some(_), Buds::Worn) => None,
        }
    }

    pub(super) fn recording(&self) -> bool {
        matches!(
            self.source,
            Source::Up {
                capture: Capture::Recording,
                ..
            }
        )
    }

    pub(super) fn blocked(&self) -> bool {
        self.card_profile == CardProfile::Headset
    }

    pub(super) fn awaiting_profile(&self, server: AudioServer) -> bool {
        server == AudioServer::Up && self.card_profile == CardProfile::Unread
    }

    pub(super) fn broken(&self) -> bool {
        self.source == Source::Broken
    }

    /// What `mic` shows while the uplink is stopped.
    pub(super) fn resting(&self) -> MicCode {
        match self.source {
            Source::Up { .. } => MicCode::Ready,
            Source::Down(_) => MicCode::NoAudioServer,
            Source::Opening(_) | Source::Closed | Source::Broken => MicCode::Idle,
        }
    }

    pub(super) fn up(&self) -> bool {
        matches!(self.source, Source::Up { .. })
    }
}

fn grace_end(left: Duration) -> Duration {
    left.saturating_add(Duration::from_millis(BUD_GRACE_MS))
}

fn open(id: OpenId, state: &State) -> MicRequest {
    let description = match state.model.display_name() {
        "" => FALLBACK_DESCRIPTION.to_owned(),
        model => format!("{model}{DESCRIPTION_SUFFIX}"),
    };
    MicRequest::Open {
        id,
        name: state.mic_node_name(),
        description,
    }
}

#[cfg(test)]
mod tests;
