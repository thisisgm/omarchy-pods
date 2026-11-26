//! What the daemon does about the link besides the link itself: link signals, state changes, suspends, audio and media facts and adverts in, the notices, wakes, audio, media and monitor requests it asks for.

mod adverts;
pub mod audio;
mod battery;
pub mod dictation;
mod ducking;
mod ear;
pub mod mic;
pub mod notices;
pub mod route;
mod sleep;

use std::time::Duration;

use pods_proto::addr::BdAddr;
use pods_proto::uplink::UplinkState;

use crate::link::{SavedKeys, Signal};
use crate::state::{AdvertIntake, State};
use adverts::Adverts;
pub use adverts::{MonitorFact, Sighting};
use audio::{AudioFact, AudioRequest};
use battery::Latches;
use dictation::Dictation;
pub use dictation::{DictationFact, DictationRequest};
use ducking::Ducking;
use ear::Ear;
pub use ear::{BOTH_OUT_SETTLE_MS, Era, MediaRequest, PAUSED_MAX, PlayerOwner};
use mic::Mic;
pub use mic::{MicFact, MicRequest, OpenId, UplinkRequest};
use notices::Notice;
use route::{Edge, ROUTE_WAIT_MS, Route};
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
    /// The link's loss count moved before a pause of that era ended, so it may have left a player playing.
    PauseCut(Era),
    Advert(Sighting),
    Monitor(MonitorFact),
    Mic(MicFact),
    Dictation(DictationFact),
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
    StoreKeys(SavedKeys),
    Media(MediaRequest),
    /// Whether the BlueZ advert monitor is wanted; `true` again after a wake asks for it anew.
    Monitor(bool),
    /// Written on the open channel; a closed channel drops it.
    Uplink(UplinkRequest),
    Mic(MicRequest),
    /// Open the reader against a new dictation span when its source never paused.
    ResyncMic,
    Dictation(DictationRequest),
    /// A line for the daemon's log.
    Log(String),
    /// Both microphone paths are done, so the daemon may stop.
    Exited,
}

/// Lives as long as the driver, so a latch outlives the link that set it.
#[derive(Debug, Default)]
pub struct Policy {
    low_battery: Latches,
    suspend: Suspend,
    ear: Ear,
    adverts: Adverts,
    ducking: Ducking,
    mic: Mic,
    dictation: Dictation,
    edge: Edge,
}

impl Policy {
    /// `keys` is the pair `keys.json` held at start.
    #[must_use]
    pub fn new(keys: Option<SavedKeys>) -> Self {
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

    /// Updates the state from an advert and returns the device a wear edge can connect.
    #[must_use]
    pub fn advert(
        &mut self,
        state: &mut State,
        sighting: &Sighting,
        now: Duration,
    ) -> Option<BdAddr> {
        self.adverts.read(state, sighting, now)
    }

    pub fn manual_disconnect(&mut self) {
        self.adverts.manual_disconnect();
    }

    pub fn connect_attempted(&mut self, now: Duration) {
        self.adverts.connect_attempted(now);
    }

    /// Folds monitor state and model confirmations into `state` before `step` sees them.
    pub fn apply(&self, state: &mut State, fact: &Fact) {
        match fact {
            Fact::Link(Signal::Up { .. }) => state.advert_intake = AdvertIntake::Paused,
            Fact::Monitor(monitor) if self.adverts.wanted() => {
                state.advert_intake = monitor.intake();
            }
            Fact::Link(Signal::Uplink(UplinkState::Running)) => {
                state.mic_models.confirm(&state.model_number);
            }
            Fact::Dictation(DictationFact::Armed) if self.dictation.accepts_armed(state) => {
                state.mic_models.confirm(&state.model_number);
            }
            // The attribute arm can finish before the control channel sends its model number.
            Fact::Changed if self.dictation.armed() && state.connected => {
                state.mic_models.confirm(&state.model_number);
            }
            Fact::Link(
                Signal::Down
                | Signal::Lost { .. }
                | Signal::Keys(_)
                | Signal::Awareness(_)
                | Signal::Uplink(UplinkState::Stopped),
            )
            | Fact::Advert(_)
            | Fact::Monitor(_)
            | Fact::Changed
            | Fact::Sleep(_)
            | Fact::Tick
            | Fact::Audio(_)
            | Fact::Paused(..)
            | Fact::PauseCut(_)
            | Fact::Mic(_)
            | Fact::Dictation(_)
            | Fact::Exit => {}
        }
    }

    /// `state` is the state after the step the fact comes from; `now` is on the link machine's clock.
    #[must_use]
    pub fn step(&mut self, state: &State, fact: Fact, now: Duration) -> Vec<Effect> {
        let mic = self.step_mic(state, &fact, now);
        let ducking = self.ducking.step_at(state, &fact, now);
        let monitor = if let Fact::Link(signal) = &fact {
            self.adverts.follow(signal)
        } else {
            None
        };
        if matches!(
            fact,
            Fact::Monitor(MonitorFact::Interrupted | MonitorFact::Unavailable)
        ) {
            self.adverts.interrupt_wear_observation();
        }
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
            Fact::Audio(fact) => Vec::from_iter(self.ear.audio(fact, now).map(Effect::Media)),
            Fact::Paused(era, players) => {
                Vec::from_iter(self.ear.paused(era, players).map(Effect::Media))
            }
            Fact::PauseCut(era) => {
                self.ear.cut(era, state, now);
                Vec::new()
            }
            Fact::Advert(_) | Fact::Monitor(_) | Fact::Mic(_) | Fact::Dictation(_) | Fact::Exit => {
                Vec::new()
            }
        };
        effects.extend(monitor);
        effects.extend(ducking);
        effects.extend(mic);
        // After the fact, so a window that closes with the link up is forgotten in the same step.
        if state.connected {
            self.suspend.relinked();
        }
        effects
    }

    fn step_mic(&mut self, state: &State, fact: &Fact, now: Duration) -> Vec<Effect> {
        let arm = self.dictation.step(state, fact, now);
        let mut effects = Vec::new();
        let was_recording = self.mic.recording();
        let (recorders, fresh) = self.mic.step_node(state, fact, now, &mut effects);
        let recording = self.mic.recording();
        let before = self.edge.route();
        let waited_until = self.edge.deadline();
        let route = self
            .edge
            .step(recording, recorders, fresh, self.dictation.armed(), now);
        if let Some(route) = route.filter(|route| Some(*route) != before) {
            if route == Route::Dictation
                && !was_recording
                && matches!(fact, Fact::Audio(AudioFact::Recorders(_)))
            {
                effects.push(Effect::ResyncMic);
            }
            let count = u16::from(recorders.dictation).saturating_add(u16::from(recorders.other));
            let line = match route {
                Route::Dictation if count == 0 && !fresh => {
                    let waited = waited_until.map_or(u128::from(ROUTE_WAIT_MS), |at| {
                        now.saturating_sub(at)
                            .as_millis()
                            .saturating_add(u128::from(ROUTE_WAIT_MS))
                    });
                    format!("mic: route unknown after {waited} ms, dictation")
                }
                Route::Dictation => format!("mic: route dictation ({count} recorders)"),
                Route::Uplink => format!("mic: route uplink ({count} recorders)"),
            };
            effects.push(Effect::Log(line));
        }
        // An uplink answer unblocks DoAP, and a spent DoAP session changes uplink demand on the same turn.
        self.mic.set_route(route, &self.dictation, state);
        self.mic.observe_uplink(state, fact, now, &mut effects);
        let bearer_lost = matches!(fact, Fact::Dictation(DictationFact::BearerLost));
        let doap_route = route
            .filter(|_| bearer_lost || self.mic.ready_for_dictation(self.dictation.code(state)));
        if self
            .dictation
            .session(state, fact, now, doap_route, &mut effects)
        {
            self.edge.fallback();
            effects.push(Effect::Log(
                "dictation: this recording takes the uplink".to_owned(),
            ));
        }
        effects.extend(arm);
        self.mic
            .set_route(self.edge.route(), &self.dictation, state);
        self.mic.follow_uplink(state, now, &mut effects);
        if self.dictation.active() {
            effects.retain(|effect| *effect != Effect::Exited);
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
            .chain(self.dictation.deadline())
            .chain(self.edge.deadline())
            .chain(self.ducking.deadline())
            .min()
    }

    /// Whether a recording span is active on the node the policy holds open.
    #[must_use]
    pub fn mic_recording(&self) -> bool {
        self.mic.recording()
    }

    /// Whether incoming uplink audio can reach the tap.
    #[must_use]
    pub fn mic_accepts_uplink(&self) -> bool {
        self.edge.route() != Some(Route::Dictation)
    }

    /// Whether the `DoAP` session is open, from setting the mode through stopping.
    #[must_use]
    pub fn dictation_session_open(&self) -> bool {
        self.dictation.active()
    }

    /// Whether logind has yet to answer this session's delay-lock request.
    pub fn set_dictation_inhibit_pending(&mut self, pending: bool) {
        self.dictation.set_inhibit_pending(pending);
    }

    /// Whether a due STOP of the uplink still waits for a quiet sink or the last bud out.
    #[must_use]
    pub fn mic_stop_waits(&self) -> bool {
        self.mic.stop_waits()
    }

    /// What the `mic` status keys show, which the driver writes into the state before each publish.
    pub fn show_mic(&self, state: &mut State) {
        self.mic.show(state);
        state.mic_dictation = self.dictation.code(state);
    }
}

/// Blind to the notifications switch, which the notifier checks at the send as the C++ `Notifier` does.
fn notify(due: impl IntoIterator<Item = Notice>) -> Vec<Effect> {
    due.into_iter().map(Effect::Notify).collect()
}

#[cfg(test)]
mod tests;
