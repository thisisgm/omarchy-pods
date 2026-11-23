//! What a recorder on the daemon's node heard of the fake pods: the tone the decoder makes of their units, the counters the status folds, and the rig's own timing.

use std::f64::consts::{PI, SQRT_2, TAU};
use std::ops::Range;
use std::time::Duration;

use pods_fake::uplink::{DEFAULT_LEVEL, MARK_LINE, PERIOD, TONE_LINE, Uplink};
use pods_mic::decode::{Decoded, Decoder};
use pods_mic::{RATE_HZ, SAMPLES_PER_UNIT};
use pods_pwrig::{Process, Recorder, Server, write_wav};
use pods_samples::uplink::tone_unit;
use serde_json::Value;

use super::rig::Rig;
use super::wait_for;

/// Superwhisper's recorder takes 16 kHz.
pub(crate) const RECORDER_RATE: u32 = 16_000;
/// The largest gap between two packets seen from real pods.
pub(crate) const WORST_GAP: Duration = Duration::from_millis(72);
/// A window short enough to place a marked packet within a few ms, long enough to tell two lines a kilohertz apart.
const WINDOW: Duration = Duration::from_micros(2500);
/// Units decoded for the tone's own level: far more than the decoder's start.
const OWN_UNITS: usize = 300;
/// The decoder's start, left out of its own level.
const OWN_SETTLE_UNITS: usize = 4;
const MICROS_PER_SECOND: u64 = 1_000_000;

/// The node name the stock player takes.
const PLAYER: &str = "pw-play";

/// A stock player's silence recorded beside the daemon's node: the pace of the graph itself, which a loaded runner can slow.
pub(crate) struct Pace<'server> {
    pub(crate) player: Process<'server>,
    pub(crate) recorder: Recorder<'server>,
}

/// Starts the player on `span` of silence and Superwhisper's second recorder on it.
pub(crate) fn pace<'server>(rig: &Rig, server: &'server Server, span: Duration) -> Pace<'server> {
    let dir = rig.session().dir.path();
    let silence = vec![0_i16; frames(span)];
    let file = dir.join("pace.wav");
    write_wav(&file, RECORDER_RATE, &silence);
    let recorder = Recorder::history(server, PLAYER, &dir.join("pace-recorded.wav"));
    let player = server.play(&file);
    server.link(PLAYER, recorder.node());
    Pace { player, recorder }
}

/// The span `frames` of a recorder at `rate` hold.
pub(crate) fn span_at(frames: u64, rate: u32) -> Duration {
    let micros = frames
        .saturating_mul(MICROS_PER_SECOND)
        .checked_div(u64::from(rate))
        .unwrap();
    Duration::from_micros(micros)
}

/// The counts the status folds at a phase change and when a recorder leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Counts {
    pub(crate) underruns: u64,
    pub(crate) concealed: u64,
    pub(crate) decode_errors: u64,
}

pub(crate) fn counts(status: &Value) -> Counts {
    let count = |key: &str| {
        status[key]
            .as_u64()
            .unwrap_or_else(|| panic!("{key} in {status}"))
    };
    Counts {
        underruns: count("mic_underruns"),
        concealed: count("mic_concealed"),
        decode_errors: count("mic_decode_errors"),
    }
}

/// The counts once the last recorder has left and the status folded them, printed for the run's record.
#[expect(clippy::print_stderr, reason = "the run's record of the counters")]
pub(crate) fn counts_after_leaving(rig: &Rig) -> Counts {
    let counted =
        counts(&rig.wait_status("the recorder gone", |status| status["mic"] != "recording"));
    eprintln!("the status counts after the recorder left: {counted:?}");
    counted
}

/// The pitch of steady units at `line`: one line above it, as the decoder's frame places it.
pub(crate) fn line_hz(line: u16) -> f64 {
    let lines_per_second = f64::from(RATE_HZ) / f64::from(2 * SAMPLES_PER_UNIT);
    f64::from(line.saturating_add(1)) * lines_per_second
}

/// The peak, of full scale, of the tone the decoder itself makes of the fake's units.
pub(crate) fn own_amplitude() -> f64 {
    let mut decoder = Decoder::new().unwrap();
    let unit = tone_unit(TONE_LINE, DEFAULT_LEVEL);
    let mut power = 0.0;
    let mut count = 0_u32;
    for at in 0..OWN_UNITS {
        let Decoded::Samples(frame) = decoder.unit(&unit) else {
            panic!("the decoder refused a tone unit");
        };
        if at >= OWN_SETTLE_UNITS {
            power += frame
                .iter()
                .map(|&sample| f64::from(sample).powi(2))
                .sum::<f64>();
            count = count.saturating_add(u32::from(SAMPLES_PER_UNIT));
        }
    }
    (power / f64::from(count)).sqrt() * SQRT_2
}

/// The largest step the decoder's own tone can take from one sample to the next at the recorder's rate, 1 dB louder: a step to zero is more than twice it.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a step inside full scale, rounded up to the next sample value"
)]
pub(crate) fn tone_step_bound() -> u16 {
    let one_db = 10_f64.powf(1.0 / 20.0);
    let per_sample = PI * line_hz(TONE_LINE) / f64::from(RECORDER_RATE);
    let step = 2.0 * per_sample.sin() * own_amplitude() * one_db * f64::from(i16::MAX);
    step.ceil() as u16
}

/// The frames `span` holds at the recorder's rate.
pub(crate) fn frames(span: Duration) -> usize {
    let micros = u64::try_from(span.as_micros()).unwrap();
    usize::try_from(micros.saturating_mul(u64::from(RECORDER_RATE)) / MICROS_PER_SECOND).unwrap()
}

/// Where the recording's first sample that is not zero is.
pub(crate) fn first_sound(samples: &[i16]) -> usize {
    samples
        .iter()
        .position(|&sample| sample != 0)
        .unwrap_or_else(|| panic!("the recording of {} samples is silent", samples.len()))
}

/// The longest run of exact zeros after the first sound and before the last.
pub(crate) fn longest_zero_run(samples: &[i16]) -> usize {
    let last = samples.iter().rposition(|&sample| sample != 0).unwrap_or(0);
    let sounding = samples.get(first_sound(samples)..=last).unwrap_or_default();
    let zeros = sounding.split(|&sample| sample != 0);
    zeros.map(<[i16]>::len).max().unwrap_or(0)
}

/// The largest step from one sample to the next in `samples`.
pub(crate) fn largest_step(samples: &[i16]) -> u16 {
    let steps = samples
        .array_windows::<2>()
        .map(|[before, after]| after.abs_diff(*before));
    steps.max().unwrap_or(0)
}

/// The windows, as ranges of samples, where the marked line is louder than the tone's and than a quarter of the tone's own amplitude, so a gap's quiet counts as neither.
pub(crate) fn marked(samples: &[i16]) -> Vec<Range<usize>> {
    let window = frames(WINDOW);
    let width = f64::from(u32::try_from(window).unwrap());
    let floor = (own_amplitude() * f64::from(i16::MAX) * width / 8.0).powi(2);
    let mut runs: Vec<Range<usize>> = Vec::new();
    for (index, chunk) in samples.chunks_exact(window).enumerate() {
        let mark = power_at(chunk, line_hz(MARK_LINE));
        if mark <= floor || mark <= power_at(chunk, line_hz(TONE_LINE)) {
            continue;
        }
        let start = index.saturating_mul(window);
        match runs.last_mut() {
            Some(run) if run.end == start => run.end = start.saturating_add(window),
            _ => runs.push(start..start.saturating_add(window)),
        }
    }
    runs
}

/// The power of `samples` at `hz`, by one Goertzel pass.
fn power_at(samples: &[i16], hz: f64) -> f64 {
    let coefficient = 2.0 * (TAU * hz / f64::from(RECORDER_RATE)).cos();
    let (mut before, mut last) = (0.0, 0.0);
    for &sample in samples {
        let next = f64::from(sample) + coefficient * last - before;
        (before, last) = (last, next);
    }
    last * last + before * before - coefficient * last * before
}

/// The span `samples` of a range hold at the recorder's rate.
pub(crate) fn span_of(samples: usize) -> Duration {
    span_at(u64::try_from(samples).unwrap(), RECORDER_RATE)
}

/// Waits for the fake to have made `count` audio packets since it was made.
pub(crate) fn wait_packets(uplink: &Uplink, count: u64) {
    wait_for(&format!("{count} audio packets from the fake"), || {
        (uplink.sent_packets() >= count).then_some(())
    });
}

/// Asks the fake for `gap` before about every `every`th packet until it made `packets` more; each ask holds the packet after the last one sent.
pub(crate) fn gap_every(uplink: &Uplink, every: u64, gap: Duration, packets: u64) {
    let end = uplink.sent_packets().saturating_add(packets);
    loop {
        let sent = uplink.sent_packets();
        if sent >= end {
            return;
        }
        uplink.stall(gap);
        wait_packets(uplink, sent.saturating_add(every));
    }
}

/// Marks the next packet the fake makes and gives its place among the packets it sent; fails with the rig's name when a packet went out while the mark was asked for.
pub(crate) fn mark_next(uplink: &Uplink) -> u64 {
    let before = uplink.sent_packets();
    uplink.mark(1);
    let after = uplink.sent_packets();
    assert_eq!(
        before, after,
        "the rig: a packet went out while the test marked the next, so the marked place is unknown"
    );
    before
}

/// Fails with the rig's name, not the daemon's, when the fake's own cadence had a gap past `limit`; prints the record either way.
#[expect(
    clippy::print_stderr,
    reason = "the run's record of the fake's own gap"
)]
pub(crate) fn assert_rig_gap(uplink: &Uplink, limit: Duration) -> Duration {
    let gap = uplink.largest_gap();
    eprintln!("the fake pods' largest gap between two packets: {gap:?}");
    assert!(
        gap <= limit,
        "the rig missed its timing: the fake pods' largest gap was {gap:?}, past {limit:?}, so this run cannot judge the daemon"
    );
    gap
}

/// The time `packets` packets of the fake's cadence stand for.
pub(crate) fn packets_span(packets: u64) -> Duration {
    PERIOD.saturating_mul(u32::try_from(packets).unwrap())
}
