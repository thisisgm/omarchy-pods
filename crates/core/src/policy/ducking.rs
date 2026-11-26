//! Conversation Awareness volume, including a restore owed through a missing sink.

use std::time::Duration;

use pods_proto::addr::BdAddr;
use pods_proto::awareness::Awareness;

use super::audio::{AudioFact, AudioRequest, Percent, SinkFact, VolumeStamp};
use super::{Effect, Fact};
use crate::link::Signal;
use crate::state::{AudioServer, State};

pub(super) const LOWERED_PERCENT: u8 = 20;
pub(super) const VOLUME_GRID: u8 = 5;
// Superwhisper Muffle takes 1500 ms to release after recording stops.
const MUFFLE_RELEASE: Duration = Duration::from_millis(1500);

#[expect(
    clippy::arithmetic_side_effects,
    reason = "a percent times 20 plus the rounding offset fits a u16, and the divisors are nonzero"
)]
pub(super) fn lowered(saved: Percent) -> Percent {
    let full = u16::from(Percent::FULL.get());
    let grid = u16::from(VOLUME_GRID);
    let fifth = u16::from(saved.get()) * u16::from(LOWERED_PERCENT) / full;
    let snapped = (fifth + grid / 2) / grid * grid;
    u8::try_from(snapped)
        .ok()
        .and_then(Percent::new)
        .unwrap_or(Percent::FULL)
}

#[derive(Debug, Default)]
pub(super) struct Ducking {
    device: Option<BdAddr>,
    sink: Option<SinkFact>,
    volume_writes: u64,
    pending_volume: Option<PendingVolume>,
    recording: bool,
    release_until: Option<Duration>,
    deferred_start: bool,
    conversation: Conversation,
    parked_restores: Vec<(BdAddr, Percent, Percent)>,
}

#[derive(Debug)]
struct PendingVolume {
    target: Percent,
    // The sink's volume before the first write and any superseded targets can still be reported.
    stale: Vec<Percent>,
    superseded: bool,
}

#[derive(Clone, Copy, Debug, Default)]
enum Conversation {
    #[default]
    Idle,
    Lowered {
        saved: Percent,
        ducked: Percent,
    },
    Restoring {
        saved: Percent,
        ducked: Percent,
    },
    /// The wearer set their own volume, so another start report in this conversation is ignored.
    UserAdjusted,
}

impl Ducking {
    pub(super) fn deadline(&self) -> Option<Duration> {
        self.release_until.filter(|_| self.deferred_start)
    }

    pub(super) fn step_at(&mut self, state: &State, fact: &Fact, now: Duration) -> Vec<Effect> {
        let volume = match fact {
            Fact::Link(Signal::Up { device }) => {
                self.follow(*device);
                None
            }
            Fact::Link(Signal::Down | Signal::Lost { .. }) => {
                self.deferred_start = false;
                self.await_sink();
                if matches!(self.conversation, Conversation::UserAdjusted) {
                    self.conversation = Conversation::Idle;
                }
                if matches!(fact, Fact::Link(Signal::Lost { .. })) {
                    self.recording = false;
                }
                None
            }
            Fact::Audio(AudioFact::Down) => {
                self.await_sink();
                self.recording = false;
                None
            }
            Fact::Audio(AudioFact::Sink(None)) => {
                self.await_sink();
                None
            }
            Fact::Audio(AudioFact::Sink(Some(sink)))
                if state.connected && state.audio_server == AudioServer::Up =>
            {
                self.read_sink(*sink)
            }
            Fact::Audio(AudioFact::Recording(recording))
                if state.audio_server != AudioServer::Idle =>
            {
                self.on_recording(*recording, now)
            }
            Fact::Audio(AudioFact::VolumeAdjusted)
                if state.connected && state.audio_server == AudioServer::Up =>
            {
                self.pending_volume = None;
                self.conversation = match self.conversation {
                    Conversation::Lowered { .. } => Conversation::UserAdjusted,
                    Conversation::Restoring { .. } => Conversation::Idle,
                    Conversation::Idle | Conversation::UserAdjusted => self.conversation,
                };
                None
            }
            Fact::Audio(AudioFact::VolumeSkipped(stamp)) => {
                self.skip(*stamp);
                None
            }
            Fact::Link(Signal::Awareness(Awareness::Started))
                if state.connected && state.conversational_awareness && !self.recording =>
            {
                self.deferred_start = self.release_until.is_some_and(|until| now < until);
                if self.deferred_start {
                    None
                } else {
                    self.start()
                }
            }
            Fact::Tick if self.deadline().is_some_and(|until| now >= until) => {
                self.deferred_start = false;
                if state.connected && state.conversational_awareness {
                    self.start()
                } else {
                    None
                }
            }
            Fact::Link(Signal::Awareness(Awareness::Ended)) | Fact::Exit => self.restore(),
            Fact::Changed if !state.conversational_awareness => self.restore(),
            Fact::Link(Signal::Awareness(_) | Signal::Keys(_) | Signal::Uplink(_))
            | Fact::Audio(
                AudioFact::Up
                | AudioFact::Sink(_)
                | AudioFact::Restored(_)
                | AudioFact::Recorders(_)
                | AudioFact::Recording(_)
                | AudioFact::VolumeAdjusted
                | AudioFact::Hint(_)
                | AudioFact::Headset(_),
            )
            | Fact::Changed
            | Fact::Sleep(_)
            | Fact::Tick
            | Fact::Paused(..)
            | Fact::PauseCut(_)
            | Fact::Advert(_)
            | Fact::Monitor(_)
            | Fact::Mic(_)
            | Fact::Dictation(_) => None,
        };
        let (Some(volume), Some(sink)) = (volume, self.sink) else {
            return Vec::new();
        };
        vec![self.request_volume(volume, sink)]
    }

    fn on_recording(&mut self, active: bool, now: Duration) -> Option<Percent> {
        self.recording = active;
        self.release_until = (!active).then(|| now.saturating_add(MUFFLE_RELEASE));
        if active {
            self.deferred_start = false;
            if matches!(self.conversation, Conversation::Lowered { .. }) {
                return self.restore();
            }
        }
        None
    }

    fn skip(&mut self, stamp: VolumeStamp) {
        if self.volume_writes == stamp.writes
            && self
                .sink
                .is_some_and(|sink| sink.volume_stamp.epoch == stamp.epoch)
            && matches!(
                self.conversation,
                Conversation::Lowered { .. } | Conversation::Restoring { .. }
            )
        {
            self.pending_volume = None;
            self.conversation = Conversation::Idle;
        }
    }

    fn request_volume(&mut self, volume: Percent, sink: SinkFact) -> Effect {
        if let Some(pending) = &mut self.pending_volume {
            if !pending.stale.contains(&pending.target) {
                pending.stale.push(pending.target);
            }
            pending.target = volume;
            pending.superseded = true;
        } else if sink.volume != volume
            || matches!(self.conversation, Conversation::Restoring { .. })
        {
            // An unchanged duck can have no echo. A restore asks for a confirmation reading.
            self.pending_volume = Some(PendingVolume {
                target: volume,
                stale: vec![sink.volume],
                superseded: false,
            });
        }
        self.volume_writes = self.volume_writes.saturating_add(1);
        let stamp = VolumeStamp {
            writes: self.volume_writes,
            ..sink.volume_stamp
        };
        let request = if matches!(self.conversation, Conversation::Restoring { .. }) {
            AudioRequest::RestoreVolume(volume, stamp)
        } else {
            AudioRequest::SetVolume(volume, stamp)
        };
        Effect::Audio(request)
    }

    fn follow(&mut self, device: BdAddr) {
        self.await_sink();
        if self.device == Some(device) {
            return;
        }
        self.deferred_start = false;
        if let (Some(previous), Conversation::Restoring { saved, ducked }) =
            (self.device, &self.conversation)
        {
            self.parked_restores.push((previous, *saved, *ducked));
        }
        let parked = self
            .parked_restores
            .iter()
            .position(|(owner, ..)| *owner == device);
        self.conversation = if let Some(index) = parked {
            let (_, saved, ducked) = self.parked_restores.swap_remove(index);
            Conversation::Restoring { saved, ducked }
        } else {
            Conversation::Idle
        };
        self.device = Some(device);
        self.recording = false;
    }

    fn start(&mut self) -> Option<Percent> {
        let sink = self.sink.filter(|sink| sink.is_default)?;
        let saved = match self.conversation {
            Conversation::Idle => sink.volume,
            Conversation::Restoring { saved, .. } => saved,
            Conversation::Lowered { .. } | Conversation::UserAdjusted => return None,
        };
        let ducked = lowered(saved);
        self.conversation = Conversation::Lowered { saved, ducked };
        Some(ducked)
    }

    fn restore(&mut self) -> Option<Percent> {
        self.deferred_start = false;
        match self.conversation {
            Conversation::Lowered { saved, ducked } => {
                self.conversation = Conversation::Restoring { saved, ducked };
                self.sink.map(|_| saved)
            }
            Conversation::UserAdjusted => {
                self.conversation = Conversation::Idle;
                None
            }
            Conversation::Idle | Conversation::Restoring { .. } => None,
        }
    }

    fn await_sink(&mut self) {
        self.sink = None;
        self.pending_volume = None;
        self.volume_writes = 0;
        if let Conversation::Lowered { saved, ducked } = self.conversation {
            self.conversation = Conversation::Restoring { saved, ducked };
        }
    }

    fn read_sink(&mut self, sink: SinkFact) -> Option<Percent> {
        let last = self.sink.replace(sink);
        let replaced = last.is_some_and(|last| last.index != sink.index);
        let epoch_changed =
            last.is_some_and(|last| last.volume_stamp.epoch != sink.volume_stamp.epoch);
        if replaced
            || (epoch_changed
                && self.pending_volume.is_some()
                && matches!(self.conversation, Conversation::Lowered { .. }))
        {
            self.pending_volume = None;
            if let Conversation::Lowered { saved, ducked } = self.conversation {
                self.conversation = Conversation::Restoring { saved, ducked };
            }
        }
        if replaced || epoch_changed {
            self.volume_writes = 0;
        }
        if sink.volume_stamp.writes < self.volume_writes {
            // A new duck can still see a prior duck while its superseded restore is in flight.
            let stale = self
                .pending_volume
                .as_ref()
                .is_some_and(|pending| pending.superseded && pending.stale.contains(&sink.volume));
            if !stale && let Conversation::Lowered { saved, .. } = &mut self.conversation {
                *saved = sink.volume;
            }
            return None;
        }
        if let Some(pending) = &self.pending_volume {
            if sink.volume == pending.target {
                self.pending_volume = None;
                if matches!(self.conversation, Conversation::Restoring { .. }) {
                    self.conversation = Conversation::Idle;
                }
                return None;
            }
            if pending.stale.contains(&sink.volume) {
                return None;
            }
            self.pending_volume = None;
        }
        match self.conversation {
            Conversation::Restoring { saved, ducked } if sink.volume == ducked => Some(saved),
            Conversation::Restoring { .. } => {
                self.conversation = Conversation::Idle;
                None
            }
            Conversation::Lowered { saved, ducked }
                if sink.volume != saved && sink.volume != ducked =>
            {
                self.conversation = Conversation::UserAdjusted;
                None
            }
            Conversation::Idle | Conversation::Lowered { .. } | Conversation::UserAdjusted => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::tests::device;
    use super::{
        AudioFact, AudioRequest, AudioServer, Awareness, Ducking, Effect, Fact, Percent, Signal,
        SinkFact, State, VolumeStamp, lowered,
    };
    use crate::policy::audio::SinkState;

    impl Ducking {
        fn step(&mut self, state: &State, fact: &Fact) -> Vec<Effect> {
            self.step_at(state, fact, Duration::ZERO)
        }
    }

    const STARTED: Fact = Fact::Link(Signal::Awareness(Awareness::Started));
    const ENDED: Fact = Fact::Link(Signal::Awareness(Awareness::Ended));

    fn connected() -> State {
        State {
            connected: true,
            audio_server: AudioServer::Up,
            conversational_awareness: true,
            ..State::default()
        }
    }

    fn sink(index: u32, percent: u8, epoch: u64, writes: u64) -> Fact {
        Fact::Audio(AudioFact::Sink(Some(SinkFact {
            index,
            is_default: true,
            volume: Percent::new(percent).unwrap(),
            state: SinkState::Running,
            volume_stamp: VolumeStamp { epoch, writes },
        })))
    }

    fn set(percent: u8, epoch: u64, writes: u64) -> Effect {
        Effect::Audio(AudioRequest::SetVolume(
            Percent::new(percent).unwrap(),
            VolumeStamp { epoch, writes },
        ))
    }

    fn restore(percent: u8, epoch: u64, writes: u64) -> Effect {
        Effect::Audio(AudioRequest::RestoreVolume(
            Percent::new(percent).unwrap(),
            VolumeStamp { epoch, writes },
        ))
    }

    #[test]
    fn a_sink_reading_queued_before_start_sets_the_restore_volume() {
        let state = connected();
        let mut ducking = Ducking::default();

        assert_eq!(ducking.step(&state, &sink(0, 50, 0, 0)), []);
        let queued = sink(0, 45, 0, 0);
        assert_eq!(ducking.step(&state, &STARTED), [set(10, 0, 1)]);
        assert_eq!(ducking.step(&state, &queued), []);
        assert_eq!(ducking.step(&state, &sink(0, 10, 0, 1)), []);
        assert_eq!(ducking.step(&state, &ENDED), [restore(45, 0, 2)]);
    }

    #[test]
    fn a_duck_discarded_after_an_epoch_change_leaves_the_baseline_available() {
        let state = connected();
        let mut ducking = Ducking::default();

        assert_eq!(ducking.step(&state, &sink(1, 55, 0, 0)), []);
        assert_eq!(ducking.step(&state, &STARTED), [set(10, 0, 1)]);
        assert_eq!(ducking.step(&state, &sink(1, 55, 1, 0)), []);
        assert_eq!(ducking.step(&state, &ENDED), []);
        assert_eq!(ducking.step(&state, &STARTED), [set(10, 1, 1)]);
        assert_eq!(ducking.step(&state, &sink(1, 10, 1, 1)), []);
        assert_eq!(ducking.step(&state, &ENDED), [restore(55, 1, 2)]);
    }

    #[test]
    fn an_owed_restore_from_the_previous_epoch_leaves_the_new_volume_available() {
        let mut state = connected();
        let mut ducking = Ducking::default();
        let up = Fact::Link(Signal::Up { device: device() });

        assert_eq!(ducking.step(&state, &up), []);
        assert_eq!(ducking.step(&state, &sink(1, 50, 0, 0)), []);
        assert_eq!(ducking.step(&state, &STARTED), [set(10, 0, 1)]);
        assert_eq!(ducking.step(&state, &sink(1, 10, 0, 1)), []);
        state.connected = false;
        assert_eq!(ducking.step(&state, &Fact::Link(Signal::Down)), []);
        assert_eq!(
            ducking.step(&state, &Fact::Audio(AudioFact::Sink(None))),
            []
        );
        state.connected = true;
        assert_eq!(ducking.step(&state, &up), []);
        assert_eq!(
            ducking.step(&state, &sink(2, 10, 1, 0)),
            [restore(50, 1, 1)]
        );

        // The queued restore reaches the new epoch without changing its write count.
        assert_eq!(ducking.step(&state, &sink(2, 10, 2, 0)), []);
        assert_eq!(ducking.step(&state, &sink(2, 50, 2, 0)), []);
        assert_eq!(ducking.step(&state, &sink(2, 30, 2, 0)), []);
        let next_duck = ducking.step(&state, &STARTED);
        let [Effect::Audio(AudioRequest::SetVolume(volume, stamp))] = next_duck.as_slice() else {
            panic!("the next conversation should lower the volume");
        };
        let echo = sink(2, volume.get(), stamp.epoch, stamp.writes);
        assert_eq!(ducking.step(&state, &echo), []);
        assert_eq!(ducking.step(&state, &ENDED), [restore(30, 2, 2)]);
        assert_eq!(next_duck, [set(5, 2, 1)]);
    }

    #[test]
    fn a_late_duck_echo_during_the_next_conversation_keeps_the_saved_volume() {
        let state = connected();
        let mut ducking = Ducking::default();

        assert_eq!(ducking.step(&state, &sink(1, 50, 0, 0)), []);
        assert_eq!(ducking.step(&state, &STARTED), [set(10, 0, 1)]);
        assert_eq!(ducking.step(&state, &ENDED), [restore(50, 0, 2)]);
        assert_eq!(ducking.step(&state, &STARTED), [set(10, 0, 3)]);
        assert_eq!(ducking.step(&state, &sink(1, 10, 0, 1)), []);
        assert_eq!(ducking.step(&state, &ENDED), [restore(50, 0, 4)]);
    }

    #[test]
    fn a_user_adjustment_while_lowered_ends_ducking_until_speech_ends() {
        let state = connected();
        let mut ducking = Ducking::default();

        assert_eq!(ducking.step(&state, &sink(1, 50, 0, 0)), []);
        assert_eq!(ducking.step(&state, &STARTED), [set(10, 0, 1)]);
        let adjusted = Fact::Audio(AudioFact::VolumeAdjusted);
        assert_eq!(ducking.step(&state, &adjusted), []);
        assert!(ducking.pending_volume.is_none());
        assert_eq!(ducking.step(&state, &sink(1, 30, 0, 1)), []);
        assert_eq!(ducking.step(&state, &STARTED), []);
        assert_eq!(ducking.step(&state, &ENDED), []);
        assert_eq!(ducking.step(&state, &STARTED), [set(5, 0, 2)]);
    }

    #[test]
    fn a_user_adjustment_during_restore_allows_a_new_conversation() {
        let state = connected();
        let mut ducking = Ducking::default();

        assert_eq!(ducking.step(&state, &sink(1, 50, 0, 0)), []);
        assert_eq!(ducking.step(&state, &STARTED), [set(10, 0, 1)]);
        assert_eq!(ducking.step(&state, &sink(1, 10, 0, 1)), []);
        assert_eq!(ducking.step(&state, &ENDED), [restore(50, 0, 2)]);
        let adjusted = Fact::Audio(AudioFact::VolumeAdjusted);
        assert_eq!(ducking.step(&state, &adjusted), []);
        assert!(ducking.pending_volume.is_none());
        assert_eq!(ducking.step(&state, &sink(1, 10, 0, 2)), []);
        assert_eq!(ducking.step(&state, &STARTED), [set(0, 0, 3)]);
    }

    #[test]
    fn a_dropped_duck_allows_a_second_duck_or_an_unrelated_volume_change() {
        for ended_off_default in [false, true] {
            let state = connected();
            let mut ducking = Ducking::default();

            assert_eq!(ducking.step(&state, &sink(1, 80, 0, 0)), []);
            assert_eq!(ducking.step(&state, &STARTED), [set(15, 0, 1)]);
            let skipped = Fact::Audio(AudioFact::VolumeSkipped(VolumeStamp {
                epoch: 0,
                writes: 1,
            }));
            assert_eq!(ducking.step(&state, &skipped), []);
            let other_output = |volume| {
                let Fact::Audio(AudioFact::Sink(Some(mut reading))) = sink(1, volume, 0, 1) else {
                    panic!("expected a sink reading");
                };
                reading.is_default = false;
                Fact::Audio(AudioFact::Sink(Some(reading)))
            };
            assert_eq!(ducking.step(&state, &other_output(80)), []);
            if ended_off_default {
                assert_eq!(ducking.step(&state, &other_output(15)), []);
                assert_eq!(ducking.step(&state, &ENDED), []);
                assert_eq!(ducking.step(&state, &sink(1, 15, 0, 1)), []);
                assert_eq!(ducking.step(&state, &STARTED), [set(5, 0, 2)]);
            } else {
                assert_eq!(ducking.step(&state, &sink(1, 80, 0, 1)), []);
                assert_eq!(ducking.step(&state, &STARTED), [set(15, 0, 2)]);
            }
        }
    }

    #[test]
    fn a_skipped_older_request_cannot_cancel_a_newer_duck() {
        let state = connected();
        let mut ducking = Ducking::default();

        assert_eq!(ducking.step(&state, &sink(1, 80, 0, 0)), []);
        assert_eq!(ducking.step(&state, &STARTED), [set(15, 0, 1)]);
        assert_eq!(ducking.step(&state, &ENDED), [restore(80, 0, 2)]);
        assert_eq!(ducking.step(&state, &STARTED), [set(15, 0, 3)]);
        let skipped = Fact::Audio(AudioFact::VolumeSkipped(VolumeStamp {
            epoch: 0,
            writes: 2,
        }));
        assert_eq!(ducking.step(&state, &skipped), []);
        assert_eq!(ducking.step(&state, &ENDED), [restore(80, 0, 4)]);
    }

    #[test]
    fn a_skip_from_an_old_epoch_cannot_cancel_a_duck() {
        let state = connected();
        let mut ducking = Ducking::default();

        assert_eq!(ducking.step(&state, &sink(1, 80, 1, 0)), []);
        assert_eq!(ducking.step(&state, &STARTED), [set(15, 1, 1)]);
        let skipped = Fact::Audio(AudioFact::VolumeSkipped(VolumeStamp {
            epoch: 0,
            writes: 1,
        }));
        assert_eq!(ducking.step(&state, &skipped), []);
        assert_eq!(ducking.step(&state, &ENDED), [restore(80, 1, 2)]);
    }

    #[test]
    fn a_matching_skip_settles_a_pending_restore() {
        let state = connected();
        let mut ducking = Ducking::default();

        assert_eq!(ducking.step(&state, &sink(1, 80, 0, 0)), []);
        assert_eq!(ducking.step(&state, &STARTED), [set(15, 0, 1)]);
        assert_eq!(ducking.step(&state, &sink(1, 15, 0, 1)), []);
        assert_eq!(ducking.step(&state, &ENDED), [restore(80, 0, 2)]);
        let skipped = Fact::Audio(AudioFact::VolumeSkipped(VolumeStamp {
            epoch: 0,
            writes: 2,
        }));
        assert_eq!(ducking.step(&state, &skipped), []);
        assert!(matches!(ducking.conversation, super::Conversation::Idle));
        assert!(ducking.pending_volume.is_none());
    }

    #[test]
    fn recording_release_defers_a_duck_until_the_muffle_fades() {
        use crate::policy::Policy;

        let at = Duration::from_millis;
        for still_speaking in [true, false] {
            let state = connected();
            let mut policy = Policy::new(None);
            drop(policy.step(&state, sink(1, 80, 0, 0), at(0)));
            drop(policy.step(&state, Fact::Audio(AudioFact::Recording(true)), at(10)));
            drop(policy.step(&state, Fact::Audio(AudioFact::Recording(false)), at(100)));

            assert_eq!(policy.step(&state, STARTED, at(101)), []);
            assert_eq!(policy.deadline(), Some(at(1600)));
            assert_eq!(policy.step(&state, Fact::Tick, at(1599)), []);

            if still_speaking {
                assert_eq!(policy.step(&state, Fact::Tick, at(1600)), [set(15, 0, 1)]);
                assert_eq!(policy.step(&state, ENDED, at(1601)), [restore(80, 0, 2)]);
            } else {
                assert_eq!(policy.step(&state, ENDED, at(1599)), []);
                assert_eq!(policy.step(&state, Fact::Tick, at(1600)), []);
                assert_eq!(policy.step(&state, STARTED, at(1600)), [set(15, 0, 1)]);
            }
        }
    }

    fn defer_duck(ducking: &mut Ducking, state: &State) -> Duration {
        let up = Fact::Link(Signal::Up { device: device() });
        let started_at = Duration::from_millis(1);
        assert_eq!(ducking.step(state, &up), []);
        assert_eq!(ducking.step(state, &sink(1, 80, 0, 0)), []);
        assert_eq!(
            ducking.step(state, &Fact::Audio(AudioFact::Recording(true))),
            []
        );
        assert_eq!(
            ducking.step(state, &Fact::Audio(AudioFact::Recording(false))),
            []
        );
        assert_eq!(ducking.step_at(state, &STARTED, started_at), []);
        let deadline = ducking.deadline().expect("the duck should be deferred");
        assert!(deadline > started_at);
        deadline
    }

    #[test]
    fn link_down_cancels_a_deferred_duck_across_reconnect() {
        let mut state = connected();
        let mut ducking = Ducking::default();
        let deadline = defer_duck(&mut ducking, &state);
        let before_deadline = deadline.saturating_sub(Duration::from_millis(1));

        state.connected = false;
        assert_eq!(
            ducking.step_at(&state, &Fact::Link(Signal::Down), before_deadline),
            []
        );
        state.connected = true;
        let up = Fact::Link(Signal::Up { device: device() });
        assert_eq!(ducking.step_at(&state, &up, before_deadline), []);
        assert_eq!(
            ducking.step_at(&state, &sink(1, 80, 1, 0), before_deadline),
            []
        );
        assert_eq!(ducking.step_at(&state, &Fact::Tick, deadline), []);
    }

    #[test]
    fn following_a_different_device_cancels_a_deferred_duck() {
        let state = connected();
        let mut ducking = Ducking::default();
        let deadline = defer_duck(&mut ducking, &state);
        let before_deadline = deadline.saturating_sub(Duration::from_millis(1));

        let up = Fact::Link(Signal::Up {
            device: "02:00:00:00:00:02".parse().unwrap(),
        });
        assert_eq!(ducking.step_at(&state, &up, before_deadline), []);
        assert_eq!(
            ducking.step_at(&state, &sink(2, 80, 1, 0), before_deadline),
            []
        );
        assert_eq!(ducking.step_at(&state, &Fact::Tick, deadline), []);
    }

    #[test]
    fn recording_while_lowered_requests_a_restore() {
        let state = connected();
        let mut ducking = Ducking::default();

        assert_eq!(ducking.step(&state, &sink(1, 50, 0, 0)), []);
        assert_eq!(ducking.step(&state, &STARTED), [set(10, 0, 1)]);
        assert_eq!(ducking.step(&state, &sink(1, 10, 0, 1)), []);
        assert_eq!(
            ducking.step(&state, &Fact::Audio(AudioFact::Recording(true))),
            [restore(50, 0, 2)]
        );
        assert_eq!(ducking.step(&state, &STARTED), []);
    }

    #[test]
    fn a_restore_to_the_current_volume_awaits_confirmation() {
        let state = connected();
        let mut ducking = Ducking::default();

        assert_eq!(ducking.step(&state, &sink(1, 0, 0, 0)), []);
        assert_eq!(ducking.step(&state, &STARTED), [set(0, 0, 1)]);
        assert!(ducking.pending_volume.is_none());
        assert_eq!(ducking.step(&state, &ENDED), [restore(0, 0, 2)]);
        assert_eq!(ducking.pending_volume.as_ref().unwrap().target.get(), 0);
        assert_eq!(ducking.step(&state, &sink(1, 0, 0, 2)), []);
        assert!(ducking.pending_volume.is_none());
        assert!(matches!(ducking.conversation, super::Conversation::Idle));
    }

    #[test]
    fn every_percent_matches_the_cpp_grid() {
        // The C++ takes the integer fifth before snapping it to the nearest 5%.
        let buckets = [
            (0..=14, 0),
            (15..=39, 5),
            (40..=64, 10),
            (65..=89, 15),
            (90..=100, 20),
        ];
        for (bucket, expected) in buckets {
            for saved in bucket {
                assert_eq!(
                    lowered(Percent::new(saved).unwrap()).get(),
                    expected,
                    "saved volume {saved}%"
                );
            }
        }
    }
}
