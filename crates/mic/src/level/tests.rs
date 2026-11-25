//! Support tests of the curve itself, sample by sample; the reader's suites and the daemon's hold what a recorder gets of it.

use proptest::prelude::*;

use super::*;

/// How far a point of the curve may stand from where its own arithmetic puts it.
const POINT_SLACK_DB: f64 = 0.05;
const SLOPE_SLACK: f64 = 1e-3;
const FORMULA_SLACK_DB: f64 = 1e-4;
/// The floats tried on each side of an edge.
const NEIGHBOURS: usize = 2000;
/// The steps of a sweep from nothing to the knee, and of one from the knee to the largest float.
const SWEEP: u16 = 4000;
/// How far the sweep past the knee reaches: a tenth of full scale times this power of ten is still a float.
const DECADES: f32 = 38.0;

fn gain() -> f32 {
    amplitude(MIC_GAIN_DB)
}

fn knee() -> f32 {
    amplitude(LIMIT_KNEE_DBFS)
}

fn ceiling() -> f32 {
    amplitude(LIMIT_CEILING_DBFS)
}

fn levelled(sample: f32) -> (f32, Tally) {
    let mut block = [sample];
    let tally = apply(&mut block);
    (block[0], tally)
}

fn dbfs(sample: f32) -> f64 {
    f64::from(DB_PER_DECADE) * f64::from(sample.abs()).log10()
}

const LIMITED: Tally = Tally {
    limited: 1,
    nonfinite: 0,
};

/// The largest sample the gain leaves at or under the knee, and the next float, which it leaves over.
fn edge() -> (f32, f32) {
    let mut at = knee() / gain();
    while at * gain() > knee() {
        at = at.next_down();
    }
    while at.next_up() * gain() <= knee() {
        at = at.next_up();
    }
    (at, at.next_up())
}

/// `count` floats in a row, the first at `from`.
fn upward(from: f32, count: usize) -> impl Iterator<Item = f32> {
    std::iter::successors(Some(from), |at| Some(at.next_up())).take(count)
}

#[test]
fn at_and_under_the_knee_a_sample_is_the_gain_times_itself_to_the_bit() {
    let (under, _) = edge();
    let sweep = (0..=SWEEP).map(|step| under * f32::from(step) / f32::from(SWEEP));
    let near = std::iter::successors(Some(under), |at| Some(at.next_down())).take(NEIGHBOURS);
    for sample in sweep.chain(near).flat_map(|sample| [sample, -sample]) {
        let (out, tally) = levelled(sample);
        assert_eq!(out.to_bits(), (sample * gain()).to_bits(), "{sample}");
        assert_eq!(tally, Tally::default(), "{sample}");
    }
    // The sample the gain puts on the knee itself is under the rule of the samples below it.
    assert_eq!((under * gain()).to_bits(), knee().to_bits());
}

#[test]
fn the_curve_never_falls_as_the_sample_rises() {
    let (under, over) = edge();
    let start = upward(under, 1).chain(upward(over, NEIGHBOURS));
    let far =
        (1..=SWEEP).map(|step| over * 10_f32.powf(DECADES * f32::from(step) / f32::from(SWEEP)));
    let mut last = levelled(under.next_down()).0;
    for sample in start.chain(far).chain([f32::MAX]) {
        let (out, _) = levelled(sample);
        assert!(out >= last, "{sample} gives {out} after {last}");
        // The curve is the same on both sides of zero.
        assert_eq!(levelled(-sample).0.to_bits(), (-out).to_bits(), "{sample}");
        last = out;
    }
}

#[test]
fn the_curve_leaves_the_knee_in_value_and_in_slope() {
    let (under, over) = edge();
    let step = f64::from(levelled(over).0) - f64::from(levelled(under).0);
    let straight = f64::from(over * gain()) - f64::from(under * gain());
    assert!(
        (step - straight).abs() <= f64::from(f32::EPSILON),
        "a step of {step} where the line steps {straight}"
    );
    // A thousandth of the room past the knee the chord from the knee has the slope 1 / (1 + 0.001).
    let room = ceiling() - knee();
    let past = (knee() + room / 1000.0) / gain();
    let rise = f64::from(levelled(past).0) - f64::from(knee());
    let run = f64::from(past * gain()) - f64::from(knee());
    let slope = rise / run;
    assert!(
        (slope - 1.0 / 1.001).abs() < SLOPE_SLACK,
        "a slope of {slope}"
    );
}

#[test]
fn a_sample_over_the_knee_is_counted_and_one_on_it_is_not() {
    let (under, over) = edge();
    assert_eq!(levelled(under).1, Tally::default());
    for sample in [over, -over, 1.0, -1.0, f32::MAX, f32::MIN] {
        assert_eq!(levelled(sample).1, LIMITED, "{sample}");
    }
    let mut block = [0.0, over, under, -1.0, 0.25, -under];
    assert_eq!(apply(&mut block).limited, 3);
}

#[test]
fn two_points_of_the_curve_stand_where_their_arithmetic_puts_them() {
    // After the gain: a sample at the ceiling, and the tap measured at +12.6 dBFS; what each leaves under the ceiling.
    let points = [(LIMIT_CEILING_DBFS, 1.4), (12.6, 0.1)];
    for (raised_dbfs, under_db) in points {
        let sample = amplitude(raised_dbfs) / gain();
        let (out, tally) = levelled(sample);
        let left = f64::from(LIMIT_CEILING_DBFS) - dbfs(out);
        assert!(
            (left - under_db).abs() <= POINT_SLACK_DB,
            "{raised_dbfs} dBFS leaves {left} dB under the ceiling"
        );
        // The curve as it is stated, in double precision.
        let (knee, ceiling) = (f64::from(knee()), f64::from(ceiling()));
        let over = (f64::from(sample * gain()) - knee) / (ceiling - knee);
        let stated = knee + (ceiling - knee) * over / (1.0 + over);
        let off_db = dbfs(out) - f64::from(DB_PER_DECADE) * stated.log10();
        assert!(off_db.abs() < FORMULA_SLACK_DB, "{out} for {stated}");
        assert_eq!(tally, LIMITED);
    }
}

#[test]
fn a_sample_that_is_not_finite_becomes_zero_and_is_counted_apart() {
    let mut block = [f32::NAN, 0.01, f32::INFINITY, 1.0, f32::NEG_INFINITY, -0.01];
    let tally = apply(&mut block);
    assert_eq!(
        tally,
        Tally {
            limited: 1,
            nonfinite: 3
        }
    );
    let kept = [0.01, 1.0, -0.01].map(|sample| levelled(sample).0);
    let wanted = [0.0, kept[0], 0.0, kept[1], 0.0, kept[2]];
    assert_eq!(block.map(f32::to_bits), wanted.map(f32::to_bits));
}

#[test]
fn an_impulse_leaves_at_the_sample_it_came_in() {
    const AT: usize = 37;
    for impulse in [0.01, -0.01, 1.0, -1.0, f32::MAX] {
        let mut block = [0.0_f32; 96];
        block[AT] = impulse;
        apply(&mut block);
        let sounding: Vec<usize> = (0..block.len())
            .filter(|&at| block[at].to_bits() != 0)
            .collect();
        assert_eq!(sounding, [AT], "{impulse}");
        assert_eq!(block[AT].to_bits(), levelled(impulse).0.to_bits());
    }
}

#[test]
fn a_block_cut_anywhere_gives_the_samples_and_the_tally_of_the_whole() {
    let whole: Vec<f32> = (0_u16..64)
        .map(|at| f32::from(at) / 40.0 - 0.8)
        .chain([f32::NAN, 2.0])
        .collect();
    let mut once = whole.clone();
    let tally = apply(&mut once);
    for cut in 0..=whole.len() {
        let mut parts = whole.clone();
        let (first, second) = parts.split_at_mut(cut);
        let (a, b) = (apply(first), apply(second));
        assert_eq!(
            (a.limited + b.limited, a.nonfinite + b.nonfinite),
            (tally.limited, tally.nonfinite)
        );
        let bits = |samples: &[f32]| -> Vec<u32> { samples.iter().map(|s| s.to_bits()).collect() };
        assert_eq!(bits(&parts), bits(&once), "cut at {cut}");
    }
}

proptest! {
    #[test]
    fn every_finite_sample_leaves_under_the_ceiling_with_its_sign(sample in any::<f32>()) {
        prop_assume!(sample.is_finite());
        let (out, tally) = levelled(sample);
        prop_assert!(out.abs() < ceiling(), "{} gives {}", sample, out);
        prop_assert_eq!(out.is_sign_negative(), sample.is_sign_negative());
        prop_assert_eq!(tally.nonfinite, 0);
        prop_assert_eq!(tally.limited, u32::from((sample * gain()).abs() > knee()));
    }
}

#[test]
fn the_largest_floats_leave_under_the_ceiling() {
    for sample in [f32::MAX, f32::MIN, f32::MAX / gain(), 1e30, -1e30] {
        let (out, _) = levelled(sample);
        assert!(out.abs() < ceiling() && out.is_finite(), "{sample}: {out}");
        assert_eq!(out.is_sign_negative(), sample.is_sign_negative());
    }
}
