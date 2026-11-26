//! The ring between the thread that decodes and the real time reader, and the reader that holds its delay by the time packets arrive.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use super::conceal::{FADE_FRAMES, Tail, UNIT_FRAMES, fade_in};
use super::dictation::{FRAME_MS, PREROLL_STAGE_FRAMES};
use super::{Counters, DOAP_LATENCY_MS, FRAMES_PER_MS, NODE_LATENCY_MS, NS_PER_MS, RATE_HZ, level};

/// A power of two, 2 s at 64 kHz and more at 48: the frames an opening may keep, and twice the latency a steady reader stands behind the writer.
const RING_SAMPLES: u64 = 131_072;
#[expect(clippy::as_conversions, reason = "a const context has no `From`")]
const _: () = assert!(
    RING_SAMPLES.is_power_of_two()
        && (PREROLL_STAGE_FRAMES as u64 * FRAME_MS as u64 + 2 * NODE_LATENCY_MS as u64)
            * FRAMES_PER_MS as u64
            <= RING_SAMPLES
);
/// Half a period: more than steady steering leaves standing, less than one missed cycle of a 20 ms recorder.
const REALIGN_MS: u16 = 15;
/// A -45 dBFS peak in decoded audio, before the recorder's gain.
const QUIET_PEAK: f32 = 0.005_623_413;
/// A standing distance of about 0.9 ms holds the drift measured, 172 parts per million.
const STEER_PPM_PER_MS: f64 = 200.0;
/// Inaudible as pitch, and more than ten times the drift measured.
const STEER_MAX_PPM: f64 = 2000.0;
/// Close 40 ms of missed playout plus 5 ms of margin per second.
const CATCH_UP_RATIO: f64 = 1.045;
/// The owner's limit on source audio played above the drift ratio after one stall.
const CATCH_UP_MAX_MS: u16 = 1_000;
/// Bring catch-up back into the drift range over the last 5 ms above the realignment bound.
const CATCH_UP_TAPER_MS: f64 = 5.0;
/// The most lag an opening can leave to steady steering.
pub const OPENING_EXIT_MS: u16 = 2;
/// The number of quanta over which the opening rate returns to nominal.
pub const OPENING_RAMP_QUANTA: u32 = 4;
/// Ten percent, inside the speeds a speech recogniser is trained on.
pub const OPENING_MAX_PPM: f64 = 100_000.0;
/// One late packet does not move the pitch.
const STEER_SMOOTH_MS: u16 = 2000;
const PARTS_PER_MILLION: f64 = 1e6;
const NS_PER_SECOND: u32 = 1_000_000_000;
const NO_RESYNC: u64 = u64::MAX;
/// The digital silence ceiling, about -90 dBFS peak, well below speech.
const SILENCE_PEAK: f32 = 0.000_03;
/// Two seconds of quiet output, longer than either path's jitter cover and the staged opening.
const SILENCE_FRAMES: u32 = 2 * RATE_HZ;

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

/// A time in ns or none, in one atomic word: `i64::MIN` is kept for none, and a time of that value is stored one ns later.
#[derive(Debug)]
struct Moment(AtomicI64);

impl Moment {
    const NONE: i64 = i64::MIN;

    const fn none() -> Self {
        Self(AtomicI64::new(Self::NONE))
    }

    fn store(&self, moment: Option<i64>) {
        let word = moment.map_or(Self::NONE, |ns| ns.max(Self::NONE.saturating_add(1)));
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
    /// The position just after the most recent decoded sample above the silence ceiling.
    signal_end: AtomicU64,
    /// How far the reader got through audio already written, published after the quiet count.
    played_until: AtomicI64,
    quiet_frames: AtomicU32,
    /// Where the run began: of what stands before it the reader hands out only the fall of audio it was playing.
    floor: AtomicU64,
    /// Counts runs even when one ends without writing a sample.
    generation: AtomicU64,
    /// The generation at the first pending resync request, or `NO_RESYNC`.
    resync_from: AtomicU64,
    /// Whether the run is of the dictation stream.
    dictation: AtomicBool,
    finished: AtomicBool,
    /// The earliest `now` less the time `written` stood for, over the last packets of the run, in ns; none while no packet of the run has landed.
    arrival: Moment,
    /// The reader's clock at its first read after a recorder linked; only the reader writes it.
    edge: Moment,
    /// The edge the run's opening was placed against, none when no recorder waited; only the chain writes it.
    opened: Moment,
}

impl Ring {
    pub(crate) fn new() -> Self {
        Self {
            samples: (0..RING_SAMPLES).map(|_| AtomicU32::new(0)).collect(),
            written: AtomicU64::new(0),
            signal_end: AtomicU64::new(0),
            played_until: AtomicI64::new(0),
            quiet_frames: AtomicU32::new(0),
            floor: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            resync_from: AtomicU64::new(NO_RESYNC),
            dictation: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            arrival: Moment::none(),
            edge: Moment::none(),
            opened: Moment::none(),
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

    /// The oldest index the writer has not lapped, allowing for its unpublished unit.
    fn oldest(&self) -> i64 {
        let capacity = i64::try_from(RING_SAMPLES).unwrap_or(i64::MAX);
        self.end()
            .saturating_sub(capacity.saturating_sub(i64::from(UNIT_FRAMES)))
    }

    /// Never waits, and overwrites the oldest; the index is shown at least every unit, which is as far past it as the reader's lap check lets the writer be.
    pub(crate) fn write(&self, samples: impl Iterator<Item = f32>) {
        let mut end = self.written.load(Ordering::Relaxed);
        let mut signal_end = None;
        for (sample, of_the_unit) in samples.zip((1..=UNIT_FRAMES).cycle()) {
            if let Some(slot) = self.slot(end) {
                // Release, so a reader that sees this sample sees the write index it was stored under.
                slot.store(sample.to_bits(), Ordering::Release);
            }
            end = end.saturating_add(1);
            if sample.abs() > SILENCE_PEAK {
                signal_end = Some(end);
            }
            if of_the_unit == UNIT_FRAMES {
                self.written.store(end, Ordering::Release);
            }
        }
        self.written.store(end, Ordering::Release);
        if let Some(signal_end) = signal_end {
            self.signal_end.store(signal_end, Ordering::Release);
        }
    }

    fn note_output(&self, next: i64, out: &[f32], resynced: bool) {
        let quiet = if let Some(last) = out.iter().rposition(|sample| sample.abs() > SILENCE_PEAK) {
            u32::try_from(out.len().saturating_sub(last).saturating_sub(1)).unwrap_or(u32::MAX)
        } else {
            let previous = if resynced {
                0
            } else {
                self.quiet_frames.load(Ordering::Relaxed)
            };
            previous.saturating_add(u32::try_from(out.len()).unwrap_or(u32::MAX))
        };
        self.quiet_frames.store(quiet, Ordering::Relaxed);
        self.played_until.store(next, Ordering::Release);
    }

    pub(crate) fn output_silent(&self) -> bool {
        let played = u64::try_from(self.played_until.load(Ordering::Acquire)).unwrap_or(0);
        let signal = self.signal_end.load(Ordering::Acquire);
        signal <= played.max(self.run())
            && self.quiet_frames.load(Ordering::Relaxed) >= SILENCE_FRAMES
    }

    /// Where the run began.
    pub(crate) fn run(&self) -> u64 {
        self.floor.load(Ordering::Acquire)
    }

    /// The stream the run is of.
    pub(crate) fn path(&self) -> Path {
        if self.dictation.load(Ordering::Acquire) {
            Path::Dictation
        } else {
            Path::Uplink
        }
    }

    pub(crate) fn start_run(&self, path: Path) {
        // The time and the edge go before the floor moves, so a reader that sees this run never sees those of the one before.
        self.arrival.store(None);
        self.opened.store(None);
        self.finished.store(false, Ordering::Relaxed);
        self.dictation
            .store(path == Path::Dictation, Ordering::Release);
        self.floor
            .store(self.written.load(Ordering::Relaxed), Ordering::Release);
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// Running out of audio after the stream ended is the normal tail.
    pub(crate) fn finish(&self) {
        self.finished.store(true, Ordering::Release);
    }

    /// Notes the recorder's open edge, and gives it in ns.
    fn link(&self, now: Duration) -> i64 {
        let edge = ns_of(now);
        self.edge.store(Some(edge));
        edge
    }

    /// How far back from `now` the run's opening reaches: the stated latency before the edge of a recorder that linked, which is noted as the edge the run opened against, or before `now` when none linked earlier.
    pub(crate) fn open(&self, now: Duration) -> Duration {
        let edge = self.edge.load();
        self.opened.store(edge);
        let since = edge.map_or(0, |edge| ns_of(now).saturating_sub(edge));
        let since = Duration::from_nanos(u64::try_from(since).unwrap_or(0));
        since.saturating_add(Duration::from_millis(u64::from(NODE_LATENCY_MS)))
    }

    /// Whether the run's opening was placed against this edge.
    fn opened_against(&self, edge: Option<i64>) -> bool {
        edge.is_some() && self.opened.load() == edge
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
        let latency = match self.path() {
            Path::Uplink => NODE_LATENCY_MS,
            Path::Dictation => DOAP_LATENCY_MS,
        };
        let arrival = self.arrival.load()?;
        let latency = i64::from(latency).saturating_mul(NS_PER_MS);
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
    ring: Arc<Ring>,
}

impl Resync {
    pub fn request(&self) {
        let generation = self.ring.generation.load(Ordering::Acquire);
        self.ring
            .resync_from
            .fetch_min(generation, Ordering::Release);
    }
}

/// What a read stands by: the frame it should end on, and the run whose time said so.
#[derive(Debug, Clone, Copy)]
struct Aim {
    target: i64,
    run: u64,
    generation: u64,
    catch_up: bool,
}

/// A run the reader started on behind its target, at the run's first frame.
#[derive(Debug, Clone, Copy)]
struct Opening {
    run: u64,
    /// When it began, in ns.
    began: i64,
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
    /// The frame the next read opens on; it moves by what is read, whatever was handed out.
    next: i64,
    was: Was,
    /// Samples handed out since the last frame that was none.
    fade: u16,
    /// The run of the last sample handed out since a recorder linked.
    heard: Option<u64>,
    /// The exclusive end of the audio handed out in `heard`, even if the reader has since run dry.
    played: i64,
    /// The generation still playing when another recorder linked to this reader.
    previous_span_run: Option<u64>,
    /// What still sounds of the last two ends of audio where the reader stood, and of the audio a jump left.
    ended: Tail,
    earlier: Tail,
    left: Tail,
    /// The smoothed distance from the target; above zero the reader is ahead.
    ahead_ms: f64,
    /// A recorder linked and the reader has not been moved to a target since.
    linking: bool,
    /// The clock at the first read after a recorder linked, in ns.
    edge: Option<i64>,
    /// The run the reader is catching up on.
    opening: Option<Opening>,
    last_read: Option<Duration>,
    catch_up_left: i64,
    /// The converter applies a requested catch-up ratio to its next read.
    catch_up_requested: bool,
}

/// The catch-up rate falls by a quarter of its excess per quantum as it spends its remaining lag budget.
fn opening_ratio(behind: i64, frames: i64) -> f64 {
    let exit = i64::from(OPENING_EXIT_MS.saturating_mul(FRAMES_PER_MS));
    let distance = float_of(behind.saturating_sub(exit)) / float_of(frames.max(1));
    if distance <= 0.0 {
        return 1.0;
    }
    let max_excess = OPENING_MAX_PPM / PARTS_PER_MILLION;
    let step = max_excess / f64::from(OPENING_RAMP_QUANTA);
    let mut budget = 0.0;
    for quantum in 1..=OPENING_RAMP_QUANTA {
        let excess = step * f64::from(quantum);
        // A quantum at ratio r closes 1 - 1/r quanta of lag.
        let closure = excess / (1.0 + excess);
        if distance <= budget + closure {
            let share = (distance - budget) / closure;
            return 1.0 + step * (f64::from(quantum.saturating_sub(1)) + share);
        }
        budget += closure;
    }
    1.0 + max_excess
}

impl Reader {
    /// The reader and its resync handle.
    pub(crate) fn new(ring: Arc<Ring>, counters: Arc<Counters>) -> (Self, Resync) {
        let resync = Resync {
            ring: Arc::clone(&ring),
        };
        let reader = Self {
            ring,
            counters,
            next: 0,
            was: Was::Gone,
            fade: 0,
            heard: None,
            played: 0,
            previous_span_run: None,
            ended: Tail::NONE,
            earlier: Tail::NONE,
            left: Tail::NONE,
            ahead_ms: 0.0,
            linking: false,
            edge: None,
            opening: None,
            last_read: None,
            catch_up_left: 0,
            catch_up_requested: false,
        };
        (reader, resync)
    }

    /// Fills `out` with the frames due at `now`, at the level a recorder gets, and returns their playout ratio.
    pub fn read(&mut self, now: Duration, out: &mut [f32]) -> f64 {
        self.read_at(now, out, None)
    }

    /// Reads for a converter that applies the ratio to its next buffer, which holds at most `capacity` frames.
    pub fn read_for_converter(&mut self, now: Duration, out: &mut [f32], capacity: usize) -> f64 {
        self.read_at(now, out, Some(i64::try_from(capacity).unwrap_or(i64::MAX)))
    }

    fn read_at(&mut self, now: Duration, out: &mut [f32], capacity: Option<i64>) -> f64 {
        let frames = i64::try_from(out.len()).unwrap_or(i64::MAX);
        if std::mem::take(&mut self.catch_up_requested) {
            self.catch_up_left = self.catch_up_left.saturating_sub(frames).max(0);
        }
        let previous = self.ring.resync_from.swap(NO_RESYNC, Ordering::AcqRel);
        let resynced = previous != NO_RESYNC;
        if resynced {
            // A recorder that links hears nothing from before it, whether or not a packet of the run has landed.
            (self.ended, self.earlier, self.left) = (Tail::NONE, Tail::NONE, Tail::NONE);
            (self.heard, self.was) = (None, Was::Gone);
            (self.linking, self.opening) = (true, None);
            self.previous_span_run = self.edge.map(|_| previous);
            self.catch_up_left = 0;
            self.edge = Some(self.ring.link(now));
        }
        // A dry read can run ahead of the writer, so it only accounts for frames already written.
        let written = self.ring.end();
        // The run is noted before its time is read, so the time is of that run or of one after it.
        let run = self.ring.run();
        let aim = self.aim(run, now, frames);
        let ratio = self.fill(aim, frames, out, capacity);
        // Last, over every sample handed out, so the sum of two fades is held under the ceiling too.
        let tally = level::apply(out);
        let played = aim.map_or(0, |_| self.next.min(written));
        self.ring.note_output(played, out, resynced);
        let counters = &self.counters;
        counters.limited.fetch_add(tally.limited, Ordering::Relaxed);
        counters
            .nonfinite
            .fetch_add(tally.nonfinite, Ordering::Relaxed);
        ratio
    }

    /// Stands the reader for a read of `frames` at `now` by the time of `run`, the run noted before, and gives what it stood by; none while the run has not told its time, or when another started since.
    fn aim(&mut self, run: u64, now: Duration, frames: i64) -> Option<Aim> {
        let previous = self.last_read.replace(now);
        let generation = self.ring.generation.load(Ordering::Acquire);
        let target = self.ring.target(now)?;
        // A run that started since `run` was noted may have told the time just read, so the read stands by neither.
        if self.ring.run() != run || self.ring.generation.load(Ordering::Acquire) != generation {
            return None;
        }
        if self.heard != Some(run) {
            self.catch_up_left = 0;
        } else if let Some(last) = previous {
            let elapsed = scaled(ns_of(now.saturating_sub(last)), RATE_HZ, NS_PER_SECOND);
            let bound = i64::from(REALIGN_MS.saturating_mul(FRAMES_PER_MS));
            // Only a fresh callback gap can renew an exhausted budget while lag persists.
            if elapsed.saturating_sub(frames) > bound {
                self.catch_up_left =
                    i64::from(CATCH_UP_MAX_MS).saturating_mul(i64::from(FRAMES_PER_MS));
            }
        }
        let catch_up = self.stand(target.saturating_sub(frames), run, generation, now);
        Some(Aim {
            target,
            run,
            generation,
            catch_up,
        })
    }

    /// Hands out the frames from where the reader stands, those of the run it stood by and none of a run that started since, and gives the ratio.
    fn fill(
        &mut self,
        aim: Option<Aim>,
        frames: i64,
        out: &mut [f32],
        capacity: Option<i64>,
    ) -> f64 {
        let told = aim.map(|aim| aim.run);
        let mut gone = 0_u64;
        for sample in &mut *out {
            *sample = self.frame(told, &mut gone);
        }
        if gone > 0 {
            self.counters
                .span_gone_frames
                .fetch_add(gone, Ordering::Relaxed);
        }
        if let (Some(previous), Some(aim)) = (self.previous_span_run, aim)
            && previous != aim.generation
            && self.heard == Some(aim.run)
        {
            self.previous_span_run = None;
        }
        match (aim, self.opening) {
            (None, _) => 1.0,
            (Some(aim), Some(_)) => opening_ratio(aim.target.saturating_sub(self.next), frames),
            (Some(aim), None) => {
                let catch_up =
                    aim.catch_up && self.was == Was::Sample && self.heard == Some(aim.run);
                self.steer(aim.target, frames, catch_up, capacity)
            }
        }
    }

    /// Positions the read and indicates when retained audio needs to catch up by rate.
    fn stand(&mut self, place: i64, run: u64, generation: u64, now: Duration) -> bool {
        let bound = i64::from(REALIGN_MS.saturating_mul(FRAMES_PER_MS));
        let first = i64::try_from(run).unwrap_or(i64::MAX);
        // Only a recorder that has heard nothing since it linked still waits for what came before its edge.
        let unheard = self
            .previous_span_run
            .map_or(self.heard.is_none(), |previous| previous != generation);
        if unheard && first < place && self.ring.opened_against(self.edge) {
            self.jump(first);
            let behind = scaled(place.saturating_sub(first), 1, u32::from(FRAMES_PER_MS));
            self.counters.opening_ms.store(0, Ordering::Relaxed);
            self.counters
                .opening_behind_ms
                .store(u32::try_from(behind).unwrap_or(u32::MAX), Ordering::Relaxed);
            self.opening = Some(Opening {
                run,
                began: ns_of(now),
            });
        }
        if let Some(opening) = self.opening {
            let same_run = run == opening.run;
            let exit = i64::from(OPENING_EXIT_MS.saturating_mul(FRAMES_PER_MS));
            if same_run && place.saturating_sub(self.next) > exit {
                return false;
            }
            if same_run {
                self.caught_up(opening, now);
            }
            self.opening = None;
        }
        let off = self.next.saturating_sub(place);
        // A realign waits out the fall of the one before it, which a second would cut short.
        if self.linking {
            self.jump(place);
        } else if off.unsigned_abs() > bound.unsigned_abs() && !self.left.falling() {
            // Keep unread speech in the current run even when the callback is late.
            if off < 0 && self.heard == Some(run) {
                let oldest = self.ring.oldest().max(first).min(place);
                if oldest > self.next {
                    let gone = u64::try_from(oldest.saturating_sub(self.next)).unwrap_or(0);
                    self.counters
                        .span_gone_frames
                        .fetch_add(gone, Ordering::Relaxed);
                    self.jump(oldest);
                } else if let Some(to) = self.quiet_destination(place, run) {
                    self.jump(to);
                } else {
                    return true;
                }
                return false;
            }
            // A later dictation anchor can reach audio that has already been handed out.
            let place = if off > 0 && self.heard == Some(run) && self.ring.path() == Path::Dictation
            {
                place.max(self.played)
            } else {
                place
            };
            if place != self.next {
                self.jump(place);
            }
        }
        false
    }

    /// Only cut sustained quiet, leaving quiet under both sides of the realignment fade.
    fn quiet_destination(&self, place: i64, run: u64) -> Option<i64> {
        let fade = i64::from(FADE_FRAMES);
        let start = self.next.saturating_sub(fade);
        let end = place.saturating_add(fade);
        let quiet_end = (start..end)
            .find(|&at| {
                !matches!(self.ring.frame_of(run, at), Frame::Sample { sample, .. } if sample.abs() < QUIET_PEAK)
            })
            .unwrap_or(end);
        let to = quiet_end.saturating_sub(fade);
        (to.saturating_sub(self.next) >= fade && self.ring.run() == run).then_some(to)
    }

    /// Records the time to reach the opening's exit bound.
    fn caught_up(&self, opening: Opening, now: Duration) {
        let took_ms = ns_of(now).saturating_sub(opening.began) / NS_PER_MS;
        self.counters.opening_ms.store(
            u32::try_from(took_ms).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
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

    /// The exclusive ends of the last read's whole request and of the audio handed out so far.
    #[must_use]
    pub fn read_ends(&self) -> (i64, i64) {
        (self.next, self.played)
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

    /// The next frame; only a sample of `told`, the run whose time the read stood by, has a place and is handed out.
    fn frame(&mut self, told: Option<u64>, gone: &mut u64) -> f32 {
        let at = self.next;
        self.next = at.saturating_add(1);
        let mut out = match self.ring.frame(at) {
            Frame::Sample { sample, run } if told == Some(run) => {
                if self.was != Was::Sample {
                    self.fade = 0;
                }
                let gain = fade_in(self.fade);
                self.fade = self.fade.saturating_add(1);
                (self.was, self.heard) = (Was::Sample, Some(run));
                self.played = self.next;
                sample * gain
            }
            Frame::Dry { run } => {
                if self.was != Was::Dry
                    && self.heard == Some(run)
                    && !self.ring.finished.load(Ordering::Acquire)
                {
                    self.counters.underruns.fetch_add(1, Ordering::Relaxed);
                }
                self.end_audio(at);
                self.was = Was::Dry;
                0.0
            }
            frame @ (Frame::Sample { .. } | Frame::Gone) => {
                if matches!(frame, Frame::Gone) {
                    *gone = gone.saturating_add(1);
                }
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
    fn steer(&mut self, target: i64, frames: i64, catch_up: bool, capacity: Option<i64>) -> f64 {
        let per_ms = f64::from(FRAMES_PER_MS);
        let ahead_ms = float_of(self.next.saturating_sub(target)) / per_ms;
        let smooth_frames = f64::from(STEER_SMOOTH_MS) * per_ms;
        let share = 1.0 - (-float_of(frames) / smooth_frames).exp();
        self.ahead_ms += (ahead_ms - self.ahead_ms) * share;
        let ppm = (self.ahead_ms * STEER_PPM_PER_MS).clamp(-STEER_MAX_PPM, STEER_MAX_PPM);
        let drift = 1.0 - ppm / PARTS_PER_MILLION;
        // The next converter request may be larger than this read, up to the buffer capacity.
        let reserved = capacity.unwrap_or(frames);
        if catch_up
            && ahead_ms < -f64::from(REALIGN_MS)
            && (1..=self.catch_up_left).contains(&reserved)
        {
            let excess = -ahead_ms - f64::from(REALIGN_MS);
            let share = (excess / CATCH_UP_TAPER_MS).min(1.0);
            let ratio = 1.0 + (CATCH_UP_RATIO - 1.0) * share;
            if ratio > drift {
                if capacity.is_some() {
                    self.catch_up_requested = true;
                } else {
                    self.catch_up_left = self.catch_up_left.saturating_sub(frames);
                }
                return ratio;
            }
        }
        drift
    }
}

#[cfg(test)]
mod tests;
