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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaRequest {
    /// Pause every player that reads `Playing`.
    PauseAll,
    Play(Vec<PlayerOwner>),
}

/// What the ear rules remember between steps.
#[derive(Debug, Default)]
pub(super) struct Ear {
    behavior: EarDetectionBehavior,
    /// Buds in an ear by the last report, so a new primary bud alone changes nothing.
    worn: usize,
    /// The pods' sink is the default by the last sink fact.
    ours: bool,
    /// When a both-out report pauses, unless a bud comes back first.
    settle: Option<Duration>,
    /// What the media adapter paused, and the only players a resume asks.
    pub(super) paused: Vec<PlayerOwner>,
}

impl Ear {
    /// A changed preference cancels the settle; a changed count of buds worn applies the preference.
    pub(super) fn changed(&mut self, state: &State, now: Duration) -> Option<MediaRequest> {
        if state.ear_detection_behavior != self.behavior {
            self.behavior = state.ear_detection_behavior;
            self.settle = None;
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
                self.settle = Some(now.saturating_add(Duration::from_millis(BOTH_OUT_SETTLE_MS)));
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

    /// A settle still pending at its end means both buds stayed out under the same preference.
    pub(super) fn tick(&mut self, now: Duration) -> Option<MediaRequest> {
        if self.settle.is_none_or(|at| at > now) {
            return None;
        }
        self.settle = None;
        self.pause()
    }

    /// The sink is unknown until the adapter reads it again; a closed channel or a loss is not a bud leaving, and only a loss forgets what was paused.
    pub(super) fn link(&mut self, signal: &Signal, state: &State) {
        match signal {
            Signal::Up { .. } => self.ours = false,
            Signal::Down => self.settle = None,
            Signal::Keys(_) => {}
            Signal::Lost { .. } => {
                self.ours = false;
                self.settle = None;
                self.paused.clear();
                self.worn = worn(state.ear);
            }
        }
    }

    pub(super) fn audio(&mut self, fact: AudioFact) {
        self.ours = match fact {
            AudioFact::Sink(sink) => sink.is_some_and(|sink| sink.is_default),
            AudioFact::Down => false,
            AudioFact::Up => self.ours,
        };
    }

    /// Remembers what the adapter paused while the link is up; buds already back by the preference make it a return.
    pub(super) fn paused(
        &mut self,
        state: &State,
        players: Vec<PlayerOwner>,
    ) -> Option<MediaRequest> {
        if !state.connected {
            return None;
        }
        for player in players {
            if self.paused.len() < PAUSED_MAX && !self.paused.contains(&player) {
                self.paused.push(player);
            }
        }
        let back = match self.behavior {
            EarDetectionBehavior::PauseWhenOneRemoved => self.worn == BUDS,
            EarDetectionBehavior::PauseWhenBothRemoved | EarDetectionBehavior::Disabled => {
                self.worn > 0
            }
        };
        if back { self.returned() } else { None }
    }

    pub(super) fn deadline(&self) -> Option<Duration> {
        self.settle
    }

    fn pause(&self) -> Option<MediaRequest> {
        self.ours.then_some(MediaRequest::PauseAll)
    }

    /// A return ends the absence and its settle: the remembered players are played when they can be, and forgotten either way.
    fn returned(&mut self) -> Option<MediaRequest> {
        self.settle = None;
        let players = mem::take(&mut self.paused);
        let plays =
            self.ours && self.behavior != EarDetectionBehavior::Disabled && !players.is_empty();
        plays.then_some(MediaRequest::Play(players))
    }
}

fn worn(report: EarReport) -> usize {
    [report.primary_in_ear(), report.secondary_in_ear()]
        .into_iter()
        .filter(|&in_ear| in_ear)
        .count()
}
