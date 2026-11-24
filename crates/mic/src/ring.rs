//! The ring between the thread that decodes and the real time reader, and the reader that holds its delay by the time packets arrive.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use super::conceal::{FADE_FRAMES, Tail, UNIT_FRAMES, fade_in};
use super::{Counters, FRAMES_PER_MS, NODE_LATENCY_MS, RATE_HZ};

/// A power of two, 256 ms at 64 kHz and more at 48: more than twice the furthest the reader stands behind the writer, about 110 ms.
const RING_SAMPLES: u64 = 16_384;
const _: () = assert!(RING_SAMPLES.is_power_of_two());
/// Half a period: more than steering leaves standing, less than one missed cycle of a 20 ms recorder.
const REALIGN_MS: u16 = 15;
/// A standing distance of about 0.9 ms holds the drift measured, 172 parts per million.
const STEER_PPM_PER_MS: f64 = 200.0;
/// Inaudible as pitch, and more than ten times the drift measured.
const STEER_MAX_PPM: f64 = 2000.0;
/// One late packet does not move the pitch.
const STEER_SMOOTH_MS: u16 = 2000;
const PARTS_PER_MILLION: f64 = 1e6;
const NS_PER_SECOND: u32 = 1_000_000_000;
const NS_PER_MS: i64 = 1_000_000;

/// `count` times `by` over `over`, rounded down: frames from ns and back at a rate that need not divide a second.
fn scaled(count: i64, by: u32, over: u32) -> i64 {
    let wide = i128::from(count).saturating_mul(i128::from(by));
    let end = if count < 0 { i64::MIN } else { i64::MAX };
    let whole = wide.checked_div_euclid(i128::from(over));
    whole
        .and_then(|whole| i64::try_from(whole).ok())
        .unwrap_or(end)
}

fn ns_of(now: Duration) -> i64 {
    i64::try_from(now.as_nanos()).unwrap_or(i64::MAX)
}

/// A count of frames as a float; no read and no distance of the reader comes near the bound it is held to.
fn float_of(frames: i64) -> f64 {
    let held = frames.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
    i32::try_from(held).map_or(0.0, f64::from)
}

/// The run's arrival in ns or none, in one atomic word: `i64::MIN` is kept for none, and an arrival of that value is stored one ns later.
#[derive(Debug)]
struct Arrival(AtomicI64);

impl Arrival {
    const NONE: i64 = i64::MIN;

    const fn none() -> Self {
        Self(AtomicI64::new(Self::NONE))
    }

    fn store(&self, arrival: Option<i64>) {
        let word = arrival.map_or(Self::NONE, |ns| ns.max(Self::NONE.saturating_add(1)));
        self.0.store(word, Ordering::Release);
    }

    fn load(&self) -> Option<i64> {
        let word = self.0.load(Ordering::Acquire);
        (word != Self::NONE).then_some(word)
    }
}

/// The stream of the pods a run is of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Path {
    Uplink,
    Dictation,
}

/// What the ring holds at one frame, with the floor of the run the frame was judged against.
pub(crate) enum Frame {
    Sample {
        sample: f32,
        run: u64,
    },
    /// Not written yet.
    Dry {
        run: u64,
    },
    /// Of an earlier run, from before the chain was made, or overwritten.
    Gone,
}

#[derive(Debug)]
pub(crate) struct Ring {
    /// The bits of each float.
    samples: Box<[AtomicU32]>,
    /// Samples written since the ring was made; it never goes back.
    written: AtomicU64,
    /// Where the run began: of what stands before it the reader hands out only the fall of audio it was playing.
    floor: AtomicU64,
    /// Whether the run is of the dictation stream.
    dictation: AtomicBool,
    /// The earliest `now` less the time `written` stood for, over the last packets of the run, in ns; none while no packet of the run has landed.
    arrival: Arrival,
}

impl Ring {
    pub(crate) fn new() -> Self {
        Self {
            samples: (0..RING_SAMPLES).map(|_| AtomicU32::new(0)).collect(),
            written: AtomicU64::new(0),
            floor: AtomicU64::new(0),
            dictation: AtomicBool::new(false),
            arrival: Arrival::none(),
        }
    }

    fn slot(&self, index: u64) -> Option<&AtomicU32> {
        let at = usize::try_from(index & RING_SAMPLES.saturating_sub(1)).ok()?;
        self.samples.get(at)
    }

    /// The frame after the last one written.
    pub(crate) fn end(&self) -> i64 {
        i64::try_from(self.written.load(Ordering::Relaxed)).unwrap_or(i64::MAX)
    }

    /// Never waits, and overwrites the oldest; the index is shown at least every unit, which is as far past it as the reader's lap check lets the writer be.
    pub(crate) fn write(&self, samples: impl Iterator<Item = f32>) {
        let mut end = self.written.load(Ordering::Relaxed);
        for (sample, of_the_unit) in samples.zip((1..=UNIT_FRAMES).cycle()) {
            if let Some(slot) = self.slot(end) {
                // Release, so a reader that sees this sample sees the write index it was stored under.
                slot.store(sample.to_bits(), Ordering::Release);
            }
            end = end.saturating_add(1);
            if of_the_unit == UNIT_FRAMES {
                self.written.store(end, Ordering::Release);
            }
        }
        self.written.store(end, Ordering::Release);
    }

    /// Where the run began.
    pub(crate) fn run(&self) -> u64 {
        self.floor.load(Ordering::Relaxed)
    }

    /// The stream the run is of.
    pub(crate) fn path(&self) -> Path {
        if self.dictation.load(Ordering::Relaxed) {
            Path::Dictation
        } else {
            Path::Uplink
        }
    }

    pub(crate) fn start_run(&self, path: Path) {
        self.dictation
            .store(path == Path::Dictation, Ordering::Release);
        self.floor
            .store(self.written.load(Ordering::Relaxed), Ordering::Release);
        self.arrival.store(None);
    }

    /// `now` less the time the write index stands for, in ns.
    pub(crate) fn arrival_at(&self, now: Duration) -> i64 {
        ns_of(now).saturating_sub(scaled(self.end(), NS_PER_SECOND, RATE_HZ))
    }

    pub(crate) fn publish(&self, earliest: i64) {
        self.arrival.store(Some(earliest));
    }

    /// The frame that should follow the last of a read at `now`: the sample whose time is the stated latency before; none while no packet of the run has landed.
    fn target(&self, now: Duration) -> Option<i64> {
        let arrival = self.arrival.load()?;
        let latency = i64::from(NODE_LATENCY_MS).saturating_mul(NS_PER_MS);
        let played = ns_of(now).saturating_sub(arrival).saturating_sub(latency);
        Some(scaled(played, RATE_HZ, NS_PER_SECOND))
    }

    pub(crate) fn frame(&self, frame: i64) -> Frame {
        self.frame_of(self.floor.load(Ordering::Acquire), frame)
    }

    /// What the ring holds at a frame of the run that began at `run`.
    fn frame_of(&self, run: u64, frame: i64) -> Frame {
        let Ok(index) = u64::try_from(frame) else {
            return Frame::Gone;
        };
        if index < run {
            return Frame::Gone;
        }
        if index >= self.written.load(Ordering::Acquire) {
            return Frame::Dry { run };
        }
        let bits = self.slot(index).map(|slot| slot.load(Ordering::Acquire));
        // Read again after the sample: the writer stores up to one unit past this index, and a ring's length on that is this slot.
        let ahead = self.written.load(Ordering::Acquire).saturating_sub(index);
        let kept = ahead.saturating_add(u64::from(UNIT_FRAMES)) <= RING_SAMPLES;
        match bits {
            Some(bits) if kept => Frame::Sample {
                sample: f32::from_bits(bits),
                run,
            },
            _ => Frame::Gone,
        }
    }

    /// The sample at a frame of the run that began at `run`, or zero where the ring holds none of it.
    pub(crate) fn sample(&self, run: u64, frame: i64) -> f32 {
        match self.frame_of(run, frame) {
            Frame::Sample { sample, .. } => sample,
            Frame::Dry { .. } | Frame::Gone => 0.0,
        }
    }
}

/// Asks the reader to drop where it stands and start at its target, as for a recorder that just linked.
#[derive(Debug, Clone)]
pub struct Resync {
    asked: Arc<AtomicBool>,
}

impl Resync {
    pub fn request(&self) {
        self.asked.store(true, Ordering::Release);
    }
}

/// What the reader's last frame was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Was {
    Sample,
    Dry,
    Gone,
}

/// The real time half: it allocates nothing, locks nothing and calls nothing that waits.
#[derive(Debug)]
pub struct Reader {
    ring: Arc<Ring>,
    counters: Arc<Counters>,
    resync: Resync,
    /// The frame the next read opens on; it moves by what is read, whatever was handed out.
    next: i64,
    was: Was,
    /// Samples handed out since the last frame that was none.
    fade: u16,
    /// The run of the last sample handed out since a recorder linked.
    heard: Option<u64>,
    /// What still sounds of the last two ends of audio where the reader stood, and of the audio a jump left.
    ended: Tail,
    earlier: Tail,
    left: Tail,
    /// The smoothed distance from the target; above zero the reader is ahead.
    ahead_ms: f64,
    /// A recorder linked and the reader has not been moved to a target since.
    linking: bool,
}

impl Reader {
    /// The reader, and the one handle to its flag.
    pub(crate) fn new(ring: Arc<Ring>, counters: Arc<Counters>) -> (Self, Resync) {
        let resync = Resync {
            asked: Arc::default(),
        };
        let reader = Self {
            ring,
            counters,
            resync: resync.clone(),
            next: 0,
            was: Was::Gone,
            fade: 0,
            heard: None,
            ended: Tail::NONE,
            earlier: Tail::NONE,
            left: Tail::NONE,
            ahead_ms: 0.0,
            linking: false,
        };
        (reader, resync)
    }

    /// Fills `out` with the frames due at `now`, and returns the rate ratio the consumer should run at.
    pub fn read(&mut self, now: Duration, out: &mut [f32]) -> f64 {
        let frames = i64::try_from(out.len()).unwrap_or(i64::MAX);
        if self.resync.asked.swap(false, Ordering::AcqRel) {
            // A recorder that links hears nothing from before it, whether or not a packet of the run has landed.
            (self.ended, self.earlier, self.left) = (Tail::NONE, Tail::NONE, Tail::NONE);
            (self.heard, self.was) = (None, Was::Gone);
            self.linking = true;
        }
        let target = self.ring.target(now);
        if let Some(target) = target {
            let off = self.next.saturating_add(frames).saturating_sub(target);
            let bound = u64::from(REALIGN_MS.saturating_mul(FRAMES_PER_MS));
            // A realign waits out the fall of the one before it, which a second would cut short.
            if self.linking || (off.unsigned_abs() > bound && !self.left.falling()) {
                self.jump(target.saturating_sub(frames));
            }
        }
        for sample in &mut *out {
            *sample = self.frame();
        }
        target.map_or(1.0, |target| self.steer(target, frames))
    }

    /// How far the last read stood from its target, smoothed, in ms; above zero the reader is ahead.
    #[must_use]
    pub fn ahead_ms(&self) -> f64 {
        self.ahead_ms
    }

    /// Whether the last read reached past the last frame written so far.
    #[must_use]
    pub fn read_all(&self) -> bool {
        self.next >= self.ring.end()
    }

    /// The audio a jump leaves falls away while the new fades in.
    fn jump(&mut self, to: i64) {
        if let (Was::Sample, Some(run)) = (self.was, self.heard) {
            let last = self.next.saturating_sub(1);
            self.left = Tail::of(last, FADE_FRAMES, fade_in(self.fade), run);
            self.was = Was::Gone;
        }
        self.next = to;
        self.ahead_ms = 0.0;
        self.linking = false;
    }

    /// The audio handed out up to the frame before `at` ended there: its last unit plays on backwards under the fall.
    fn end_audio(&mut self, at: i64) {
        if let (Was::Sample, Some(run)) = (self.was, self.heard) {
            let last = at.saturating_sub(1);
            // The end before the last has fallen by now: with no realign between, a third end comes more than a unit after the first.
            self.earlier = self.ended;
            self.ended = Tail::of(last, UNIT_FRAMES, fade_in(self.fade), run);
        }
    }

    fn frame(&mut self) -> f32 {
        let at = self.next;
        self.next = at.saturating_add(1);
        let mut out = match self.ring.frame(at) {
            Frame::Sample { sample, run } => {
                if self.was != Was::Sample {
                    self.fade = 0;
                }
                let gain = fade_in(self.fade);
                self.fade = self.fade.saturating_add(1);
                (self.was, self.heard) = (Was::Sample, Some(run));
                sample * gain
            }
            Frame::Dry { run } => {
                if self.was != Was::Dry && self.heard == Some(run) {
                    self.counters.underruns.fetch_add(1, Ordering::Relaxed);
                }
                self.end_audio(at);
                self.was = Was::Dry;
                0.0
            }
            Frame::Gone => {
                self.end_audio(at);
                self.was = Was::Gone;
                0.0
            }
        };
        for tail in [&mut self.ended, &mut self.earlier, &mut self.left] {
            if let Some(falling) = tail.next(&self.ring) {
                out += falling;
            }
        }
        out
    }

    /// The ratio is 1 less a share of the smoothed distance, so a reader that is ahead asks for fewer frames a second.
    fn steer(&mut self, target: i64, frames: i64) -> f64 {
        let per_ms = f64::from(FRAMES_PER_MS);
        let ahead_ms = float_of(self.next.saturating_sub(target)) / per_ms;
        let smooth_frames = f64::from(STEER_SMOOTH_MS) * per_ms;
        let share = 1.0 - (-float_of(frames) / smooth_frames).exp();
        self.ahead_ms += (ahead_ms - self.ahead_ms) * share;
        let ppm = (self.ahead_ms * STEER_PPM_PER_MS).clamp(-STEER_MAX_PPM, STEER_MAX_PPM);
        1.0 - ppm / PARTS_PER_MILLION
    }
}

#[cfg(test)]
mod tests;
