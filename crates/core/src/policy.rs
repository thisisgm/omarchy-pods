//! What the daemon does about the link besides the link itself: link signals, state changes, suspends, audio and media facts and adverts in, the notices, wakes, audio, media and monitor requests it asks for.

mod adverts;
pub mod audio;
mod battery;
mod ear;
pub mod mic;
pub mod notices;
mod sleep;

use std::time::Duration;

use pods_proto::keys::ProximityKeys;
use pods_proto::uplink::UplinkState;

use crate::link::Signal;
use crate::state::{AdvertIntake, State};
use adverts::Adverts;
pub use adverts::{MonitorFact, Sighting};
use audio::{AudioFact, AudioRequest};
use battery::Latches;
use ear::Ear;
pub use ear::{BOTH_OUT_SETTLE_MS, Era, MediaRequest, PAUSED_MAX, PlayerOwner};
use mic::Mic;
pub use mic::{MicFact, MicRequest, OpenId, UplinkRequest};
use notices::Notice;
pub use sleep::SleepEdge;
use sleep::Suspend;

/// What the driver saw in one step of the link machine, from logind, from the audio and media adapters or from the BlueZ watcher.
#[derive(Debug, Clone)]
pub enum Fact {
    Link(Signal),
    /// The step changed the state.
    Changed,
    Sleep(SleepEdge),
    /// The clock reached `deadline()`.
    Tick,
    Audio(AudioFact),
    /// The players a pause of that era paused.
    Paused(Era, Vec<PlayerOwner>),
    Advert(Sighting),
    Monitor(MonitorFact),
    Mic(MicFact),
    /// The daemon is stopping; it waits for `Effect::Exited` within its bound.
    Exit,
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
    /// Whether the BlueZ advert monitor is wanted; `true` again after a wake asks for it anew.
    Monitor(bool),
    /// Written on the open channel; a closed channel drops it.
    Uplink(UplinkRequest),
    Mic(MicRequest),
    /// A line for the daemon's log.
    Log(String),
    /// The uplink is stopped and its sink restored, so the daemon may stop.
    Exited,
}

/// Lives as long as the driver, so a latch outlives the link that set it.
#[derive(Debug, Default)]
pub struct Policy {
    low_battery: Latches,
    suspend: Suspend,
    ear: Ear,
    adverts: Adverts,
    mic: Mic,
}

impl Policy {
    /// `keys` is the pair `keys.json` held at start.
    #[must_use]
    pub fn new(keys: Option<ProximityKeys>) -> Self {
        Self {
            adverts: Adverts::new(keys),
            ..Self::default()
        }
    }

    /// What is due before any fact: the advert monitor, when keys are held.
    #[must_use]
    pub fn start(&self) -> Vec<Effect> {
        Vec::from_iter(self.adverts.wanted().then_some(Effect::Monitor(true)))
    }

    /// Folds a fact into `state` before `step` sees it: an advert from our pods, what the advert monitor reads, and a model whose uplink runs.
    pub fn apply(&self, state: &mut State, fact: &Fact) {
        match fact {
            Fact::Link(Signal::Up { .. }) => state.advert_intake = AdvertIntake::Paused,
            Fact::Advert(sighting) => {
                if let Some(keys) = self.adverts.keys() {
                    adverts::read(state, keys, sighting);
                }
            }
            Fact::Monitor(monitor) if self.adverts.wanted() => {
                state.advert_intake = monitor.intake();
            }
            Fact::Link(Signal::Uplink(UplinkState::Running)) => {
                state.mic_models.confirm(&state.model_number);
            }
            Fact::Link(
                Signal::Down
                | Signal::Lost { .. }
                | Signal::Keys(_)
                | Signal::Uplink(UplinkState::Stopped),
            )
            | Fact::Monitor(_)
            | Fact::Changed
            | Fact::Sleep(_)
            | Fact::Tick
            | Fact::Audio(_)
            | Fact::Paused(..)
            | Fact::Mic(_)
            | Fact::Exit => {}
        }
    }

    /// `state` is the state after the step the fact comes from; `now` is on the link machine's clock.
    #[must_use]
    pub fn step(&mut self, state: &State, fact: Fact, now: Duration) -> Vec<Effect> {
        let mic = self.mic.step(state, &fact, now);
        let monitor = if let Fact::Link(signal) = &fact {
            self.adverts.follow(signal)
        } else {
            None
        };
        let mut effects = match fact {
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
                let mut effects = self.woke(now);
                effects.extend(self.ear.tick(now).map(Effect::Media));
                effects
            }
            Fact::Audio(fact) => {
                self.ear.audio(fact);
                Vec::new()
            }
            Fact::Paused(era, players) => {
                Vec::from_iter(self.ear.paused(era, players).map(Effect::Media))
            }
            Fact::Advert(_) | Fact::Monitor(_) | Fact::Mic(_) | Fact::Exit => Vec::new(),
        };
        effects.extend(monitor);
        effects.extend(mic);
        // After the fact, so a window that closes with the link up is forgotten in the same step.
        if state.connected {
            self.suspend.relinked();
        }
        effects
    }

    /// A closed suspend window wakes the link, and asks for the monitor anew, as BlueZ may have dropped it in the suspend.
    fn woke(&mut self, now: Duration) -> Vec<Effect> {
        if !self.suspend.close(now) {
            return Vec::new();
        }
        let again = self.adverts.wanted().then_some(Effect::Monitor(true));
        [Effect::Wake].into_iter().chain(again).collect()
    }

    /// When the driver must hand in `Fact::Tick` next, if the policy waits on the clock.
    #[must_use]
    pub fn deadline(&self) -> Option<Duration> {
        self.suspend
            .deadline()
            .into_iter()
            .chain(self.ear.deadline())
            .chain(self.mic.deadline())
            .min()
    }

    /// Whether a recorder is linked to the node the policy holds open; a fact of an earlier open never sets it.
    #[must_use]
    pub fn mic_recording(&self) -> bool {
        self.mic.recording()
    }

    /// What the `mic` status keys show, which the driver writes into the state before each publish.
    pub fn show_mic(&self, state: &mut State) {
        self.mic.show(state);
    }
}

/// Blind to the notifications switch, which the notifier checks at the send as the C++ `Notifier` does.
fn notify(due: impl IntoIterator<Item = Notice>) -> Vec<Effect> {
    due.into_iter().map(Effect::Notify).collect()
}

#[cfg(test)]
mod tests;
