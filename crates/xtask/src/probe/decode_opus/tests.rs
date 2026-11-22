//! The rows of a comparison and of the control, from frames written out by hand: no decoder is in these.

use super::experiments::verdict_rows;
use super::*;
use crate::probe::decode::REFUSED;
use crate::probe::decode::compare::tests::{
    BETWEEN_THE_RULES, FRAME, ONE_LSB, PAST_BOTH_RULES, laid,
};

fn row<'rows>(rows: &'rows [Row], key: &str) -> (&'rows str, &'rows str) {
    let found = rows.iter().find(|row| row.key == key).unwrap();
    (&found.value, &found.note)
}

#[test]
fn the_rows_against_an_oracle_give_the_largest_share_and_the_frames_over_the_gate() {
    // One frame past 1 LSB and inside the share of its peak, then the same with a frame past both rules after it.
    let streams = [
        (vec![BETWEEN_THE_RULES], "1", "5.000e-4", "0"),
        (
            vec![BETWEEN_THE_RULES, PAST_BOTH_RULES],
            "2",
            "2.500e-1",
            "1",
        ),
    ];
    for (cases, over_one_lsb, of_peak, over_gate) in streams {
        let (candidate, oracle) = laid(&cases);
        let compared = compare(&candidate, &oracle, FRAME).map_err(|error| error.to_string());
        for (decoder, key) in [
            (OracleDecoder::Libopus, "libopus"),
            (OracleDecoder::Native, "ffopus"),
        ] {
            let rows = against_rows(decoder, &compared);
            let expected = [
                ("frames_compared", "7"),
                ("offset_units", "0"),
                ("max_lsb", "2.000"),
                ("frames_over_one_lsb", over_one_lsb),
                ("max_of_peak", of_peak),
                ("frames_over_gate", over_gate),
            ];
            assert_eq!(rows.len(), expected.len());
            for (name, value) in expected {
                let (found, note) = row(&rows, &format!("{key}_{name}"));
                assert_eq!(found, value, "{key} {name}");
                let not_a_gate = note.starts_with(NOT_A_GATE);
                assert_eq!(not_a_gate, decoder == OracleDecoder::Native, "{note}");
            }
        }
    }
    let rows = against_rows(OracleDecoder::Libopus, &Err("no decode".to_owned()));
    let figures = [
        "max_lsb",
        "frames_over_one_lsb",
        "max_of_peak",
        "frames_over_gate",
    ];
    for name in figures {
        assert_eq!(
            row(&rows, &format!("libopus_{name}")),
            (REFUSED, "no decode")
        );
    }
}

#[test]
fn the_largest_share_says_a_quiet_frame_can_lead_it_with_no_frame_over_the_gate() {
    // Half an LSB apart at a peak of 8 LSB: a sixteenth of its peak, and inside the gate by 1 LSB alone.
    let quiet = (
        [8.0 * ONE_LSB, 2.0 * ONE_LSB, 0.0, 0.0],
        [8.0 * ONE_LSB, 2.5 * ONE_LSB, 0.0, 0.0],
    );
    let (candidate, oracle) = laid(&[quiet]);
    let compared = compare(&candidate, &oracle, FRAME).map_err(|error| error.to_string());
    let rows = against_rows(OracleDecoder::Libopus, &compared);
    let (share, note) = row(&rows, "libopus_max_of_peak");
    assert_eq!(share, "6.250e-2");
    assert!(share.parse::<f64>().unwrap() > AGREEMENT_OF_PEAK);
    assert_eq!(row(&rows, "libopus_frames_over_gate").0, "0");
    let said = "so a quiet frame can put this above 0.001 with no frame over the gate";
    assert!(note.ends_with(said), "{note}");
}

#[test]
fn the_control_discriminates_only_where_the_gate_puts_a_frame_outside() {
    let verdict = |cases: &[_]| {
        let (whole, damaged) = laid(cases);
        let rows = verdict_rows("control_", Some((&whole, &damaged)), FRAME);
        let over = row(&rows, "control_candidate_frames_over_gate")
            .0
            .to_owned();
        (over, row(&rows, "control_discriminates").0.to_owned())
    };
    // A frame past 1 LSB that is inside the share of its peak is not a frame the gate tells apart.
    assert_eq!(
        verdict(&[BETWEEN_THE_RULES]),
        ("0".to_owned(), "no".to_owned())
    );
    let both = verdict(&[BETWEEN_THE_RULES, PAST_BOTH_RULES, PAST_BOTH_RULES]);
    assert_eq!(both, ("2".to_owned(), "yes".to_owned()));

    let rows = verdict_rows("control_", None, FRAME);
    assert_eq!(row(&rows, "control_candidate_frames_over_gate").0, REFUSED);
    assert_eq!(row(&rows, "control_discriminates").0, REFUSED);
}
