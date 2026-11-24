//! The microphone uplink as samples: units are decoded on the thread that reads the socket into a ring, and a real time reader takes them at a delay held by the time packets arrive.

mod conceal;
pub mod decode;
pub mod opus;
mod ring;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use conceal::{Tail, fade_in};
use decode::{Decoded, Decoder};
use ring::Ring;
pub use ring::{Reader, Resync};

/// The decoder's rate, which the configuration declares as 48 kHz.
pub const RATE_HZ: u32 = 64_000;
/// The samples one unit decodes to under `EXPECTED_CONFIG`.
pub const SAMPLES_PER_UNIT: u16 = 480;
/// The configuration the host supplies for the stream: AAC-ELD, mono, 480 sample frames, declared at 48 kHz.
pub const EXPECTED_CONFIG: [u8; 4] = [0xf8, 0xe6, 0x30, 0x00];
/// What one unit adds to the stamp, which counts samples.
pub const STAMP_STEP: u32 = 480;
/// Four units a packet.
pub const UPLINK_PERIOD_MS: u32 = 30;
/// How late a packet may land and still be played whole.
pub const JITTER_COVER_MS: u32 = 55;
/// The delay the reader holds, which is the latency a source states for it.
pub const NODE_LATENCY_MS: u32 = UPLINK_PERIOD_MS + JITTER_COVER_MS;
/// The most the stated latency may be: a recorder caps the wait after its stop at 100 ms, and this limit stands 10 ms inside it.
pub const NODE_LATENCY_LIMIT_MS: u32 = 90;
const _: () = assert!(NODE_LATENCY_MS <= NODE_LATENCY_LIMIT_MS);
/// Twice the units a packet holds; the rest of an SDU is dropped and counted.
pub const MAX_UNITS_PER_SDU: usize = 8;

/// The units the pods put in one packet, which the period and both caps follow.
const UNITS_PER_PACKET: u32 = 4;
/// Two packets: a larger hole in the stamps is a new run, so no stamp can make the chain write silence without end.
const MAX_GAP_UNITS: u32 = 2 * UNITS_PER_PACKET;
/// About one second of packets: the earliest arrival in it is the pods' own cadence without the radio's delay.
pub const ARRIVAL_WINDOW: usize = 32;
const FRAMES_PER_MS: u16 = 64;
#[expect(clippy::as_conversions, reason = "a const context has no `From`")]
const _: () = assert!(
    FRAMES_PER_MS as u32 * 1000 == RATE_HZ
        && SAMPLES_PER_UNIT as u32 == STAMP_STEP
        && UPLINK_PERIOD_MS * RATE_HZ == UNITS_PER_PACKET * STAMP_STEP * 1000
        && MAX_UNITS_PER_SDU == 2 * UNITS_PER_PACKET as usize
);

#[derive(Debug, Default)]
pub struct Counters {
    /// Times the ring ran dry under a recorder that was being handed audio.
    pub underruns: AtomicU32,
    /// Units written in place of audio that was lost or refused.
    pub concealed: AtomicU32,
    /// Units the decoder refused, and units past `MAX_UNITS_PER_SDU` in one SDU.
    pub decode_errors: AtomicU32,
    /// Panics caught inside the decoder, each answered with a fresh decoder.
    pub decoder_resets: AtomicU32,
}

/// The writer's half, the real time half and the handle that restarts the real time half at its target; none when the decoder refuses `EXPECTED_CONFIG`.
#[must_use]
pub fn chain(counters: Arc<Counters>) -> Option<(Chain, Reader, Resync)> {
    let ring = Arc::new(Ring::new());
    let (reader, resync) = Reader::new(Arc::clone(&ring), Arc::clone(&counters));
    let chain = Chain {
        decoder: Decoder::new()?,
        ring,
        counters,
        expected: None,
        faded: true,
        arrivals: Arrivals::default(),
    };
    Some((chain, reader, resync))
}

/// The units a unit's stamp says were lost before it, against the stamp the run expected next; none when the stamp starts a new run.
fn lost_before(expected: Option<u32>, stamp: u32) -> Option<u32> {
    let Some(expected) = expected else {
        return Some(0);
    };
    let lost = stamp.wrapping_sub(expected) / STAMP_STEP;
    (lost <= MAX_GAP_UNITS).then_some(lost)
}

/// What the chain writes for one unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitWrites {
    /// One unit in place of each lost before it, then the unit itself.
    pub units: u32,
    /// The stamp started a new run, which empties the window of arrivals.
    pub new_run: bool,
}

/// What the chain writes for a unit with `stamp`, when the run expected `expected` next.
#[must_use]
pub fn unit_writes(expected: Option<u32>, stamp: u32) -> UnitWrites {
    match lost_before(expected, stamp) {
        Some(lost) => UnitWrites {
            units: lost.saturating_add(1),
            new_run: false,
        },
        None => UnitWrites {
            units: 1,
            new_run: true,
        },
    }
}

/// The arrivals of the last `ARRIVAL_WINDOW` packets of a run, each its landing less the time its audio stands for, in ns.
#[derive(Debug)]
pub struct Arrivals(VecDeque<i64>);

impl Default for Arrivals {
    fn default() -> Self {
        Self(VecDeque::with_capacity(ARRIVAL_WINDOW))
    }
}

impl Arrivals {
    /// Takes the arrival of the run's next packet and gives the earliest of the window, the packet's own among them.
    pub fn push(&mut self, arrival: i64) -> i64 {
        if self.0.len() == ARRIVAL_WINDOW {
            self.0.pop_front();
        }
        self.0.push_back(arrival);
        self.0.iter().copied().fold(arrival, i64::min)
    }
}

/// The writer's half: it lives on the thread that reads the socket.
#[derive(Debug)]
pub struct Chain {
    decoder: Decoder,
    ring: Arc<Ring>,
    counters: Arc<Counters>,
    /// The stamp the next unit of the run carries; none before the run's first unit.
    expected: Option<u32>,
    /// Whether the last unit written was no audio: none yet in this run, or a concealed one.
    faded: bool,
    /// What `Ring::arrival_at` gave for the last packets of the run.
    arrivals: Arrivals,
}

impl Chain {
    /// The units of one SDU as (stamp, body), and the time the SDU arrived.
    pub fn sdu<'a>(&mut self, now: Duration, units: impl Iterator<Item = (u32, &'a [u8])>) {
        let mut units = units;
        let mut wrote = false;
        for (stamp, body) in units.by_ref().take(MAX_UNITS_PER_SDU) {
            self.unit(stamp, |decoder| decoder.unit(body));
            wrote = true;
        }
        let dropped = u32::try_from(units.count()).unwrap_or(u32::MAX);
        self.counters
            .decode_errors
            .fetch_add(dropped, Ordering::Relaxed);
        if wrote {
            let earliest = self.arrivals.push(self.ring.arrival_at(now));
            self.ring.publish(earliest);
        }
    }

    /// A new run of the uplink: nothing written before it and not yet heard is played, and the next unit's stamp is its start.
    pub fn reset(&mut self) {
        self.renew_decoder();
        self.ring.start_run();
        self.arrivals = Arrivals::default();
        self.expected = None;
        self.faded = true;
    }

    /// The configuration was accepted when the chain was made; were it refused now, the old decoder would stay.
    fn renew_decoder(&mut self) {
        if let Some(fresh) = Decoder::new() {
            self.decoder = fresh;
        }
    }

    /// Writes what the stamp says was lost before this unit, then the unit or what stands for it.
    fn unit(&mut self, stamp: u32, decode: impl FnOnce(&mut Decoder) -> Decoded<'_>) {
        match lost_before(self.expected, stamp) {
            Some(lost) => (0..lost).for_each(|_| self.conceal()),
            None => self.reset(),
        }
        self.expected = Some(stamp.wrapping_add(STAMP_STEP));
        match decode(&mut self.decoder) {
            Decoded::Samples(samples) => {
                let fading = self.faded;
                let unit = samples.iter().zip(0_u16..);
                let gain = |at| if fading { fade_in(at) } else { 1.0 };
                self.ring.write(unit.map(|(&sample, at)| sample * gain(at)));
                self.faded = false;
            }
            Decoded::Error => {
                self.counters.decode_errors.fetch_add(1, Ordering::Relaxed);
                self.conceal();
            }
            Decoded::Panic => {
                self.counters.decoder_resets.fetch_add(1, Ordering::Relaxed);
                self.renew_decoder();
                self.conceal();
            }
        }
    }

    /// One unit in place of audio that never came: the unit before it played backwards under the fall, then silence for as long as none comes.
    fn conceal(&mut self) {
        let last = self.ring.end().saturating_sub(1);
        let mut tail = if self.faded {
            Tail::NONE
        } else {
            Tail::of(last, SAMPLES_PER_UNIT, 1.0, self.ring.run())
        };
        let ring = &self.ring;
        ring.write((0..SAMPLES_PER_UNIT).map(|_| tail.next(ring).unwrap_or(0.0)));
        self.faded = true;
        self.counters.concealed.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests;
