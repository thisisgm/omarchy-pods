//! The tap: where the pods' audio of either stream, read on the runtime's thread, meets the chain of the node that is open, and the node's real time half of it.

use std::mem;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use pods_mic::{Chain, Counters, Reader, Resync};
use pods_proto::dictation::AudioNotification;
use pods_proto::uplink::AudioSdu;
use pods_pw::Fill;

/// Where the pods' audio meets the node: the writer's half of the open node's chain, behind a lock the channel's reads and the adapter's opens and drops take.
#[derive(Debug)]
pub(in crate::daemon) struct Tap {
    state: Mutex<State>,
    counters: Arc<Counters>,
    /// Both halves of every chain count time from here.
    epoch: Instant,
    packets: AtomicU64,
    dictation_packets: AtomicU64,
}

#[derive(Debug, Default)]
struct State {
    chain: Option<Chain>,
    stream: Stream,
    /// A START went out and its first audio has not come.
    armed: bool,
    /// A dictation start went out and its first audio has not come.
    dictation_armed: bool,
}

#[derive(Debug, Default)]
enum Stream {
    #[default]
    Uplink,
    Dictation {
        /// The current chain has yet to begin the selected run.
        run_due: bool,
    },
}

impl Tap {
    pub(in crate::daemon) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::default(),
            counters: Arc::default(),
            epoch: Instant::now(),
            packets: AtomicU64::new(0),
            dictation_packets: AtomicU64::new(0),
        })
    }

    /// Queues audio when uplink is selected and a node is open, returning true for the first SDU since `arm`.
    pub(in crate::daemon) fn audio(&self, sdu: &AudioSdu<'_>) -> bool {
        self.packets.fetch_add(1, Ordering::Relaxed);
        let now = self.epoch.elapsed();
        let mut state = self.lock();
        let first = mem::take(&mut state.armed);
        if matches!(state.stream, Stream::Uplink)
            && let Some(chain) = state.chain.as_mut()
        {
            if first {
                chain.reset();
            }
            chain.sdu(now, sdu.units().map(|unit| (unit.stamp, unit.data)));
        }
        first
    }

    /// Audio SDUs read since the daemon started.
    pub(in crate::daemon) fn packets(&self) -> u64 {
        self.packets.load(Ordering::Relaxed)
    }

    /// Queues audio when dictation is selected and a node is open, returning true for the first notification since `arm_dictation`.
    pub(in crate::daemon) fn dictation(&self, audio: &AudioNotification<'_>) -> bool {
        self.dictation_packets.fetch_add(1, Ordering::Relaxed);
        let now = self.epoch.elapsed();
        let mut state = self.lock();
        let first = mem::take(&mut state.dictation_armed);
        if let State {
            chain: Some(chain),
            stream: Stream::Dictation { run_due },
            ..
        } = &mut *state
        {
            if mem::take(run_due) {
                chain.dictation_run();
            }
            let frames = audio.frames().map(|frame| (frame.counter, frame.data));
            chain.frames(now, frames);
        }
        first
    }

    /// Dictation notifications read since the daemon started.
    pub(in crate::daemon) fn dictation_packets(&self) -> u64 {
        self.dictation_packets.load(Ordering::Relaxed)
    }

    /// A dictation start went out: the next dictation audio a chain takes starts a new run of it, on that stream.
    pub(in crate::daemon) fn arm_dictation(&self) {
        let mut state = self.lock();
        state.dictation_armed = true;
        state.stream = Stream::Dictation { run_due: true };
    }

    /// A START went out: the next audio starts a new run of the chain.
    pub(in crate::daemon) fn arm(&self) {
        let mut state = self.lock();
        state.armed = true;
        state.stream = Stream::Uplink;
    }

    pub(in crate::daemon) fn counters(&self) -> &Counters {
        &self.counters
    }

    /// A chain on the tap's counters and instant, its real time half as the node's fill; none when the decoder refuses the configuration.
    pub(super) fn chain(&self) -> Option<(Chain, Feed, Resync)> {
        let (chain, reader, resync) = pods_mic::chain(Arc::clone(&self.counters))?;
        let feed = Feed {
            reader,
            epoch: self.epoch,
        };
        Some((chain, feed, resync))
    }

    /// The node `open` gives, fed by `chain`, which the tap takes only once the node is open.
    pub(super) fn open<N, E>(
        &self,
        chain: Chain,
        open: impl FnOnce() -> Result<N, E>,
    ) -> Result<N, E> {
        let opened = open();
        if opened.is_ok() {
            self.put(Some(chain));
        }
        opened
    }

    /// The tap lets go of the chain before the node it feeds is dropped.
    pub(super) fn close<N>(&self, node: N) {
        self.put(None);
        drop(node);
    }

    /// The old chain is dropped after the lock is let go.
    fn put(&self, chain: Option<Chain>) {
        let old = {
            let mut state = self.lock();
            if chain.is_some()
                && let Stream::Dictation { run_due } = &mut state.stream
            {
                *run_due = true;
            }
            mem::replace(&mut state.chain, chain)
        };
        drop(old);
    }

    /// No holder can panic while it holds the lock, as the decoder's panics are caught inside the chain.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The node's half of the chain: one read of the reader at the time since the tap's instant.
pub(super) struct Feed {
    reader: Reader,
    epoch: Instant,
}

impl Fill for Feed {
    fn fill(&mut self, out: &mut [f32]) -> f64 {
        self.reader.read(self.epoch.elapsed(), out)
    }
}

#[cfg(test)]
mod tests;
