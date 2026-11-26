//! The microphone of the pods as samples: uplink units and dictation frames are decoded on the thread that reads the socket into a ring, and a real time reader takes them at a delay held by the time packets arrive.

mod conceal;
pub mod decode;
mod dictation;
pub mod level;
pub mod opus;
pub mod resample;
mod ring;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use conceal::{Tail, UNIT_FRAMES, fade_in};
use decode::{Decoded, Decoder};
use dictation::Dictation;
pub use dictation::{
    FRAME_MS, MAX_FRAMES_PER_NOTIFICATION, MAX_GAP_FRAMES, PREROLL_STAGE_FRAMES, STEADY_FRAMES,
};
use resample::Resampler;
pub use ring::{OPENING_EXIT_MS, OPENING_MAX_PPM, OPENING_RAMP_QUANTA, Reader, Resync};
use ring::{Path, Ring};

/// The node's rate, the only one a reader or a source sees.
pub const RATE_HZ: u32 = 64_000;
/// The AAC decoder's rate, which the configuration declares as 48 kHz.
pub const UPLINK_DECODE_HZ: u32 = 64_000;
/// The samples one unit decodes to under `EXPECTED_CONFIG`.
pub const SAMPLES_PER_UNIT: u16 = 480;
/// The configuration the host supplies for the stream: AAC-ELD, mono, 480 sample frames, declared at 48 kHz.
pub const EXPECTED_CONFIG: [u8; 4] = [0xf8, 0xe6, 0x30, 0x00];
/// The wire stamp's clock, inferred from 180 ticks per 7.5 ms unit.
pub const UPLINK_STAMP_HZ: u32 = 24_000;
/// The number of wire ticks in one decoded unit.
#[expect(clippy::as_conversions, reason = "a const context has no `From`")]
pub const STAMP_STEP: u32 = SAMPLES_PER_UNIT as u32 * UPLINK_STAMP_HZ / UPLINK_DECODE_HZ;
/// Four units a packet.
pub const UPLINK_PERIOD_MS: u32 = 30;
/// How late a packet may land and still be played whole.
pub const JITTER_COVER_MS: u32 = 55;
/// The latency the node states on both paths. The ring holds it on uplink and holds [`DOAP_LATENCY_MS`] on dictation.
pub const NODE_LATENCY_MS: u32 = UPLINK_PERIOD_MS + JITTER_COVER_MS;
/// The jitter the shorter dictation cadence can cover.
const DOAP_JITTER_COVER_MS: u32 = 65;
/// The delay the reader holds on the dictation path.
pub const DOAP_LATENCY_MS: u32 = FRAME_MS + DOAP_JITTER_COVER_MS;
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
// Three late steady notifications distinguish a delay step from a brief stall.
const DOAP_REANCHOR_PACKETS: usize = 3;
const NS_PER_MS: i64 = 1_000_000;
const FRAMES_PER_MS: u16 = if RATE_HZ == 48_000 { 48 } else { 64 };
#[expect(clippy::as_conversions, reason = "a const context has no `From`")]
const _: () = assert!(
    (RATE_HZ == 48_000 || RATE_HZ == 64_000)
        && FRAMES_PER_MS as u32 * 1000 == RATE_HZ
        && UPLINK_PERIOD_MS * UPLINK_DECODE_HZ == UNITS_PER_PACKET * SAMPLES_PER_UNIT as u32 * 1000
        && UPLINK_PERIOD_MS * UPLINK_STAMP_HZ == UNITS_PER_PACKET * STAMP_STEP * 1000
        && MAX_UNITS_PER_SDU == 2 * UNITS_PER_PACKET as usize
);

#[derive(Debug, Default)]
pub struct Counters {
    /// Times the ring ran dry under a recorder that was being handed audio.
    pub underruns: AtomicU32,
    /// Units and frames written in place of audio that was lost or refused.
    pub concealed: AtomicU32,
    /// Units and frames a decoder refused, and those past the cap of one SDU or one notification.
    pub decode_errors: AtomicU32,
    /// Panics caught inside the decoder, each answered with a fresh decoder.
    pub decoder_resets: AtomicU32,
    /// Times a fresh decoder was refused and the one held stayed.
    pub decoder_renew_refused: AtomicU32,
    /// Dictation frames whose counter the run had passed, of which nothing is written.
    pub dictation_repeats: AtomicU32,
    /// How far behind its target the reader stood when the last opening began, in ms.
    pub opening_behind_ms: AtomicU32,
    /// How long the last opening took to catch up, in ms. Zero until catch-up and for an opening dropped by a run change.
    pub opening_ms: AtomicU32,
    /// Samples the reader handed out whose magnitude after the gain was over the limiter's knee.
    pub limited: AtomicU32,
    /// Samples the reader was about to hand out that were not finite and became zero.
    pub nonfinite: AtomicU32,
    /// Nonempty uplink SDUs taken by the chain in this span.
    pub span_sdus: AtomicU64,
    /// Uplink runs restarted by an unexpected unit stamp in this span.
    pub span_restarts: AtomicU64,
    /// Output frames read as Gone in this span.
    pub span_gone_frames: AtomicU64,
    /// The positive float bits sort in amplitude order.
    span_peak: AtomicU32,
}

impl Counters {
    pub fn reset_span(&self) {
        self.span_sdus.store(0, Ordering::Relaxed);
        self.span_restarts.store(0, Ordering::Relaxed);
        self.span_gone_frames.store(0, Ordering::Relaxed);
        self.span_peak.store(0, Ordering::Relaxed);
    }

    /// The raw decoded peak as a positive fraction of full scale.
    #[must_use]
    pub fn decoded_peak(&self) -> f32 {
        f32::from_bits(self.span_peak.load(Ordering::Relaxed))
    }

    fn measure_peak<'a>(&self, decoded: Decoded<'a>) -> Decoded<'a> {
        if let Decoded::Samples(samples) = decoded {
            let peak = samples
                .iter()
                .filter(|sample| sample.is_finite())
                .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
            self.span_peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
        }
        decoded
    }
}

/// The writer's half, the real time half and the handle that restarts the real time half at its target; none when a decoder will not open.
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
        resampler: Resampler::new(UPLINK_DECODE_HZ, RATE_HZ),
        resampled: Vec::new(),
        dictation: Dictation::new()?,
    };
    Some((chain, reader, resync))
}

/// A fresh decoder in place of the held one; one that will not open leaves the held one in place and is counted.
fn renew<D>(held: &mut D, counters: &Counters, fresh: Option<D>) {
    match fresh {
        Some(fresh) => *held = fresh,
        None => {
            counters
                .decoder_renew_refused
                .fetch_add(1, Ordering::Relaxed);
        }
    }
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

    fn push_dictation(&mut self, arrival: i64) -> i64 {
        let earliest = self.push(arrival);
        let stale = self.0.len().saturating_sub(DOAP_REANCHOR_PACKETS);
        let recent = self.0.range(stale..).copied().fold(arrival, i64::min);
        let latency_ns = i64::from(DOAP_LATENCY_MS).saturating_mul(NS_PER_MS);
        if recent.saturating_sub(earliest) > latency_ns {
            drop(self.0.drain(..stale));
            return recent;
        }
        earliest
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
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
    /// What brings a unit to the node's rate; none while the decoder runs at it.
    resampler: Option<Resampler>,
    /// The unit the resampler made last.
    resampled: Vec<f32>,
    /// What the dictation stream's frames pass on their way to the ring.
    dictation: Dictation,
}

impl Chain {
    /// The units of one SDU as (stamp, body), and the time the SDU arrived; on a run of the dictation stream it is a stale packet, neither written nor counted, until `reset` opens a run of the uplink.
    pub fn sdu<'a>(&mut self, now: Duration, units: impl Iterator<Item = (u32, &'a [u8])>) {
        if self.ring.path() != Path::Uplink {
            return;
        }
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
            self.counters.span_sdus.fetch_add(1, Ordering::Relaxed);
            self.arrived(now);
        }
    }

    /// What landed at `now` tells the run's time: the reader holds its delay by the earliest of the window's arrivals.
    fn arrived(&mut self, now: Duration) {
        let earliest = self.arrivals.push(self.ring.arrival_at(now));
        self.ring.publish(earliest);
    }

    /// A new run of the uplink: nothing written before it and not yet heard is played, and the next unit's stamp is its start.
    pub fn reset(&mut self) {
        renew(&mut self.decoder, &self.counters, Decoder::new());
        self.expected = None;
        if let Some(resampler) = &mut self.resampler {
            resampler.reset();
        }
        self.start_run(Path::Uplink);
    }

    /// Nothing written before the run and not yet heard is played, and the run's first audio follows none.
    fn start_run(&mut self, path: Path) {
        self.ring.start_run(path);
        self.arrivals = Arrivals::default();
        self.faded = true;
    }

    /// Writes what the stamp says was lost before this unit, then the unit or what stands for it.
    fn unit(&mut self, stamp: u32, decode: impl FnOnce(&mut Decoder) -> Decoded<'_>) {
        if let Some(lost) = lost_before(self.expected, stamp) {
            (0..lost).for_each(|_| self.conceal(UNIT_FRAMES));
        } else {
            self.counters.span_restarts.fetch_add(1, Ordering::Relaxed);
            self.reset();
        }
        self.expected = Some(stamp.wrapping_add(STAMP_STEP));
        match self.counters.measure_peak(decode(&mut self.decoder)) {
            Decoded::Samples(samples) => {
                let samples = match &mut self.resampler {
                    Some(resampler) => {
                        self.resampled.clear();
                        resampler.push(samples, &mut self.resampled);
                        &self.resampled
                    }
                    None => samples,
                };
                Self::write_audio(&self.ring, &mut self.faded, samples);
            }
            Decoded::Error => {
                self.counters.decode_errors.fetch_add(1, Ordering::Relaxed);
                self.conceal(UNIT_FRAMES);
            }
            Decoded::Panic => {
                self.counters.decoder_resets.fetch_add(1, Ordering::Relaxed);
                renew(&mut self.decoder, &self.counters, Decoder::new());
                self.conceal(UNIT_FRAMES);
            }
        }
    }

    /// Audio into the ring, under the rise where it follows none.
    fn write_audio(ring: &Ring, faded: &mut bool, samples: &[f32]) {
        let fading = *faded;
        let gain = |at| if fading { fade_in(at) } else { 1.0 };
        let audio = samples.iter().zip(0_u16..);
        ring.write(audio.map(|(&sample, at)| sample * gain(at)));
        *faded = false;
    }

    /// `frames` frames in place of audio that never came: the unit before them played backwards under the fall, then silence for as long as none comes.
    fn conceal(&mut self, frames: u16) {
        let last = self.ring.end().saturating_sub(1);
        let mut tail = if self.faded {
            Tail::NONE
        } else {
            Tail::of(last, UNIT_FRAMES, 1.0, self.ring.run())
        };
        let ring = &self.ring;
        ring.write((0..frames).map(|_| tail.next(ring).unwrap_or(0.0)));
        self.faded = true;
        self.counters.concealed.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests;
