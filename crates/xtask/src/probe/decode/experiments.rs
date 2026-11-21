//! The five fixed experiments of a decode: two other configurations, a damaged stream as a control, a splice, and mutated units.

use super::super::analyse::report::Row;
use super::super::store::Run;
use super::super::xorshift::next_random;
use super::candidate::{Candidate, SLOW_CALL, Tally};
use super::compare::{TOLERANCE_LSB, frame_apart, lsb_apart};
use super::oracle::OracleError;
use super::{
    Both, EXPECTED_CONFIG, FRAMES, LSB, REFUSED, SAMPLES_PER_UNIT, UNITS, VERDICT, figure, lsb,
    one_line,
};

/// The same stream declared with 512 sample frames: the frame length flag of the expected configuration cleared.
const LONG_FRAME_CONFIG: [u8; 4] = [0xf8, 0xe6, 0x20, 0x00];
const LONG_FRAME_SAMPLES: u16 = 512;
/// The same stream declared at 64 kHz, sampling index 2, for which no 480 sample band table exists.
const TRUE_RATE_CONFIG: [u8; 4] = [0xf8, 0xe4, 0x30, 0x00];
/// The unit a splice takes out, counted from 0 as the units table counts.
const SPLICED_UNIT: usize = 1000;
/// The mutated units the candidate is fed.
const MUTATIONS: usize = 10_000;
/// The seed of the mutations, so two runs over one log feed the same bytes.
const MUTATION_SEED: u64 = 0x5eed_0007;
const BITS_PER_BYTE: usize = 8;
/// Why a figure of the whole decode against another decode is refused.
const NOT_FRAME_ON_FRAME: &str =
    "one of the two decodes is not whole, or they do not fit frame on frame";

/// What taking one unit out did to a decode.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Splice {
    /// Frames from the splice on that differ from the same frames of the whole decode by more than 1 LSB.
    frames_changed: usize,
    /// The largest step between two neighbouring samples in the first frame after the splice, the step into it included.
    step_lsb: f64,
}

/// The frames of two decodes of one length that differ by more than 1 LSB; none when the two cannot be laid frame on frame.
fn frames_changed(full: &[f32], other: &[f32], samples_per_unit: usize) -> Option<usize> {
    let whole = full.len().checked_rem(samples_per_unit) == Some(0);
    if full.is_empty() || full.len() != other.len() || !whole {
        return None;
    }
    let frames = full
        .chunks(samples_per_unit)
        .zip(other.chunks(samples_per_unit));
    Some(
        frames
            .filter(|(a, b)| frame_apart(a, b) > TOLERANCE_LSB)
            .count(),
    )
}

/// Measures a decode with unit `at` taken out against the decode of every unit; none unless the first is exactly one frame shorter and a frame follows the splice.
fn splice(full: &[f32], spliced: &[f32], at: usize, samples_per_unit: usize) -> Option<Splice> {
    if spliced.len().checked_add(samples_per_unit)? != full.len() {
        return None;
    }
    let from = at.checked_mul(samples_per_unit)?;
    let after_in_full = full.get(from.checked_add(samples_per_unit)?..)?;
    let frames_changed = frames_changed(after_in_full, spliced.get(from..)?, samples_per_unit)?;
    // One sample before the frame, so the first pair is the step into it.
    let around = spliced.get(from.checked_sub(1)?..from.checked_add(samples_per_unit)?)?;
    let steps = around.windows(2).filter_map(|pair| match pair {
        &[before, after] => Some(lsb_apart(after, before)),
        _ => None,
    });
    Some(Splice {
        frames_changed,
        step_lsb: steps.fold(0.0, f64::max),
    })
}

/// Whether one decoder told the damaged stream from the sound one; none when it gave no whole decode of the sound one to tell it from.
fn told_apart(
    full: Option<&[f32]>,
    control: Option<&[f32]>,
    samples_per_unit: usize,
) -> Option<bool> {
    let full = full.filter(|full| !full.is_empty())?;
    let Some(control) = control else {
        return Some(true);
    };
    Some(control.len() != full.len() || frames_changed(full, control, samples_per_unit)? > 0)
}

/// What the control says of the comparison: `yes` when a decoder told the streams apart, `no` when none that could did, `refused` when none could.
fn discriminates(told: [Option<bool>; 2]) -> &'static str {
    match told {
        [Some(true), _] | [_, Some(true)] => "yes",
        [None, None] => REFUSED,
        _ => "no",
    }
}

/// The units without unit `at`; none when the stream has no unit `at` with a unit after it.
fn without_unit<'unit>(units: &[&'unit [u8]], at: usize) -> Option<Vec<&'unit [u8]>> {
    let after = units.get(at.checked_add(1)?..)?;
    if after.is_empty() {
        return None;
    }
    Some([units.get(..at)?, after].concat())
}

/// A draw from 0 up to `limit`, which is left out; none when `limit` is 0.
fn below(random: &mut u64, limit: usize) -> Option<usize> {
    let limit = u64::try_from(limit).ok()?;
    usize::try_from(next_random(random).checked_rem(limit)?).ok()
}

/// One unit damaged: half the time one bit of it flipped, else cut at a length shorter than its own; a unit of no bytes stays as it is.
fn mutated(unit: &[u8], random: &mut u64) -> Vec<u8> {
    if below(random, 2) == Some(0) {
        let mut flipped = unit.to_vec();
        let bit = below(random, unit.len().saturating_mul(BITS_PER_BYTE));
        let place = bit.and_then(|bit| {
            let in_byte = u32::try_from(bit.checked_rem(BITS_PER_BYTE)?).ok()?;
            Some((bit.checked_div(BITS_PER_BYTE)?, in_byte))
        });
        if let Some((byte, in_byte)) = place
            && let Some(byte) = flipped.get_mut(byte)
        {
            *byte ^= 1_u8.rotate_left(in_byte);
        }
        return flipped;
    }
    let kept = below(random, unit.len()).unwrap_or(0);
    unit.get(..kept).unwrap_or(unit).to_vec()
}

/// Feeds the candidate `count` seeded mutations of units drawn from `units`.
fn mutations(candidate: &mut Candidate, units: &[&[u8]], count: usize) -> Tally {
    let mut random = MUTATION_SEED;
    let mut draw = || {
        let unit = units.get(below(&mut random, units.len())?)?;
        Some(mutated(unit, &mut random))
    };
    let damaged: Vec<Vec<u8>> = (0..count).filter_map(|_| draw()).collect();
    let damaged: Vec<&[u8]> = damaged.iter().map(Vec::as_slice).collect();
    Tally::of(candidate, &damaged, usize::from(SAMPLES_PER_UNIT))
}

/// Every unit without its first byte: a decode of it that shows no error and no changed frame means the comparison cannot tell a damaged stream from a sound one.
fn control_rows(run: &Run, units: &[&[u8]], full: &Both<'_>) -> Result<Vec<Row>, OracleError> {
    let prefix = "first_byte_removed_";
    let cut: Vec<&[u8]> = units
        .iter()
        .map(|unit| unit.get(1..).unwrap_or(&[]))
        .collect();
    let control = Both::of(run, EXPECTED_CONFIG, &cut, SAMPLES_PER_UNIT)?;
    let frame = usize::from(SAMPLES_PER_UNIT);
    let (full_candidate, full_oracle) = (full.whole_candidate(), full.whole_oracle());
    let (cut_candidate, cut_oracle) = (control.whole_candidate(), control.whole_oracle());
    let verdict = discriminates([
        told_apart(full_candidate, cut_candidate, frame),
        told_apart(full_oracle, cut_oracle, frame),
    ]);
    let changed = |full: Option<&[f32]>, cut: Option<&[f32]>| frames_changed(full?, cut?, frame);
    let note = format!("frames more than {TOLERANCE_LSB} LSB from the decode of the whole units");
    let mut rows = control.rows(prefix);
    rows.extend([
        figure(
            &format!("{prefix}candidate_frames_changed"),
            FRAMES,
            changed(full_candidate, cut_candidate),
            &note,
            NOT_FRAME_ON_FRAME,
        ),
        figure(
            &format!("{prefix}oracle_frames_changed"),
            FRAMES,
            changed(full_oracle, cut_oracle),
            &note,
            NOT_FRAME_ON_FRAME,
        ),
        Row::new(
            &format!("{prefix}discriminates"),
            &verdict,
            VERDICT,
            "yes when a decoder that decoded the whole units failed on the damaged ones or gave other frames; no means the comparison above proves nothing on these units; refused when neither decoded the whole units",
        ),
    ]);
    Ok(rows)
}

fn splice_rows(run: &Run, units: &[&[u8]], full: &Both<'_>) -> Result<Vec<Row>, OracleError> {
    let prefix = "splice_";
    let Some(spliced) = without_unit(units, SPLICED_UNIT) else {
        let note = format!("the stream has no unit {SPLICED_UNIT} with a unit after it");
        return Ok(vec![Row::new(
            &format!("{prefix}unit"),
            &REFUSED,
            UNITS,
            &note,
        )]);
    };
    let cut = Both::of(run, EXPECTED_CONFIG, &spliced, SAMPLES_PER_UNIT)?;
    let frame = usize::from(SAMPLES_PER_UNIT);
    let changed = format!(
        "frames from the splice on more than {TOLERANCE_LSB} LSB from the same frames of the whole decode"
    );
    let measured =
        |full: Option<&[f32]>, cut: Option<&[f32]>| splice(full?, cut?, SPLICED_UNIT, frame);
    let mut rows = vec![Row::new(
        &format!("{prefix}unit"),
        &SPLICED_UNIT,
        UNITS,
        "the unit taken out, counted from 0",
    )];
    rows.extend(cut.rows(prefix));
    for (decoder, found) in [
        (
            "candidate",
            measured(full.whole_candidate(), cut.whole_candidate()),
        ),
        ("oracle", measured(full.whole_oracle(), cut.whole_oracle())),
    ] {
        rows.extend([
            figure(
                &format!("{prefix}{decoder}_frames_changed"),
                FRAMES,
                found.map(|splice| splice.frames_changed),
                &changed,
                NOT_FRAME_ON_FRAME,
            ),
            figure(
                &format!("{prefix}{decoder}_step_lsb"),
                LSB,
                found.map(|splice| lsb(splice.step_lsb)),
                "largest step between neighbouring samples in the first frame after the splice, the step into it included",
                NOT_FRAME_ON_FRAME,
            ),
        ]);
    }
    Ok(rows)
}

fn mutation_rows(units: &[&[u8]]) -> Vec<Row> {
    let mut candidate = match Candidate::new(EXPECTED_CONFIG) {
        Ok(candidate) => candidate,
        Err(reason) => return vec![Row::new("mutations", &REFUSED, UNITS, &one_line(&reason))],
    };
    let tally = mutations(&mut candidate, units, MUTATIONS);
    let fed = tally
        .decoded
        .saturating_add(tally.errors)
        .saturating_add(tally.panics);
    let slow_ms = SLOW_CALL.as_millis();
    let how = format!(
        "units drawn from the stream by a generator seeded {MUTATION_SEED:#x}, each with one bit flipped or cut short; fed to the candidate only"
    );
    vec![
        Row::new("mutations", &fed, UNITS, &how),
        Row::new("mutations_decoded", &tally.decoded, UNITS, "gave samples"),
        Row::new(
            "mutations_errors",
            &tally.errors,
            UNITS,
            "refused by the decoder",
        ),
        Row::new(
            "mutations_panics",
            &tally.panics,
            UNITS,
            "calls that panicked and were caught",
        ),
        Row::new(
            &format!("mutations_over_{slow_ms}_ms"),
            &tally.slow,
            UNITS,
            &format!("calls that took longer than {slow_ms} ms"),
        ),
    ]
}

/// The rows of every experiment, in their fixed order.
pub(crate) fn rows(run: &Run, units: &[&[u8]], full: &Both<'_>) -> Result<Vec<Row>, OracleError> {
    let mut rows = Both::of(run, LONG_FRAME_CONFIG, units, LONG_FRAME_SAMPLES)?.rows("config_512_");
    rows.extend(Both::of(run, TRUE_RATE_CONFIG, units, SAMPLES_PER_UNIT)?.rows("config_64k_"));
    rows.extend(control_rows(run, units, full)?);
    rows.extend(splice_rows(run, units, full)?);
    rows.extend(mutation_rows(units));
    Ok(rows)
}

#[cfg(test)]
mod tests;
