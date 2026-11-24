//! A seeded generator of dictation capture logs, shared by the unit tests and the test of the built command.

use pods_proto::dictation::{Frame, STARTED, STOPPED, TOC_CELT_WIDEBAND_20MS_MONO, encode_audio};
use pods_proto::doap::{self, Handles};
use pods_proto::sdulog::{Direction, Record};

use super::xorshift::next_random;

pub(crate) const NS_PER_MS: u64 = 1_000_000;
pub(crate) const NS_PER_SECOND: u64 = 1_000_000_000;
/// Samples in one 20 ms frame of a 16 kHz stream.
pub(crate) const SAMPLES_PER_FRAME: u32 = 320;
pub(crate) const REPLY_AFTER_MS: u64 = 60;
pub(crate) const AUDIO_AFTER_MS: u64 = 80;
/// How long after the stop write, or after the last packet before it, the generated reply follows.
pub(crate) const STOP_REPLY_AFTER_MS: u64 = 5;
/// Half a frame after the last frame of the session.
pub(crate) const STOP_AFTER_MS: u64 = 10;
const BURST_HEAD_FRAMES: u32 = 4;
/// Frames in every later packet of a burst but the last, which holds what is left.
const BURST_PACKET_FRAMES: u32 = 5;
/// The shortest and the longest generated frame, the table of contents byte included.
pub(crate) const FRAME_BYTES: (u64, u64) = (74, 94);
/// The lowest and the highest generated level byte.
const LEVELS: (u64, u64) = (185, 222);
/// The flag byte of about every second generated frame; the others carry zero.
pub(crate) const FLAGGED: u8 = 0x80;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Plan {
    pub(crate) start_ns: u64,
    /// The sample clock of the peer, 320 samples a frame.
    pub(crate) clock_hz: u32,
    /// Frames delivered before the stop write, the burst among them.
    pub(crate) frames: u32,
    /// Frames of the opening burst, delivered over `burst_ms`; none for 0.
    pub(crate) burst_frames: u32,
    pub(crate) burst_ms: u64,
    /// How far before the start write the first frame begins; a lead needs a burst that delivers it.
    pub(crate) lead_ms: u64,
    /// Each packet after the burst is late by a seeded delay under this, which must stay under one frame.
    pub(crate) jitter_ms: u32,
    pub(crate) seed: u64,
    /// The counter of the first packet.
    pub(crate) first_packet: u8,
    /// A frame counter that is never sent.
    pub(crate) withheld: Option<u16>,
    /// A frame counter and the first byte its frame carries in place of the usual one.
    pub(crate) odd_codec: Option<(u16, u8)>,
    /// Audio packets between the stop write and its reply, and after the reply.
    pub(crate) late: (u32, u32),
}

impl Plan {
    /// A session as a Mac was seen to get one: 63 frames in 250 ms that begin 930 ms before the start write.
    pub(crate) fn seen(start_ns: u64, frames: u32) -> Self {
        Self {
            start_ns,
            clock_hz: 16_000,
            frames,
            burst_frames: 63,
            burst_ms: 250,
            lead_ms: 930,
            jitter_ms: 0,
            seed: 0x5eed_0016,
            first_packet: 0,
            withheld: None,
            odd_codec: None,
            late: (0, 0),
        }
    }

    /// The same session with no burst: its first frame begins at the start write.
    pub(crate) fn without_burst(self) -> Self {
        Self {
            burst_frames: 0,
            lead_ms: 0,
            ..self
        }
    }

    /// When frame `number`, counted from 1, has ended at the peer.
    fn ended_ns(&self, number: u32) -> u64 {
        let samples = u128::from(number).saturating_mul(u128::from(SAMPLES_PER_FRAME));
        let after_ns = samples
            .saturating_mul(u128::from(NS_PER_SECOND))
            .checked_div(u128::from(self.clock_hz))
            .unwrap();
        let first_ns = self
            .start_ns
            .saturating_sub(self.lead_ms.saturating_mul(NS_PER_MS));
        first_ns.saturating_add(u64::try_from(after_ns).unwrap())
    }

    /// How many frames each packet of the burst holds: 4, then fives, then what is left.
    fn burst_packets(&self) -> Vec<u32> {
        let mut sizes = Vec::new();
        let mut left = self.burst_frames;
        while left > 0 {
            let most = if sizes.is_empty() {
                BURST_HEAD_FRAMES
            } else {
                BURST_PACKET_FRAMES
            };
            sizes.push(most.min(left));
            left = left.saturating_sub(most);
        }
        sizes
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Session {
    pub(crate) records: Vec<Record>,
    pub(crate) start_ns: u64,
    pub(crate) stop_ns: u64,
    /// The packet counter the next session would continue from.
    pub(crate) next_packet: u8,
}

struct Stream {
    plan: Plan,
    random: u64,
    records: Vec<Record>,
    /// Frames ended so far, sent or withheld.
    frames: u32,
    packet: u8,
}

impl Stream {
    fn pick(&mut self, (low, high): (u64, u64)) -> u64 {
        let choices = high.saturating_sub(low).saturating_add(1);
        let offset = next_random(&mut self.random).checked_rem(choices);
        low.saturating_add(offset.unwrap_or(0))
    }

    fn push(&mut self, at_ns: u64, direction: Direction, sdu: &[u8]) {
        self.records.push(Record {
            at_ns,
            direction,
            sdu: sdu.to_vec(),
        });
    }

    /// One audio notification of the next `count` frames, no earlier than `from_ns` and never before its last frame has ended.
    fn audio(&mut self, count: u32, from_ns: u64) -> u64 {
        let mut coded: Vec<(u8, u8, u16, Vec<u8>)> = Vec::new();
        let after = self.frames.saturating_add(count);
        for number in self.frames..after {
            let counter = u16::try_from(number).unwrap();
            let len = usize::try_from(self.pick(FRAME_BYTES)).unwrap();
            let mut data = vec![TOC_CELT_WIDEBAND_20MS_MONO];
            while data.len() < len {
                data.push(next_random(&mut self.random).to_le_bytes()[0]);
            }
            if let Some((_, first)) = self.plan.odd_codec.filter(|(odd, _)| *odd == counter) {
                data[0] = first;
            }
            let flag = if self.pick((0, 1)) == 1 { FLAGGED } else { 0 };
            let level = u8::try_from(self.pick(LEVELS)).unwrap();
            if self.plan.withheld != Some(counter) {
                coded.push((flag, level, counter, data));
            }
        }
        self.frames = after;
        let at_ns = from_ns.max(self.plan.ended_ns(self.frames));
        if coded.is_empty() {
            return at_ns;
        }
        let frames: Vec<Frame<'_>> = coded
            .iter()
            .map(|(flag, level, counter, data)| Frame {
                flag: *flag,
                level: *level,
                counter: *counter,
                data,
            })
            .collect();
        let pdu = encode_audio(&Handles::OBSERVED, self.packet, &frames).unwrap();
        self.packet = self.packet.wrapping_add(1);
        self.push(at_ns, Direction::FromPods, &pdu);
        at_ns
    }

    /// One packet of a single frame, late by the seeded jitter.
    fn steady(&mut self) -> u64 {
        let limit_ns = u64::from(self.plan.jitter_ms).saturating_mul(NS_PER_MS);
        let late_ns = next_random(&mut self.random)
            .checked_rem(limit_ns)
            .unwrap_or(0);
        let due_ns = self.plan.ended_ns(self.frames.saturating_add(1));
        self.audio(1, due_ns.saturating_add(late_ns))
    }
}

pub(crate) fn synth(plan: Plan) -> Session {
    let mut stream = Stream {
        plan,
        random: plan.seed,
        records: Vec::new(),
        frames: 0,
        packet: plan.first_packet,
    };
    let start = doap::start(&Handles::OBSERVED);
    stream.push(plan.start_ns, Direction::ToPods, &start);
    let after = |from_ns: u64, ms: u64| from_ns.saturating_add(ms.saturating_mul(NS_PER_MS));
    let replied_ns = after(plan.start_ns, REPLY_AFTER_MS);
    stream.push(replied_ns, Direction::FromPods, &STARTED);
    let burst = plan.burst_packets();
    let gaps = u64::try_from(burst.len().saturating_sub(1).max(1)).unwrap();
    let mut last_ns = plan.start_ns;
    for (count, k) in burst.into_iter().zip(0_u64..) {
        let into_burst_ns = plan.burst_ms.saturating_mul(k).saturating_mul(NS_PER_MS);
        let into_burst_ns = into_burst_ns.checked_div(gaps).unwrap_or(0);
        let due_ns = after(plan.start_ns, AUDIO_AFTER_MS).saturating_add(into_burst_ns);
        last_ns = stream.audio(count, due_ns);
    }
    while stream.frames < plan.frames {
        last_ns = stream.steady();
    }
    let stop_ns = after(last_ns, STOP_AFTER_MS);
    let stop = doap::stop(&Handles::OBSERVED, doap::STOP_REASON_HOST);
    stream.push(stop_ns, Direction::ToPods, &stop);
    let (before_reply, after_reply) = plan.late;
    for _ in 0..before_reply {
        last_ns = stream.steady();
    }
    let stopped_ns = after(stop_ns.max(last_ns), STOP_REPLY_AFTER_MS);
    stream.push(stopped_ns, Direction::FromPods, &STOPPED);
    for _ in 0..after_reply {
        stream.steady();
    }
    stream.records.sort_by_key(|record| record.at_ns);
    Session {
        records: stream.records,
        start_ns: plan.start_ns,
        stop_ns,
        next_packet: stream.packet,
    }
}
