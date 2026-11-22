//! What the daemon does about the link besides the link itself: link signals, state changes, suspends and audio facts in, the notices, wakes and audio requests due out.

pub mod audio;
mod battery;
pub mod notices;
mod sleep;

use std::time::Duration;

use crate::link::Signal;
use crate::state::State;
use audio::{AudioFact, AudioRequest};
use battery::Latches;
use notices::Notice;
pub use sleep::SleepEdge;
use sleep::Suspend;

/// What the driver saw in one step of the link machine, from logind or from the audio adapter.
#[derive(Debug, Clone, Copy)]
pub enum Fact {
    Link(Signal),
    /// The step changed the state.
    Changed,
    Sleep(SleepEdge),
    /// The clock reached `deadline()`.
    Tick,
    Audio(AudioFact),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Notify(Notice),
    /// The suspend window closed; the driver answers by stepping the link machine with `Event::Resumed`.
    Wake,
    Audio(AudioRequest),
}

/// Lives as long as the driver, so a latch outlives the link that set it.
#[derive(Debug, Default)]
pub struct Policy {
    low_battery: Latches,
    suspend: Suspend,
}

impl Policy {
    /// `state` is the state after the step the fact comes from; `now` is on the link machine's clock.
    #[must_use]
    pub fn step(&mut self, state: &State, fact: Fact, now: Duration) -> Vec<Effect> {
        let effects = match fact {
            Fact::Link(signal) => {
                let mut effects = notify(notices::disconnected(signal, self.suspend));
                effects.push(Effect::Audio(audio::follow(signal)));
                effects
            }
            Fact::Changed => notify(self.low_battery.due(state.batteries)),
            Fact::Sleep(edge) => {
                self.suspend.edge(edge, now);
                Vec::new()
            }
            Fact::Tick => Vec::from_iter(self.suspend.close(now).then_some(Effect::Wake)),
            Fact::Audio(_) => Vec::new(),
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
        self.suspend.deadline()
    }
}

/// Blind to the notifications switch, which the notifier checks at the send as the C++ `Notifier` does.
fn notify(due: impl IntoIterator<Item = Notice>) -> Vec<Effect> {
    due.into_iter().map(Effect::Notify).collect()
}

#[cfg(test)]
mod tests;
