//! Ear detection: a bud taken out pauses what plays on the pods, and the buds back resume exactly what was paused.

use std::mem;
use std::time::Duration;

use pods_proto::ear::EarReport;

use super::audio::AudioFact;
use crate::link::Signal;
use crate::state::{EarDetectionBehavior, State};

/// The C++ `bothPodsOutSettleMs`: a bud passed from hand to case reports both out for a moment.
pub const BOTH_OUT_SETTLE_MS: u64 = 1200;

/// The most players remembered at once, and the most the media adapter asks.
pub const PAUSED_MAX: usize = 32;

/// The D-Bus limit on the length of a bus name.
pub(super) const BUS_NAME_MAX: usize = 255;

/// Buds in a pair, so both are worn when this many are.
const BUDS: usize = 2;

/// The first character of every unique bus name, which the bus never hands out twice.
const UNIQUE_PREFIX: char = ':';

/// The unique bus name of the connection that took a pause, as `:1.42`, so a player reopened under the same well known name is a different one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerOwner(String);

impl PlayerOwner {
    #[must_use]
    pub fn new(name: &str) -> Option<Self> {
        let named = name
            .strip_prefix(UNIQUE_PREFIX)
            .is_some_and(|rest| !rest.is_empty());
        (named && name.len() <= BUS_NAME_MAX).then(|| Self(name.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The link's losses counted when a pause was decided, so its result is kept only if no loss came after.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Era(u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaRequest {
    /// Pause every player that reads `Playing`, and hand the result back with the era.
    PauseAll(Era),
    Play(Vec<PlayerOwner>),
}

/// Whether the pods' sink is the default, by the adapter's last read of it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Sink {
    /// No device is linked, from the start and from a loss until a link up, so no read decides anything.
    #[default]
    Unlinked,
    /// From a link up or a lost audio server until the adapter reads a sink.
    Unread,
    /// Unread, and the buds came back meanwhile, so the read decides the resume.
    Awaited,
    Elsewhere,
    Ours,
}

/// The wait before a pause: from a both-out report, or over again after the channel closed on it or on its pause, or after a pause came due with the sink unread.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Settle {
    #[default]
    Idle,
    /// Pauses at this time, unless a bud comes back first.
    Until(Duration),
    /// The channel closed on it or on its pause, so it waits for the link to be up again.
    Cut,
    /// The link is up again, or a pause came due with the sink unread, so it starts over at the next read of a sink.
    Reopened,
}

/// What the ear rules remember between steps.
#[derive(Debug, Default)]
pub(super) struct Ear {
    behavior: EarDetectionBehavior,
    /// Buds in an ear by the last report, so a new primary bud alone changes nothing.
    worn: usize,
    sink: Sink,
    settle: Settle,
    /// What the media adapter paused, and the only players a resume asks.
    pub(super) paused: Vec<PlayerOwner>,
    era: Era,
}

impl Ear {
    /// A changed preference cancels the settle; a changed count of buds worn applies the preference.
    pub(super) fn changed(&mut self, state: &State, now: Duration) -> Option<MediaRequest> {
        if state.ear_detection_behavior != self.behavior {
            self.behavior = state.ear_detection_behavior;
            self.settle = Settle::Idle;
        }
        let was = mem::replace(&mut self.worn, worn(state.ear));
        if self.worn == was {
            return None;
        }
        // With both buds worn nothing is absent, under every preference, so no list outlives it.
        if self.worn == BUDS {
            return self.returned();
        }
        match self.behavior {
            EarDetectionBehavior::PauseWhenOneRemoved if self.worn < was => self.pause(),
            EarDetectionBehavior::PauseWhenBothRemoved if self.worn == 0 => {
                // With no device linked nothing plays on the pods, so no wait starts that a link up could find.
                if self.sink != Sink::Unlinked {
                    self.settle = settle_from(now);
                }
                None
            }
            // Under these two a bud back from none also ends the absence, and a bud leaving never does.
            EarDetectionBehavior::PauseWhenBothRemoved | EarDetectionBehavior::Disabled
                if was == 0 =>
            {
                self.returned()
            }
            EarDetectionBehavior::PauseWhenOneRemoved
            | EarDetectionBehavior::PauseWhenBothRemoved
            | EarDetectionBehavior::Disabled => None,
        }
    }

    /// A settle still pending at its end means the buds stayed out under the same preference.
    pub(super) fn tick(&mut self, now: Duration) -> Option<MediaRequest> {
        if self.deadline().is_none_or(|at| at > now) {
            return None;
        }
        self.settle = Settle::Idle;
        self.pause()
    }

    /// The sink is unread until the adapter reads it again; a closed channel is not a bud leaving, so it cuts a settle until the link is up again and its sink read, and only a loss forgets what was paused.
    pub(super) fn link(&mut self, signal: &Signal, state: &State) {
        match signal {
            Signal::Up { .. } => {
                if self.sink != Sink::Awaited {
                    self.sink = Sink::Unread;
                }
                if self.settle == Settle::Cut {
                    self.settle = Settle::Reopened;
                }
            }
            Signal::Down => {
                if self.settle != Settle::Idle {
                    self.settle = Settle::Cut;
                }
            }
            Signal::Keys(_) | Signal::Uplink(_) => {}
            Signal::Lost { .. } => {
                self.sink = Sink::Unlinked;
                self.settle = Settle::Idle;
                self.paused.clear();
                self.era = Era(self.era.0.wrapping_add(1));
                self.worn = worn(state.ear);
            }
        }
    }

    /// A sink read starts a reopened settle over, and settles a return that waited for it if the buds are still back by the preference.
    pub(super) fn audio(&mut self, fact: AudioFact, now: Duration) -> Option<MediaRequest> {
        // A sink read for the lost device can still be queued when the driver stops following it.
        if self.sink == Sink::Unlinked {
            return None;
        }
        let read = match fact {
            AudioFact::Sink(Some(sink)) if sink.is_default => Sink::Ours,
            AudioFact::Sink(Some(_)) => Sink::Elsewhere,
            AudioFact::Sink(None) | AudioFact::Down => {
                self.unread();
                return None;
            }
            AudioFact::Up
            | AudioFact::Restored(_)
            | AudioFact::Recorders(_)
            | AudioFact::Hint(_)
            | AudioFact::Headset(_) => return None,
        };
        if self.settle == Settle::Reopened {
            self.settle = settle_from(now);
        }
        let awaited = mem::replace(&mut self.sink, read) == Sink::Awaited;
        if awaited && self.back() {
            self.returned()
        } else {
            None
        }
    }

    /// Remembers what a pause of this era paused; buds already back by the preference make it a return.
    pub(super) fn paused(&mut self, era: Era, players: Vec<PlayerOwner>) -> Option<MediaRequest> {
        if era != self.era {
            return None;
        }
        for player in players {
            if self.paused.len() < PAUSED_MAX && !self.paused.contains(&player) {
                self.paused.push(player);
            }
        }
        if self.back() { self.returned() } else { None }
    }

    /// A pause the closed channel cut waits as a cut settle does while the buds stay out, so the pods report their ears again before it is asked.
    pub(super) fn cut(&mut self, era: Era, state: &State, now: Duration) {
        if era != self.era || !self.absent() {
            return;
        }
        if !state.connected {
            self.settle = Settle::Cut;
            return;
        }
        self.settle = match self.sink {
            Sink::Unread | Sink::Awaited => Settle::Reopened,
            Sink::Unlinked | Sink::Elsewhere | Sink::Ours => settle_from(now),
        };
    }

    fn absent(&self) -> bool {
        self.behavior != EarDetectionBehavior::Disabled && !self.back()
    }

    fn back(&self) -> bool {
        match self.behavior {
            EarDetectionBehavior::PauseWhenOneRemoved => self.worn == BUDS,
            EarDetectionBehavior::PauseWhenBothRemoved | EarDetectionBehavior::Disabled => {
                self.worn > 0
            }
        }
    }

    /// A return that waits for the sink keeps waiting, and a late fact with no device linked starts no wait.
    fn unread(&mut self) {
        if matches!(self.sink, Sink::Elsewhere | Sink::Ours) {
            self.sink = Sink::Unread;
        }
    }

    pub(super) fn deadline(&self) -> Option<Duration> {
        match self.settle {
            Settle::Until(at) => Some(at),
            Settle::Idle | Settle::Cut | Settle::Reopened => None,
        }
    }

    /// A pause asked leaves no settle to ask it again; one due at an unread sink waits for the read as a reopened settle does.
    fn pause(&mut self) -> Option<MediaRequest> {
        match self.sink {
            Sink::Unread | Sink::Awaited => {
                self.settle = Settle::Reopened;
                None
            }
            Sink::Unlinked | Sink::Elsewhere => None,
            Sink::Ours => {
                self.settle = Settle::Idle;
                Some(MediaRequest::PauseAll(self.era))
            }
        }
    }

    /// A return ends the absence and its settle: once the sink is read, the remembered players are played when they can be, and forgotten either way.
    fn returned(&mut self) -> Option<MediaRequest> {
        self.settle = Settle::Idle;
        let off = self.behavior == EarDetectionBehavior::Disabled;
        if !off && matches!(self.sink, Sink::Unread | Sink::Awaited) {
            self.sink = Sink::Awaited;
            return None;
        }
        let players = mem::take(&mut self.paused);
        let plays = self.sink == Sink::Ours && !off && !players.is_empty();
        plays.then_some(MediaRequest::Play(players))
    }
}

fn settle_from(now: Duration) -> Settle {
    Settle::Until(now.saturating_add(Duration::from_millis(BOTH_OUT_SETTLE_MS)))
}

fn worn(report: EarReport) -> usize {
    [report.primary_in_ear(), report.secondary_in_ear()]
        .into_iter()
        .filter(|&in_ear| in_ear)
        .count()
}
