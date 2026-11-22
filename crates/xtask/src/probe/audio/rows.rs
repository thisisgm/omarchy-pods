//! The rows of the acoustic table: what each cue window and each start holds, and why a figure is missing where it is.

mod against;
mod whole;

use std::fmt::Display;

use super::super::analyse::load::Window;
use super::super::analyse::report::{NONE, Row, marks_cut_row};
use super::super::script::Asked;
use super::super::wav::Wav;
use super::levels::{
    NoLevel, active_note, audible_note, first_audible, floor_dbfs, floor_note, is_digital_silence,
    level, near_zero_share, samples_in, seconds_of, speech,
};
use super::onsets::{RISE_OVER_MS, taps, taps_note};
use super::reference::Reference;
use super::spectrum::{cutoff, cutoff_note, grid, reach, reach_note};
use super::timeline::{CueWindow, Timeline};
pub(crate) use whole::whole_rows;

const DBFS: &str = "dBFS";
const DB: &str = "dB";
const HZ: &str = "Hz";
const MS: &str = "ms";
const SAMPLES: &str = "samples";
const MS_PER_SECOND: f64 = 1000.0;
const ANSWER: &str = "answer";
/// What every figure that leans on the reference recorder's clock says in its note.
const RECORDER_CLOCK: &str = "recorder clock";
/// The stretch after an onset the size of a tap is read from.
const TAP_MS: u32 = 10;
const NO_SAMPLE: &str = "the window holds no sample";
const NO_SILENT_WINDOW: &str = "the run has no silent window to compare with";

/// A figure, or why there is none.
type Found<T> = Result<T, String>;

#[derive(Debug)]
pub(crate) struct Recording<'a> {
    pub(crate) stream: &'a Wav,
    pub(crate) timeline: &'a Timeline,
    pub(crate) cues: Vec<CueWindow>,
    pub(crate) starts: Vec<Window>,
    /// Whether the marks file ended in a line without its newline, which was left out.
    pub(crate) marks_cut: bool,
    /// Whether the run followed the script whose starts fall inside speech.
    pub(crate) firstword: bool,
    pub(crate) reference: Option<Reference<'a>>,
}

impl<'a> Recording<'a> {
    fn stretch(&self, window: Window) -> (usize, &'a [f32]) {
        let range = self.timeline.stretch(window);
        (range.start, self.stream.samples.get(range).unwrap_or(&[]))
    }
}

/// `value` with `decimals` places; one that rounds to zero carries no sign.
fn rounded(value: f64, decimals: usize) -> String {
    let text = format!("{value:.decimals$}");
    match text.strip_prefix('-') {
        Some(digits) if digits.bytes().all(|digit| matches!(digit, b'0' | b'.')) => {
            digits.to_owned()
        }
        _ => text,
    }
}

/// A figure with `decimals` places, or the word for none and the reason; a figure that is no finite number is none too.
fn figure(key: &str, found: Found<f64>, decimals: usize, unit: &'static str, note: &str) -> Row {
    match found {
        Ok(value) if value.is_finite() => Row::new(key, &rounded(value, decimals), unit, note),
        Ok(_) => Row::new(key, &NONE, unit, "the figure is not a finite number"),
        Err(why) => Row::new(key, &NONE, unit, &why),
    }
}

fn counted<T: Display>(key: &str, found: Found<T>, unit: &'static str, note: &str) -> Row {
    match found {
        Ok(value) => Row::new(key, &value, unit, note),
        Err(why) => Row::new(key, &NONE, unit, &why),
    }
}

fn said<T, E: Display>(found: Result<T, E>) -> Found<T> {
    found.map_err(|why| format!("the stretch has no such level: {why}"))
}

fn answer(yes: bool) -> &'static str {
    if yes { "yes" } else { "no" }
}

/// What the decoded file says of itself, which every table of it opens with.
fn stream_rows(stream: &Wav) -> [Row; 2] {
    let labelled = "the rate the decoded file is labelled with";
    [
        Row::new("stream_rate", &stream.rate, HZ, labelled),
        Row::new("stream_samples", &stream.samples.len(), SAMPLES, ""),
    ]
}

/// The whole table: the two recordings, each cue window, each start, then what only a reference gives.
pub(crate) fn rows(recording: &Recording<'_>) -> Vec<Row> {
    let stream = recording.stream;
    let mut rows = Vec::from(stream_rows(stream));
    rows.push(marks_cut_row(recording.marks_cut));
    if let Some(reference) = &recording.reference {
        let channels = format!("of {} channels, the first is read", reference.wav.channels);
        rows.extend([
            Row::new("reference_rate", &reference.wav.rate, HZ, ""),
            Row::new(
                "reference_samples",
                &reference.wav.samples.len(),
                SAMPLES,
                &channels,
            ),
        ]);
    }
    let silent = recording
        .cues
        .iter()
        .find(|cue| cue.asked == Some(Asked::Silence));
    let silent = silent.map(|cue| recording.stretch(cue.window).1);
    let mut tap_windows = Vec::new();
    for (index, cue) in recording.cues.iter().enumerate() {
        let key = format!("cue_{}", index.saturating_add(1));
        let (start, samples) = recording.stretch(cue.window);
        rows.extend(window_rows(&key, cue.asked, samples));
        match cue.asked {
            Some(Asked::Speech) => rows.extend(speech_rows(&key, recording, cue.window, silent)),
            Some(Asked::Silence) => rows.extend(silence_rows(&key, samples, stream.rate)),
            Some(Asked::Sibilant) => rows.push(cutoff_row(&key, samples, stream.rate)),
            Some(Asked::Jingle) => rows.push(reach_row(&key, samples, silent, stream.rate)),
            Some(Asked::Taps) => {
                let onsets: Vec<usize> = taps(samples, stream.rate)
                    .iter()
                    .map(|onset| onset.saturating_add(start))
                    .collect();
                rows.extend(tap_rows(&key, samples.len(), &onsets, stream));
                tap_windows.push(against::TapWindow {
                    key,
                    window: cue.window,
                    onsets,
                });
            }
            None => {}
        }
    }
    for (index, &start) in recording.starts.iter().enumerate() {
        let key = format!("start_{}", index.saturating_add(1));
        rows.push(first_audible_row(
            &key,
            recording.stretch(start).1,
            stream.rate,
        ));
    }
    if let Some(reference) = &recording.reference {
        if !tap_windows.is_empty() {
            rows.extend(against::tap_rows(recording, reference, &tap_windows));
        }
        if recording.firstword {
            rows.extend(against::head_rows(recording, reference));
        }
    }
    rows
}

/// The share of a stretch that lies near zero, which a gate that writes zeros raises.
fn near_zero_row(key: &str, samples: &[f32]) -> Row {
    let share = near_zero_share(samples).map(|share| share * 100.0);
    figure(
        &format!("{key}_near_zero"),
        share.ok_or_else(|| NO_SAMPLE.to_owned()),
        2,
        "percent",
        "samples within one 16 bit step of zero",
    )
}

fn window_rows(key: &str, asked: Option<Asked>, samples: &[f32]) -> [Row; 3] {
    let task = match asked {
        Some(Asked::Taps) => "taps",
        Some(Asked::Speech) => "speech",
        Some(Asked::Sibilant) => "sibilant",
        Some(Asked::Jingle) => "jingle",
        Some(Asked::Silence) => "silence",
        None => "other",
    };
    let cut =
        "from one second after the cue to the next cue or stop mark, by the arrival of the units";
    [
        Row::new(
            &format!("{key}_task"),
            &task,
            "name",
            "what the cue asked for",
        ),
        Row::new(&format!("{key}_samples"), &samples.len(), SAMPLES, cut),
        near_zero_row(key, samples),
    ]
}

/// The RMS over the active frames of a stretch and its floor, each as a figure, then their rows with the peak between them.
fn level_rows(key: &str, samples: &[f32], rate: u32) -> (Found<f64>, Found<f64>, [Row; 3]) {
    let ours = said(speech(samples, rate));
    let over = active_note(ours.as_ref().map_or(0, |&(_, frames)| frames));
    let rms = ours.clone().map(|(level, _)| level.rms_dbfs);
    let peak = ours.map(|(level, _)| level.peak_dbfs);
    let floor = said(floor_dbfs(samples, rate));
    let rows = [
        figure(&format!("{key}_speech_rms"), rms.clone(), 2, DBFS, &over),
        figure(&format!("{key}_speech_peak"), peak, 2, DBFS, &over),
        figure(
            &format!("{key}_floor"),
            floor.clone(),
            2,
            DBFS,
            &floor_note(),
        ),
    ];
    (rms, floor, rows)
}

fn speech_rows(
    key: &str,
    recording: &Recording<'_>,
    window: Window,
    silent: Option<&[f32]>,
) -> Vec<Row> {
    let rate = recording.stream.rate;
    let samples = recording.stretch(window).1;
    let (ours, floor, levels) = level_rows(key, samples, rate);
    let silent_floor = silent.ok_or_else(|| NO_SILENT_WINDOW.to_owned());
    let silent_floor = silent_floor.and_then(|silent| said(floor_dbfs(silent, rate)));
    let over_silent = floor.and_then(|floor| Ok(floor - silent_floor?));
    let mut rows = Vec::from(levels);
    rows.push(figure(
        &format!("{key}_floor_minus_silent"),
        over_silent,
        2,
        DB,
        "that floor less the floor of the silent window; near 0 where the pauses of speech fall to the silence",
    ));
    if let Some(reference) = &recording.reference {
        let theirs = reference
            .stretch(window)
            .ok_or_else(|| against::NOT_HELD.to_owned());
        let theirs = theirs.and_then(|(_, samples)| said(speech(samples, reference.wav.rate)));
        let theirs = theirs.map(|(level, _)| level.rms_dbfs);
        let apart = ours.and_then(|ours| Ok(ours - theirs.clone()?));
        let note = format!("active frames of the same window in the reference; {RECORDER_CLOCK}");
        rows.extend([
            figure(&format!("{key}_reference_rms"), theirs, 2, DBFS, &note),
            figure(
                &format!("{key}_pods_minus_reference"),
                apart,
                2,
                DB,
                RECORDER_CLOCK,
            ),
        ]);
    }
    rows
}

fn silence_rows(key: &str, samples: &[f32], rate: u32) -> [Row; 2] {
    let zeros = if samples.is_empty() {
        Err(NO_SAMPLE.to_owned())
    } else {
        Ok(answer(is_digital_silence(samples)))
    };
    [
        figure(
            &format!("{key}_noise_rms"),
            said(floor_dbfs(samples, rate)),
            2,
            DBFS,
            &floor_note(),
        ),
        counted(
            &format!("{key}_exact_zeros"),
            zeros,
            ANSWER,
            "whether every sample of the window is exactly zero",
        ),
    ]
}

fn cutoff_row(key: &str, samples: &[f32], rate: u32) -> Row {
    let found =
        cutoff(&grid(&[samples], rate)).map_err(|why| format!("the window has no cutoff: {why}"));
    counted(&format!("{key}_cutoff"), found, HZ, &cutoff_note())
}

fn reach_row(key: &str, samples: &[f32], silent: Option<&[f32]>, rate: u32) -> Row {
    let silent = silent.ok_or_else(|| NO_SILENT_WINDOW.to_owned());
    let found = silent.and_then(|silent| {
        reach(&grid(&[samples], rate), &grid(&[silent], rate)).map_err(|why| why.to_string())
    });
    let note = reach_note("the window", "the silent window");
    counted(&format!("{key}_reach"), found, HZ, &note)
}

fn tap_rows(key: &str, window_len: usize, onsets: &[usize], stream: &Wav) -> Vec<Row> {
    let long_enough = window_len >= samples_in(RISE_OVER_MS, stream.rate);
    let found = long_enough.then_some(onsets.len());
    let found = found.ok_or_else(|| {
        format!("the window is shorter than the {RISE_OVER_MS} ms a rise is read over")
    });
    let mut rows = vec![counted(&format!("{key}_taps"), found, "taps", &taps_note())];
    for (index, &onset) in onsets.iter().enumerate() {
        let until = onset
            .saturating_add(samples_in(TAP_MS, stream.rate))
            .min(stream.samples.len());
        let tap = stream.samples.get(onset..until).unwrap_or(&[]);
        let at_s = seconds_of(onset, stream.rate);
        rows.push(figure(
            &format!("{key}_tap_{}_peak", index.saturating_add(1)),
            said(level(tap)).map(|level| level.peak_dbfs),
            2,
            DBFS,
            &format!("the largest sample of the {TAP_MS} ms from {at_s:.3} s of the stream on"),
        ));
    }
    rows
}

fn first_audible_row(key: &str, samples: &[f32], rate: u32) -> Row {
    let found = if samples.is_empty() {
        Err("the start holds no sample".to_owned())
    } else {
        let frame = first_audible(samples, rate).ok_or_else(|| NoLevel::NotAudible.to_string());
        frame.map(|sample| seconds_of(sample, rate) * MS_PER_SECOND)
    };
    let note = audible_note(samples.len());
    figure(&format!("{key}_first_audible"), found, 1, MS, &note)
}
