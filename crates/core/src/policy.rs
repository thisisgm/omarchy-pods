//! What the daemon does about the link besides the link itself: link signals and state changes in, the notices due out.

mod battery;
pub mod notices;

use crate::link::Signal;
use crate::state::State;
use battery::Latches;
use notices::Notice;

/// What the driver saw in one step of the link machine.
#[derive(Debug, Clone, Copy)]
pub enum Fact {
    Link(Signal),
    /// The step changed the state.
    Changed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Notify(Notice),
}

/// Lives as long as the driver, so a latch outlives the link that set it.
#[derive(Debug, Default)]
pub struct Policy {
    low_battery: Latches,
}

impl Policy {
    /// `state` is the state after the step the fact comes from.
    #[must_use]
    pub fn step(&mut self, state: &State, fact: Fact) -> Vec<Effect> {
        let due: Vec<Notice> = match fact {
            Fact::Link(signal) => notices::disconnected(signal).into_iter().collect(),
            Fact::Changed => self.low_battery.due(state.batteries),
        };
        due.into_iter().map(Effect::Notify).collect()
    }
}

#[cfg(test)]
mod tests;
