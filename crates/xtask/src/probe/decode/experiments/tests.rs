//! The fixed experiments on streams small enough to check by hand.

use super::super::super::analyse::support::SILENT_UNIT;
use super::super::EXPECTED_CONFIG;
use super::super::candidate::Candidate;
use super::super::compare::LSB_PER_FULL_SCALE;
use super::super::compare::tests::ONE_LSB;
use super::*;

/// A frame short enough to write out.
const FRAME: usize = 2;

/// The bits that differ between two byte strings of one length.
fn bits_apart(a: &[u8], b: &[u8]) -> u32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(a, b)| (a ^ b).count_ones()).sum()
}

#[test]
fn mutations_never_escape_as_a_panic() {
    let mut candidate = Candidate::new(EXPECTED_CONFIG).unwrap();
    let counts = mutations(&mut candidate, &[&SILENT_UNIT[..]; 8], 1000);
    assert_eq!(counts.decoded + counts.errors + counts.panics, 1000);
    assert_eq!(counts.panics, 0);
    // A cut to no bytes at all is among the mutations, and the decoder has nothing to decode in it.
    assert!(counts.errors > 0);
}

#[test]
fn a_mutation_flips_one_bit_or_cuts_the_unit_short() {
    let unit = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
    let mut random = MUTATION_SEED;
    let (mut flipped, mut cut) = (0_u32, 0_u32);
    for _ in 0..1000 {
        let mutated = mutated(&unit, &mut random);
        if mutated.len() == unit.len() {
            assert_eq!(bits_apart(&mutated, &unit), 1);
            flipped += 1;
        } else {
            assert!(mutated.len() < unit.len());
            assert_eq!(mutated, unit[..mutated.len()]);
            cut += 1;
        }
    }
    assert!(flipped > 300 && cut > 300, "{flipped} flipped, {cut} cut");
    assert_eq!(mutated(&[], &mut random), b"");
}

#[test]
fn the_frames_a_splice_changes_are_counted_from_the_splice_on() {
    let full: Vec<f32> = (0..12_u8).map(|k| f32::from(k) / 64.0).collect();
    // Frame 2 is taken out; the frame that follows comes back 2 LSB off, the rest as they were.
    let mut spliced = [&full[..2 * FRAME], &full[3 * FRAME..]].concat();
    spliced[2 * FRAME + 1] += 2.0 * ONE_LSB;
    let found = splice(&full, &spliced, 2, FRAME).unwrap();
    assert_eq!(found.frames_changed, 1);
    // The step into the first frame after the splice: from sample 3 of the full decode to sample 6.
    assert_eq!(
        found.step_lsb,
        f64::from(full[6] - full[3]) * LSB_PER_FULL_SCALE
    );

    assert_eq!(splice(&full, &full, 2, FRAME), None);
    assert_eq!(splice(&full, &spliced, 5, FRAME), None);
}

#[test]
fn frames_that_changed_are_counted_only_between_streams_of_one_length() {
    let full = [0.0_f32; 4 * FRAME];
    let mut other = full;
    other[0] = 2.0 * ONE_LSB;
    other[7] = 1.0;
    assert_eq!(frames_changed(&full, &other, FRAME), Some(2));
    assert_eq!(frames_changed(&full, &full, FRAME), Some(0));
    assert_eq!(frames_changed(&full, &full[..3 * FRAME], FRAME), None);
    assert_eq!(frames_changed(&[], &[], FRAME), None);
    // Two streams of one length that is no whole number of frames have no last frame to lay on one.
    assert_eq!(frames_changed(&full[..5], &full[..5], FRAME), None);
}

#[test]
fn a_decoder_tells_the_streams_apart_only_against_its_own_whole_decode() {
    let full = [0.0_f32; 4 * FRAME];
    let mut other = full;
    other[3] = 2.0 * ONE_LSB;
    let told = |full, control| told_apart(full, control, FRAME);
    assert_eq!(told(Some(&full[..]), None), Some(true));
    assert_eq!(told(Some(&full[..]), Some(&other[..])), Some(true));
    assert_eq!(told(Some(&full[..]), Some(&full[..3 * FRAME])), Some(true));
    assert_eq!(told(Some(&full[..]), Some(&[][..])), Some(true));
    assert_eq!(told(Some(&full[..]), Some(&full[..])), Some(false));
    // With no whole decode of the sound stream, a failure on the damaged one tells nothing.
    assert_eq!(told(None, None), None);
    assert_eq!(told(None, Some(&full[..])), None);
    assert_eq!(told(Some(&[][..]), Some(&[][..])), None);
    assert_eq!(told(Some(&full[..5]), Some(&full[..5])), None);
}

#[test]
fn the_control_discriminates_when_one_decoder_with_a_whole_decode_told_the_streams_apart() {
    assert_eq!(discriminates([Some(true), Some(false)]), "yes");
    assert_eq!(discriminates([None, Some(true)]), "yes");
    assert_eq!(discriminates([Some(false), Some(false)]), "no");
    assert_eq!(discriminates([Some(false), None]), "no");
    assert_eq!(discriminates([None, None]), "refused");
}

#[test]
fn a_unit_is_taken_out_only_where_a_unit_follows_it() {
    let units: [&[u8]; 4] = [&[0], &[1], &[2], &[3]];
    assert_eq!(without_unit(&units, 0).unwrap(), [&[1][..], &[2], &[3]]);
    assert_eq!(without_unit(&units, 2).unwrap(), [&[0][..], &[1], &[3]]);
    assert_eq!(without_unit(&units, 3), None);
    assert_eq!(without_unit(&units, 4), None);
    assert_eq!(without_unit(&[], 0), None);
}
