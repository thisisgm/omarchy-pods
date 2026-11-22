//! What the daemon does about the link besides the link itself: link signals, state changes, suspends, audio and media facts in, the notices, wakes, audio and media requests due out.

pub mod audio;
mod battery;
mod ear;
pub mod notices;
mod sleep;

use std::time::Duration;

use pods_proto::keys::ProximityKeys;

use crate::link::Signal;
use crate::state::State;
use audio::{AudioFact, AudioRequest};
use battery::Latches;
use ear::Ear;
pub use ear::{BOTH_OUT_SETTLE_MS, MediaRequest, PAUSED_MAX, PlayerOwner};
use notices::Notice;
pub use sleep::SleepEdge;
use sleep::Suspend;

/// What the driver saw in one step of the link machine, from logind or from the audio and media adapters.
#[derive(Debug, Clone)]
pub enum Fact {
    Link(Signal),
    /// The step changed the state.
    Changed,
    Sleep(SleepEdge),
    /// The clock reached `deadline()`.
    Tick,
    Audio(AudioFact),
    /// The players the media adapter paused.
    Paused(Vec<PlayerOwner>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Notify(Notice),
    /// The suspend window closed; the driver answers by stepping the link machine with `Event::Resumed`.
    Wake,
    Audio(AudioRequest),
    /// A pair the pods answered with; the driver hands it to the writer of `keys.json`.
    StoreKeys(ProximityKeys),
    Media(MediaRequest),
}

/// Lives as long as the driver, so a latch outlives the link that set it.
#[derive(Debug, Default)]
pub struct Policy {
    low_battery: Latches,
    suspend: Suspend,
    ear: Ear,
}

impl Policy {
    /// `state` is the state after the step the fact comes from; `now` is on the link machine's clock.
    #[must_use]
    pub fn step(&mut self, state: &State, fact: Fact, now: Duration) -> Vec<Effect> {
        let effects = match fact {
            Fact::Link(Signal::Keys(keys)) => vec![Effect::StoreKeys(keys)],
            Fact::Link(signal) => {
                let mut effects = notify(notices::disconnected(&signal, self.suspend));
                effects.extend(audio::follow(&signal).map(Effect::Audio));
                self.ear.link(&signal, state);
                effects
            }
            Fact::Changed => {
                let mut effects = notify(self.low_battery.due(state.batteries));
                effects.extend(self.ear.changed(state, now).map(Effect::Media));
                effects
            }
            Fact::Sleep(edge) => {
                self.suspend.edge(edge, now);
                Vec::new()
            }
            Fact::Tick => {
                let mut effects = Vec::from_iter(self.suspend.close(now).then_some(Effect::Wake));
                effects.extend(self.ear.tick(now).map(Effect::Media));
                effects
            }
            Fact::Audio(fact) => {
                self.ear.audio(fact);
                Vec::new()
            }
            Fact::Paused(players) => {
                Vec::from_iter(self.ear.paused(state, players).map(Effect::Media))
            }
        };
        // After the fact, so a window that closes with the link up is forgotten in the same step.
        if state.connected {
            self.suspend.relinked();
        }
        effects
    }

    /// When the driver must hand in `Fact::Tick` next, if the policy waits on the clock.
    #[must_use]
    pub fn deadline(&self) -> Option<Duration> {
        self.suspend
            .deadline()
            .into_iter()
            .chain(self.ear.deadline())
            .min()
    }
}

/// Blind to the notifications switch, which the notifier checks at the send as the C++ `Notifier` does.
fn notify(due: impl IntoIterator<Item = Notice>) -> Vec<Effect> {
    due.into_iter().map(Effect::Notify).collect()
}

#[cfg(test)]
mod tests;
