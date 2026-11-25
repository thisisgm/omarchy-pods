//! Conversation Awareness volume, including a restore owed through a missing sink.

use pods_proto::addr::BdAddr;
use pods_proto::awareness::Awareness;

use super::audio::{AudioFact, AudioRequest, Percent, SinkFact, VolumeStamp};
use super::{Effect, Fact};
use crate::link::Signal;
use crate::state::{AudioServer, State};

pub(super) const LOWERED_PERCENT: u8 = 20;
pub(super) const VOLUME_GRID: u8 = 5;

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

#[derive(Debug, Default)]
enum Conversation {
    #[default]
    Idle,
    Lowered {
        saved: Percent,
        ducked: Percent,
        restore_owed: bool,
    },
    /// The wearer set their own volume, so another start report in this conversation is ignored.
    UserAdjusted,
}

impl Ducking {
    pub(super) fn step(&mut self, state: &State, fact: &Fact) -> Vec<Effect> {
        let volume = match fact {
            Fact::Link(Signal::Up { device }) => {
                self.follow(*device);
                None
            }
            Fact::Link(Signal::Down | Signal::Lost { .. }) => {
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
            Fact::Audio(AudioFact::Recorders(recorders))
                if state.audio_server != AudioServer::Idle =>
            {
                self.recording = recorders.dictation != 0 || recorders.other != 0;
                if self.recording && matches!(self.conversation, Conversation::Lowered { .. }) {
                    self.restore()
                } else {
                    None
                }
            }
            Fact::Link(Signal::Awareness(Awareness::Started))
                if state.connected && state.conversational_awareness && !self.recording =>
            {
                self.start()
            }
            Fact::Link(Signal::Awareness(Awareness::Ended)) | Fact::Exit => self.restore(),
            Fact::Changed if !state.conversational_awareness => self.restore(),
            Fact::Link(Signal::Awareness(_) | Signal::Keys(_) | Signal::Uplink(_))
            | Fact::Audio(
                AudioFact::Up
                | AudioFact::Sink(_)
                | AudioFact::Restored(_)
                | AudioFact::Recorders(_)
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
        if let Some(pending) = &mut self.pending_volume {
            if !pending.stale.contains(&pending.target) {
                pending.stale.push(pending.target);
            }
            pending.target = volume;
            pending.superseded = true;
        } else if sink.volume != volume {
            // The audio adapter deduplicates unchanged sink readings.
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
        vec![Effect::Audio(AudioRequest::SetVolume(volume, stamp))]
    }

    fn follow(&mut self, device: BdAddr) {
        self.await_sink();
        if self.device == Some(device) {
            return;
        }
        if let (Some(previous), Conversation::Lowered { saved, ducked, .. }) =
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
            Conversation::Lowered {
                saved,
                ducked,
                restore_owed: true,
            }
        } else {
            Conversation::Idle
        };
        self.device = Some(device);
        self.recording = false;
    }

    fn start(&mut self) -> Option<Percent> {
        let sink = self.sink.filter(|sink| sink.is_default)?;
        if !matches!(self.conversation, Conversation::Idle) {
            return None;
        }
        let saved = self
            .pending_volume
            .as_ref()
            .map_or(sink.volume, |pending| pending.target);
        let ducked = lowered(saved);
        self.conversation = Conversation::Lowered {
            saved,
            ducked,
            restore_owed: false,
        };
        Some(ducked)
    }

    fn restore(&mut self) -> Option<Percent> {
        match self.conversation {
            Conversation::Lowered { saved, .. } if self.sink.is_some() => {
                self.conversation = Conversation::Idle;
                Some(saved)
            }
            Conversation::UserAdjusted => {
                self.conversation = Conversation::Idle;
                None
            }
            Conversation::Idle | Conversation::Lowered { .. } => None,
        }
    }

    fn await_sink(&mut self) {
        self.sink = None;
        self.pending_volume = None;
        self.volume_writes = 0;
        if let Conversation::Lowered { restore_owed, .. } = &mut self.conversation {
            *restore_owed = true;
        }
    }

    fn read_sink(&mut self, sink: SinkFact) -> Option<Percent> {
        let last = self.sink.replace(sink);
        let replaced = last.is_some_and(|last| last.index != sink.index);
        if replaced {
            self.pending_volume = None;
        }
        // The adapter can apply an older write, but counts only writes in the current epoch.
        if replaced || last.is_some_and(|last| last.volume_stamp.epoch != sink.volume_stamp.epoch) {
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
                return None;
            }
            if pending.stale.contains(&sink.volume) {
                return None;
            }
            self.pending_volume = None;
        }
        let Conversation::Lowered {
            saved,
            ducked,
            restore_owed,
        } = self.conversation
        else {
            return None;
        };
        if restore_owed || replaced {
            self.conversation = Conversation::Idle;
            return (sink.volume == ducked).then_some(saved);
        }
        if sink.volume != saved && sink.volume != ducked {
            self.conversation = Conversation::UserAdjusted;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::device;
    use super::{
        AudioFact, AudioRequest, AudioServer, Awareness, Ducking, Effect, Fact, Percent, Signal,
        SinkFact, State, VolumeStamp, lowered,
    };
    use crate::policy::audio::SinkState;

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

    #[test]
    fn a_sink_reading_queued_before_start_sets_the_restore_volume() {
        let state = connected();
        let mut ducking = Ducking::default();

        assert_eq!(ducking.step(&state, &sink(0, 50, 0, 0)), []);
        let queued = sink(0, 45, 0, 0);
        assert_eq!(ducking.step(&state, &STARTED), [set(10, 0, 1)]);
        assert_eq!(ducking.step(&state, &queued), []);
        assert_eq!(ducking.step(&state, &sink(0, 10, 0, 1)), []);
        assert_eq!(ducking.step(&state, &ENDED), [set(45, 0, 2)]);
    }

    #[test]
    fn a_duck_from_the_previous_epoch_restores_the_wearers_volume() {
        let mut state = connected();
        let mut ducking = Ducking::default();
        let up = Fact::Link(Signal::Up { device: device() });

        assert_eq!(ducking.step(&state, &up), []);
        assert_eq!(ducking.step(&state, &sink(1, 50, 0, 0)), []);
        state.connected = false;
        assert_eq!(ducking.step(&state, &Fact::Link(Signal::Down)), []);
        state.connected = true;
        assert_eq!(ducking.step(&state, &up), []);
        assert_eq!(ducking.step(&state, &sink(1, 55, 0, 0)), []);
        assert_eq!(ducking.step(&state, &STARTED), [set(10, 0, 1)]);

        // Re-follow resets the adapter count before it applies the queued duck.
        assert_eq!(ducking.step(&state, &sink(1, 55, 1, 0)), []);
        assert_eq!(ducking.step(&state, &sink(1, 10, 1, 0)), []);
        assert_eq!(ducking.step(&state, &ENDED), [set(55, 1, 1)]);
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
        assert_eq!(ducking.step(&state, &sink(2, 10, 1, 0)), [set(50, 1, 1)]);

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
        assert_eq!(ducking.step(&state, &ENDED), [set(30, 2, 2)]);
        assert_eq!(next_duck, [set(5, 2, 1)]);
    }

    #[test]
    fn a_late_duck_echo_during_the_next_conversation_keeps_the_saved_volume() {
        let state = connected();
        let mut ducking = Ducking::default();

        assert_eq!(ducking.step(&state, &sink(1, 50, 0, 0)), []);
        assert_eq!(ducking.step(&state, &STARTED), [set(10, 0, 1)]);
        assert_eq!(ducking.step(&state, &ENDED), [set(50, 0, 2)]);
        assert_eq!(ducking.step(&state, &STARTED), [set(10, 0, 3)]);
        assert_eq!(ducking.step(&state, &sink(1, 10, 0, 1)), []);
        assert_eq!(ducking.step(&state, &ENDED), [set(50, 0, 4)]);
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
