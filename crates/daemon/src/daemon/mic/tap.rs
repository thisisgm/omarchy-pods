//! The tap: where the pods' audio, read on the runtime's thread, meets the chain of the node that is open, and the node's real time half of it.

use std::mem;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use pods_mic::{Chain, Counters, Reader, Resync};
use pods_proto::uplink::AudioSdu;
use pods_pw::Fill;

/// Where the pods' audio meets the node: the writer's half of the open node's chain, behind a lock the channel's reads and the adapter's opens and drops take.
#[derive(Debug)]
pub(in crate::daemon) struct Tap {
    chain: Mutex<Option<Chain>>,
    counters: Arc<Counters>,
    /// Both halves of every chain count time from here.
    epoch: Instant,
    packets: AtomicU64,
    /// A START went out and its first audio has not come.
    armed: AtomicBool,
}

impl Tap {
    pub(in crate::daemon) fn new() -> Arc<Self> {
        Arc::new(Self {
            chain: Mutex::new(None),
            counters: Arc::default(),
            epoch: Instant::now(),
            packets: AtomicU64::new(0),
            armed: AtomicBool::new(false),
        })
    }

    /// Hands the SDU's units to the open node's chain, or drops them with no node; true for the first audio since `arm`.
    pub(in crate::daemon) fn audio(&self, sdu: &AudioSdu<'_>) -> bool {
        let first = self.armed.swap(false, Ordering::AcqRel);
        self.packets.fetch_add(1, Ordering::Relaxed);
        let now = self.epoch.elapsed();
        if let Some(chain) = self.lock().as_mut() {
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

    /// A START went out: the next audio starts a new run of the chain.
    pub(in crate::daemon) fn arm(&self) {
        self.armed.store(true, Ordering::Release);
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
        let old = mem::replace(&mut *self.lock(), chain);
        drop(old);
    }

    /// No holder can panic while it holds the lock, as the decoder's panics are caught inside the chain.
    fn lock(&self) -> MutexGuard<'_, Option<Chain>> {
        self.chain.lock().unwrap_or_else(PoisonError::into_inner)
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
