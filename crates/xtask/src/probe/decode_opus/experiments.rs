//! The four fixed experiments of a dictation decode: a damaged stream as a control, a concealed frame, mutated frames, and the flag and level bytes against the loudness of their frames.

use std::collections::BTreeMap;

use super::candidate::{Candidate, FrameResult};
use super::frames::Logged;
use super::{COMPARED, decode, whole};
use crate::probe::analyse::report::Row;
use crate::probe::analyse::stats::{count, share};
use crate::probe::decode::candidate::Tally;
use crate::probe::decode::compare::{TOLERANCE_LSB, compare, frame_apart};
use crate::probe::decode::experiments::{below, cut_short, damage_rows, discriminates, flipped};
use crate::probe::decode::{FRAMES, REFUSED, VERDICT, figure, tally_rows};
use crate::probe::xorshift::next_random;

/// The frame that is withheld and concealed, counted from 0 as the frames table counts.
const CONCEALED_FRAME: usize = 500;
/// The mutated frames the candidate is fed.
const MUTATIONS: usize = 10_000;
/// The seed of the mutations, so two runs over one log feed the same bytes.
const MUTATION_SEED: u64 = 0x5eed_0017;
/// The ways a frame is damaged, each as likely as the others.
const DAMAGES: usize = 3;
/// A frame is loud when its level is within this of the loudest frame of its session.
const LOUD_WITHIN_DB: f64 = 20.0;
/// A level in decibels is this times the logarithm of a mean square.
const DB_PER_DECADE_OF_POWER: f64 = 10.0;
/// One second of the stream: fewer frames give no share and no correlation worth a row.
const FEWEST_FRAMES: usize = 50;
const SHARE: &str = "share";

/// Why the candidate's whole decode is not there to measure against.
const NOT_WHOLE: &str = "the candidate did not decode every frame";

/// A frame without its second byte; a frame of one byte or none stays as it is.
fn without_second_byte(frame: &[u8]) -> Vec<u8> {
    match frame {
        [first, _, rest @ ..] => [&[*first], rest].concat(),
        short => short.to_vec(),
    }
}

/// Every frame without its second byte: a decode of them with no error and no frame over the gate means the comparison cannot tell a damaged stream from a sound one.
fn control_rows(frames: &[Logged<'_>], full: Option<&[f32]>) -> Vec<Row> {
    let prefix = "second_byte_removed_";
    let cut: Vec<Option<Vec<u8>>> = frames
        .iter()
        .map(|frame| frame.coded.map(without_second_byte))
        .collect();
    let cut: Vec<Option<&[u8]>> = cut.iter().map(Option::as_deref).collect();
    let control = decode(COMPARED, &cut);
    let damaged = whole(&control, frames.len());
    let mut rows = tally_rows(prefix, &control, FRAMES).to_vec();
    rows.extend(verdict_rows(
        prefix,
        full.zip(damaged),
        usize::from(COMPARED.frame()),
    ));
    rows
}

/// What the control found, from the whole decode and the decode of the damaged frames; none is a decode that is not whole.
pub(super) fn verdict_rows(prefix: &str, laid: Option<(&[f32], &[f32])>, frame: usize) -> [Row; 2] {
    let over =
        laid.and_then(|(full, damaged)| Some(compare(full, damaged, frame).ok()?.frames_over_gate));
    let verdict = discriminates([over.map(|frames| frames > 0), None]);
    [
        figure(
            &format!("{prefix}candidate_frames_over_gate"),
            FRAMES,
            over,
            "frames the same gate puts outside the decode of the whole frames",
            "one of the two decodes is not whole, or they are too short to compare",
        ),
        Row::new(
            &format!("{prefix}discriminates"),
            &verdict,
            VERDICT,
            "yes when the gate puts a damaged frame outside; no means the comparisons above prove nothing on these frames; refused when one of the two decodes is not whole",
        ),
    ]
}

/// What withholding one frame did to the candidate's decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Concealment {
    /// The samples the candidate gave in place of the frame.
    samples: usize,
    /// How many frames after it the last one more than 1 LSB from the whole decode comes; 0 when none is.
    apart_for: usize,
    /// The frames that follow the concealed one.
    after: usize,
}

/// Decodes the stream again with `CONCEALED_FRAME` concealed and lays what follows on the whole decode `full`.
fn concealment(frames: &[Logged<'_>], full: &[f32]) -> Result<Concealment, String> {
    let frame = usize::from(COMPARED.frame());
    let mut candidate = Candidate::new(COMPARED);
    let mut found = Concealment {
        samples: 0,
        apart_for: 0,
        after: 0,
    };
    for (index, logged) in frames.iter().enumerate() {
        let result = match logged.coded {
            Some(bytes) if index != CONCEALED_FRAME => candidate.frame(bytes),
            _ => candidate.conceal(),
        };
        let Some(after) = index.checked_sub(CONCEALED_FRAME) else {
            continue;
        };
        let samples = match result {
            FrameResult::Samples(samples) => samples,
            FrameResult::Error(error) => return Err(format!("frame {index}: {error}")),
            FrameResult::Panic => return Err(format!("frame {index}: the decoder panicked")),
        };
        if after == 0 {
            found.samples = samples.len();
            continue;
        }
        let from = index.saturating_mul(frame);
        let same = full.get(from..from.saturating_add(frame));
        let near = same.is_some_and(|same| {
            samples.len() == frame && frame_apart(&samples, same) <= TOLERANCE_LSB
        });
        found.after = after;
        if !near {
            found.apart_for = after;
        }
    }
    Ok(found)
}

fn concealment_rows(frames: &[Logged<'_>], full: Option<&[f32]>) -> Vec<Row> {
    let prefix = "concealed_";
    let refused = |why: &str| vec![Row::new(&format!("{prefix}frame"), &REFUSED, FRAMES, why)];
    if frames.len() <= CONCEALED_FRAME {
        return refused(&format!("the stream has no frame {CONCEALED_FRAME}"));
    }
    // A frame nobody decodes is concealed in the whole decode too: withholding it changes nothing and would read as a recovery.
    if frames
        .get(CONCEALED_FRAME)
        .is_some_and(|frame| frame.coded.is_none())
    {
        let why = format!("frame {CONCEALED_FRAME} is not b8, so the whole decode conceals it too");
        return refused(&why);
    }
    let found = match full.ok_or(NOT_WHOLE.to_owned()) {
        Ok(full) => concealment(frames, full),
        Err(why) => Err(why),
    };
    let found = match found {
        Ok(found) => found,
        Err(why) => return refused(&why),
    };
    // A decode still apart at its last frame has not shown that it comes back, and one with no frame after has none to show it in.
    let back = (found.apart_for < found.after).then_some(found.apart_for);
    let never = match found.after {
        0 => format!("no frame follows frame {CONCEALED_FRAME}"),
        after => {
            format!("still more than {TOLERANCE_LSB} LSB apart at the last frame, {after} after it")
        }
    };
    vec![
        Row::new(
            &format!("{prefix}frame"),
            &CONCEALED_FRAME,
            FRAMES,
            "the frame withheld from the candidate and concealed, counted from 0",
        ),
        Row::new(
            &format!("{prefix}samples"),
            &found.samples,
            "samples",
            "what the candidate gave in its place",
        ),
        figure(
            &format!("{prefix}frames_to_recover"),
            FRAMES,
            back,
            &format!(
                "frames after it up to the last one more than {TOLERANCE_LSB} LSB from the whole decode; 0 when none is"
            ),
            &never,
        ),
    ]
}

/// One frame damaged: one bit of it flipped, cut short, or its first byte replaced.
fn mutated(frame: &[u8], random: &mut u64) -> Vec<u8> {
    match below(random, DAMAGES) {
        Some(0) => flipped(frame, random),
        Some(1) => cut_short(frame, random),
        _ => {
            let mut replaced = frame.to_vec();
            let [byte, ..] = next_random(random).to_le_bytes();
            if let Some(first) = replaced.first_mut() {
                *first = byte;
            }
            replaced
        }
    }
}

fn mutation_rows(frames: &[Logged<'_>]) -> Vec<Row> {
    let real: Vec<&[u8]> = frames.iter().filter_map(|frame| frame.coded).collect();
    let mut random = MUTATION_SEED;
    let mut candidate = Candidate::new(COMPARED);
    let mut tally = Tally::default();
    for _ in 0..MUTATIONS {
        let drawn = below(&mut random, real.len()).and_then(|index| real.get(index));
        let Some(frame) = drawn else {
            break;
        };
        let damaged = mutated(frame, &mut random);
        // A first byte that is another one names another length, so any count of samples is a decode.
        tally.call(None, || candidate.frame(&damaged));
        tally.samples.clear();
    }
    let how = format!(
        "frames drawn from the stream by a generator seeded {MUTATION_SEED:#x}, each with one bit flipped, cut short or its first byte replaced; fed to the candidate only"
    );
    damage_rows(&tally, &how, FRAMES)
}

/// The level of one frame of samples in decibels; none for a frame of silence, which has no level.
fn level_db(samples: &[f32]) -> Option<f64> {
    let squares = samples
        .iter()
        .map(|&sample| f64::from(sample) * f64::from(sample));
    let mean = squares.sum::<f64>() / count(samples.len());
    (mean > 0.0).then(|| DB_PER_DECADE_OF_POWER * mean.log10())
}

/// How far two columns move together, from -1 to 1; none when one of them never changes.
fn correlation(points: &[(f64, f64)]) -> Option<f64> {
    let points_count = count(points.len());
    let mean_x = points.iter().map(|(x, _)| x).sum::<f64>() / points_count;
    let mean_y = points.iter().map(|(_, y)| y).sum::<f64>() / points_count;
    let moved = |of: &dyn Fn(f64, f64) -> f64| -> f64 {
        let products = points.iter().map(|(x, y)| of(x - mean_x, y - mean_y));
        products.sum()
    };
    let spread = (moved(&|x, _| x * x) * moved(&|_, y| y * y)).sqrt();
    (spread > 0.0).then(|| moved(&|x, y| x * y) / spread)
}

/// One decoded frame as the flag and level question reads it.
struct Heard {
    session: Option<u32>,
    flagged: bool,
    level: u8,
    /// None for a frame that decoded to silence.
    db: Option<f64>,
}

/// Whether the flag byte follows the loud frames and the level byte follows the loudness, which is what they are guessed to mean.
fn flag_rows(frames: &[Logged<'_>], full: Option<&[f32]>) -> Vec<Row> {
    let keys @ [loud_key, rest_key, level_key] = [
        "flagged_share_loud",
        "flagged_share_rest",
        "level_rms_correlation",
    ];
    let refused = |why: &str| keys.map(|key| Row::new(key, &REFUSED, "", why)).to_vec();
    let Some(full) = full else {
        return refused(NOT_WHOLE);
    };
    let decoded = frames
        .iter()
        .zip(full.chunks(usize::from(COMPARED.frame())));
    let heard: Vec<Heard> = decoded
        .filter(|(frame, _)| frame.coded.is_some())
        .map(|(frame, samples)| Heard {
            session: frame.session,
            flagged: frame.flag != 0,
            level: frame.level,
            db: level_db(samples),
        })
        .collect();
    if heard.len() < FEWEST_FRAMES {
        let thin = format!("{} decoded frames; needs {FEWEST_FRAMES}", heard.len());
        return refused(&thin);
    }
    let mut loudest: BTreeMap<Option<u32>, f64> = BTreeMap::new();
    for (frame, db) in heard.iter().filter_map(|frame| Some((frame, frame.db?))) {
        let top = loudest.entry(frame.session).or_insert(db);
        *top = top.max(db);
    }
    let loud = |frame: &&Heard| {
        let top = loudest.get(&frame.session);
        frame
            .db
            .zip(top)
            .is_some_and(|(db, top)| db >= top - LOUD_WITHIN_DB)
    };
    let (loud, rest): (Vec<&Heard>, Vec<&Heard>) = heard.iter().partition(loud);
    let share_row = |key: &str, group: &[&Heard], which: &str| {
        let flagged = group.iter().filter(|frame| frame.flagged).count();
        let note = format!(
            "{flagged} of {} frames {which} carry a flag byte that is not 00",
            group.len()
        );
        let found = share(flagged, group.len()).map(|share| format!("{share:.3}"));
        figure(key, SHARE, found, &note, &note)
    };
    let points: Vec<(f64, f64)> = heard
        .iter()
        .filter_map(|frame| Some((f64::from(frame.level), frame.db?)))
        .collect();
    // Fewer points lie near a line whatever the bytes mean: two of them always read 1 or -1.
    let level = (points.len() >= FEWEST_FRAMES).then(|| correlation(&points));
    let no_level = match level {
        None => format!(
            "{} frames that are not silence; needs {FEWEST_FRAMES}",
            points.len()
        ),
        Some(_) => "the level byte or the level of the frames never changes".to_owned(),
    };
    let within = format!("within {LOUD_WITHIN_DB} dB of the loudest frame of their session");
    vec![
        share_row(loud_key, &loud, &within),
        share_row(rest_key, &rest, &format!("not {within}")),
        figure(
            level_key,
            "",
            level.flatten().map(|found| format!("{found:.3}")),
            &format!(
                "the level byte against the level of its decoded frame in dB, over {} frames that are not silence",
                points.len()
            ),
            &no_level,
        ),
    ]
}

/// The rows of every experiment, in their fixed order; `full` is the candidate's decode when it is whole.
pub(crate) fn rows(frames: &[Logged<'_>], full: Option<&[f32]>) -> Vec<Row> {
    let mut rows = control_rows(frames, full);
    rows.extend(concealment_rows(frames, full));
    rows.extend(mutation_rows(frames));
    rows.extend(flag_rows(frames, full));
    rows
}
