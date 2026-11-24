//! The rig the chain's support tests share: a chain fed a cycle of tone units, a twin decoder fed the same, and the test's own clock; the twin of the dictation stream's decoder; and how far a decode of a tone fixture may stand from its sine, which the decoder's suite takes from here too.

use std::f32::consts::PI;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use pods_mic::decode::{Decoded, Decoder};
use pods_mic::opus::{self, DICTATION_DECODE_HZ};
use pods_mic::resample::Resampler;
use pods_mic::{
    Chain, Counters, FRAME_MS, NODE_LATENCY_MS, RATE_HZ, Reader, Resync, SAMPLES_PER_UNIT,
    STAMP_STEP, UPLINK_PERIOD_MS, chain,
};
use pods_samples::uplink::{self, tone_unit};

/// The samples step their stamps by what the chain counts a unit as.
const _: () = assert!(uplink::STAMP_STEP == STAMP_STEP);

/// The lines and the levels the units of a run cycle through: 208 units, 1.56 s, before the stream repeats, so a stretch of a shorter tape is found in one place only.
pub(crate) const LINES: [u16; 16] = [8, 15, 22, 13, 20, 11, 18, 9, 16, 23, 14, 21, 12, 19, 10, 17];
pub(crate) const LEVELS: [u8; 13] = [
    172, 177, 182, 174, 179, 171, 176, 181, 173, 178, 183, 175, 180,
];
pub(crate) const UNITS_PER_PACKET: u32 = 4;
#[expect(clippy::as_conversions, reason = "a const context has no `From`")]
pub(crate) const PACKET_FRAMES: usize = UNITS_PER_PACKET as usize * SAMPLES_PER_UNIT as usize;
/// The size of the ring, which the crate keeps to itself; the read of delay.rs that is longer than the ring fails when the two part.
const RING_SAMPLES: usize = 131_072;
/// The newest frames a read can be handed: the reader leaves the writer the unit it may be on.
#[expect(clippy::as_conversions, reason = "a const context has no `From`")]
pub(crate) const HELD: usize = RING_SAMPLES - SAMPLES_PER_UNIT as usize;
/// What a consumer takes at a time: 20 ms.
pub(crate) const QUANTUM: usize = 1280;
pub(crate) const QUANTUM_TIME: Duration = Duration::from_millis(20);
/// The longest any fade lasts, 5 ms; past it a sample is the decoder's own.
pub(crate) const FADE: usize = 320;
/// When the first packet of a run lands.
pub(crate) const FIRST: Duration = Duration::from_millis(100);
/// The steering never asks for more than 2000 parts per million either way.
pub(crate) const MAX_STEER: f64 = 2000e-6;
/// How far a sample under a fade may stand from the shape times the decoder's sample.
pub(crate) const SHAPE_SLACK: f32 = 1e-6;

/// How far a decode of a tone fixture, the tone or the mark, may stand from the sine it was coded from: the bound the suites that measure it hold a lossy codec to.
pub(crate) const TONE_SLACK_HZ: u32 = 5;
pub(crate) const TONE_SLACK_DB: f64 = 1.0;

pub(crate) struct Rig {
    pub(crate) chain: Chain,
    pub(crate) reader: Reader,
    pub(crate) resync: Resync,
    pub(crate) counters: Arc<Counters>,
    /// The test's own decoder, fed the units the chain is fed, and what it gave.
    pub(crate) twin: Decoder,
    pub(crate) decoded: Vec<f32>,
    pub(crate) stamp: u32,
    pub(crate) fed: u32,
}

impl Rig {
    pub(crate) fn new() -> Self {
        let counters = Arc::new(Counters::default());
        let (chain, reader, resync) = chain(Arc::clone(&counters)).unwrap();
        Self {
            chain,
            reader,
            resync,
            counters,
            twin: Decoder::new().unwrap(),
            decoded: Vec::new(),
            stamp: 0,
            fed: 0,
        }
    }

    /// Hands the chain one SDU of these units at `now`, each stamped a step after the one before.
    pub(crate) fn send(&mut self, now: Duration, units: &[Vec<u8>]) {
        let first = self.stamp;
        let stamps = (0..).map(|unit: u32| first.wrapping_add(STAMP_STEP.wrapping_mul(unit)));
        self.chain
            .sdu(now, stamps.zip(units.iter().map(Vec::as_slice)));
        let sent = u32::try_from(units.len()).unwrap();
        self.stamp = first.wrapping_add(STAMP_STEP.wrapping_mul(sent));
    }

    /// Lands `count` units of the cycle in one SDU at `now`; the twin decodes the same units.
    pub(crate) fn land_units(&mut self, now: Duration, count: u32) {
        let cycle = |unit: u32| {
            let at = usize::try_from(unit).unwrap();
            tone_unit(
                LINES[at.rem_euclid(LINES.len())],
                LEVELS[at.rem_euclid(LEVELS.len())],
            )
        };
        let end = self.fed.saturating_add(count);
        let units: Vec<Vec<u8>> = (self.fed..end).map(cycle).collect();
        self.fed = end;
        for unit in &units {
            let Decoded::Samples(samples) = self.twin.unit(unit) else {
                panic!("the twin refused a unit of the cycle");
            };
            self.decoded.extend_from_slice(samples);
        }
        self.send(now, &units);
    }

    pub(crate) fn land(&mut self, now: Duration) {
        self.land_units(now, UNITS_PER_PACKET);
    }

    pub(crate) fn read(&mut self, now: Duration, frames: usize) -> (Vec<f32>, f64) {
        let mut heard = vec![0.0; frames];
        let ratio = self.reader.read(now, &mut heard);
        (heard, ratio)
    }

    /// Plays the landings and the reads in order of time, a landing first where the two meet, and returns what the reads gave, joined.
    pub(crate) fn play(&mut self, lands: &[Duration], reads: &[Duration]) -> Vec<f32> {
        let mut lands = lands.iter().peekable();
        let mut tape = Vec::new();
        for &now in reads {
            while let Some(&at) = lands.next_if(|&&at| at <= now) {
                self.land(at);
            }
            tape.extend(self.read(now, QUANTUM).0);
        }
        tape
    }

    /// A reset gives the chain a fresh decoder, so the twin and what it gave start again with it.
    pub(crate) fn renew_twin(&mut self) {
        (self.twin, self.decoded) = (Decoder::new().unwrap(), Vec::new());
    }

    pub(crate) fn count(counter: &AtomicU32) -> u32 {
        counter.load(Ordering::Relaxed)
    }
}

/// The test's own decoder of the dictation stream, and what it gave at the node's rate.
pub(crate) struct Twin {
    decoder: opus::Decoder,
    to_node: Resampler,
    pub(crate) decoded: Vec<f32>,
}

impl Twin {
    pub(crate) fn new() -> Self {
        Self {
            decoder: opus::Decoder::new().unwrap(),
            to_node: Resampler::new(DICTATION_DECODE_HZ, RATE_HZ).unwrap(),
            decoded: Vec::new(),
        }
    }

    /// What one call into the decoder gave joins what it gave before.
    pub(crate) fn call(&mut self, call: impl FnOnce(&mut opus::Decoder) -> Decoded<'_>) {
        let Decoded::Samples(samples) = call(&mut self.decoder) else {
            panic!("the twin gave no samples");
        };
        self.to_node.push(samples, &mut self.decoded);
    }
}

pub(crate) fn ms(count: u32) -> Duration {
    Duration::from_millis(u64::from(count))
}

pub(crate) fn period() -> Duration {
    ms(UPLINK_PERIOD_MS)
}

/// The time `count` steps after `start`.
pub(crate) fn after(start: Duration, step: Duration, count: u32) -> Duration {
    start.saturating_add(step.saturating_mul(count))
}

/// When packet `packet` lands if it is on time.
pub(crate) fn landing(packet: u32) -> Duration {
    after(FIRST, period(), packet)
}

/// When packets `packets` land if each is on time.
pub(crate) fn on_time(packets: Range<u32>) -> Vec<Duration> {
    packets.map(landing).collect()
}

/// `count` reads a quantum apart, the first at `from`.
pub(crate) fn reads(from: Duration, count: u32) -> Vec<Duration> {
    (0..count)
        .map(|read| after(from, QUANTUM_TIME, read))
        .collect()
}

/// The frame after the last of a read at `now` on a run whose packets land on time: the sample that landed the stated latency before.
pub(crate) fn due(now: Duration) -> usize {
    let before = FIRST.saturating_add(ms(NODE_LATENCY_MS));
    let played = now.saturating_add(period()).checked_sub(before).unwrap();
    frames_in(played)
}

/// The frames the pods send in `time`.
pub(crate) fn frames_in(time: Duration) -> usize {
    let frames = time.as_nanos().saturating_mul(u128::from(RATE_HZ)) / 1_000_000_000;
    usize::try_from(frames).unwrap()
}

/// The frames one frame of the dictation stream is at the node's rate.
pub(crate) fn frame_frames() -> usize {
    frames_in(ms(FRAME_MS))
}

pub(crate) fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|sample| sample.to_bits()).collect()
}

/// Where `stretch` stands in `samples`, bit for bit.
pub(crate) fn find(samples: &[f32], stretch: &[f32]) -> Option<usize> {
    let stretch = bits(stretch);
    let samples = bits(samples);
    samples
        .windows(stretch.len())
        .position(|window| window == stretch)
}

pub(crate) fn silent(samples: &[f32]) -> bool {
    samples.iter().all(|&sample| sample == 0.0)
}

/// Whether the consumer is asked for exactly its own rate.
pub(crate) fn unsteered(ratio: f64) -> bool {
    ratio.to_bits() == 1.0_f64.to_bits()
}

/// Bit for bit: nothing between the decoder and the reader changed a sample.
pub(crate) fn assert_same(heard: &[f32], decoded: &[f32]) {
    assert_eq!(bits(heard), bits(decoded));
}

pub(crate) fn largest_step(samples: &[f32]) -> f32 {
    let steps = samples.windows(2).map(|pair| (pair[1] - pair[0]).abs());
    steps.fold(0.0, f32::max)
}

/// The raised cosine that falls from one to zero over `over` samples.
pub(crate) fn fall(at: usize, over: usize) -> f32 {
    let turn = PI * f32::from(u16::try_from(at).unwrap()) / f32::from(u16::try_from(over).unwrap());
    f32::midpoint(1.0, turn.cos())
}

/// The last unit of `before` backwards under the fall: what audio that ended there plays on as.
pub(crate) fn falling(before: &[f32]) -> impl Iterator<Item = f32> {
    let unit = usize::from(SAMPLES_PER_UNIT);
    let backwards = before.iter().rev().zip(0..unit);
    backwards.map(move |(&sample, at)| sample * fall(at, unit))
}

/// The gain `at` samples into a fade in.
pub(crate) fn rise(at: usize) -> f32 {
    1.0 - fall(at, FADE)
}

/// The start of `samples` as the first of a run comes out: under the writer's fade in and the reader's.
pub(crate) fn run_start(samples: &[f32]) -> impl Iterator<Item = f32> {
    let start = samples.iter().zip(0..FADE);
    start.map(|(&sample, at)| sample * rise(at).powi(2))
}

pub(crate) fn assert_shaped(heard: &[f32], shaped: impl Iterator<Item = f32>) {
    for (at, (&heard, shaped)) in heard.iter().zip(shaped).enumerate() {
        assert!(
            (heard - shaped).abs() <= SHAPE_SLACK,
            "sample {at}: {heard} for {shaped}"
        );
    }
}

/// The read at `now` fades in from nothing on the audio due then, on a run whose packets set their time by landing on time.
pub(crate) fn assert_live(rig: &mut Rig, now: Duration) {
    let (heard, _) = rig.read(now, QUANTUM);
    let live = &rig.decoded[base_of(now)..due(now)];
    assert_shaped(&heard[..FADE], (0..FADE).map(|k| live[k] * rise(k)));
    assert_same(&heard[FADE..], &live[FADE..]);
}

/// The frame of the run that the first sample of a tape holds, when its first read was at `from` and nothing jumped since.
pub(crate) fn base_of(from: Duration) -> usize {
    due(from).checked_sub(QUANTUM).unwrap()
}
