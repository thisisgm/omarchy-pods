//! The label of a run from the clock rows of its starts, and its refusals.

use super::*;

/// Starts as an analysis made before the coarse row gives them: a slope each, and no coarse clock.
fn slopes<'row>(values: &[&'row str]) -> Vec<(&'row str, Option<&'row str>)> {
    values.iter().map(|&slope| (slope, None)).collect()
}

fn label(rate_hz: u32, by_slope: usize, by_coarse: usize, without_audio: usize) -> Label {
    Label {
        rate_hz,
        by_slope,
        by_coarse,
        without_audio,
    }
}

#[test]
fn the_label_is_the_rate_every_slope_is_within_one_percent_of() {
    for (values, rate_hz) in [
        (&["63998.412"][..], 64_000),
        (&["64640.000", "63360.000"], 64_000),
        (&["48000.000"], 48_000),
        (&["47520.000", "48480.000"], 48_000),
    ] {
        let found = Label::of(&slopes(values)).unwrap();
        assert_eq!(found, label(rate_hz, values.len(), 0, 0));
    }
}

#[test]
fn a_start_with_no_slope_is_labelled_by_its_coarse_clock_within_five_percent() {
    for (coarse, rate_hz) in [
        ("63987", 64_000),
        ("60800", 64_000),
        ("67200", 64_000),
        ("50400", 48_000),
        ("45600", 48_000),
    ] {
        let found = Label::of(&[("refused", Some(coarse))]).unwrap();
        assert_eq!(found, label(rate_hz, 0, 1, 0), "{coarse}");
    }
    for coarse in ["60799", "67201", "50401", "45599", "56000", "NaN", "0"] {
        let refused = Label::of(&[("refused", Some(coarse))]).unwrap_err();
        assert!(
            matches!(
                &refused,
                LabelError::NeitherRate { key, hz, .. } if *key == COARSE_CLOCK_KEY && hz == coarse
            ),
            "{refused}"
        );
        let said = format!("{COARSE_CLOCK_KEY} {coarse} is within 5 percent of neither");
        assert!(refused.to_string().starts_with(&said), "{refused}");
    }
}

#[test]
fn a_slope_is_read_before_the_coarse_clock_and_never_replaced_by_it() {
    let found = Label::of(&[("63998.412", Some("48000"))]).unwrap();
    assert_eq!(found, label(64_000, 1, 0, 0));
    for slope in [
        "56000.000",
        "64640.001",
        "63359.999",
        "47519.999",
        "48480.001",
        "NaN",
        "0",
    ] {
        let refused = Label::of(&[(slope, Some("64000"))]).unwrap_err();
        assert!(
            matches!(
                &refused,
                LabelError::NeitherRate { key, hz, .. } if *key == CLOCK_KEY && hz == slope
            ),
            "{refused}"
        );
        let said = format!("{CLOCK_KEY} {slope} is within 1 percent of neither");
        assert!(refused.to_string().starts_with(&said), "{refused}");
    }
}

#[test]
fn a_start_without_audio_needs_no_rate_and_gives_none() {
    let mixed = [
        ("refused", Some(NONE)),
        ("63998.412", Some("64012")),
        ("refused", Some("63987")),
    ];
    assert_eq!(Label::of(&mixed).unwrap(), label(64_000, 1, 1, 1));
    for no_figure in [&[("refused", Some(NONE))][..], &[]] {
        let refused = Label::of(no_figure).unwrap_err();
        assert!(matches!(refused, LabelError::NoClock), "{refused}");
    }
}

#[test]
fn a_start_that_holds_audio_and_gives_no_figure_refuses_the_run() {
    let thin = [("64000.000", Some("64000")), ("refused", Some("refused"))];
    let refused = Label::of(&thin).unwrap_err();
    assert!(
        matches!(refused, LabelError::ClockNotMeasured { start: 2 }),
        "{refused}"
    );
    let refused = Label::of(&slopes(&["refused", "64000.000"])).unwrap_err();
    assert!(
        matches!(refused, LabelError::ClockNotMeasured { start: 1 }),
        "{refused}"
    );
    assert_eq!(
        refused.to_string(),
        "start 1 of analysis.tsv holds audio and gives neither clock_hz nor clock_hz_coarse as a number: its clock was not measured, and the notes of its clock rows say why"
    );
}

#[test]
fn starts_of_two_rates_refuse_the_run() {
    for starts in [
        slopes(&["64000.000", "48000.000"]),
        vec![("64000.000", Some("64000")), ("refused", Some("48010"))],
        vec![("refused", Some("48010")), ("refused", Some("63987"))],
    ] {
        let refused = Label::of(&starts).unwrap_err();
        assert!(matches!(refused, LabelError::ClocksDisagree), "{refused}");
    }
}

#[test]
fn the_two_rows_of_a_start_are_paired_by_their_order_in_the_table() {
    let analysis = "key\tvalue\tunit\tnote\n\
        clock_hz\trefused\tHz\tstart 1\n\
        clock_hz_coarse\tnone\tHz\tstart 1\n\
        clock_hz\t47990.125\tHz\tstart 2\n\
        clock_hz_coarse\t48011\tHz\tstart 2\n\
        clock_hz\trefused\tHz\tstart 3\n\
        clock_hz_coarse\t47950\tHz\tstart 3\n";
    let found = Label::read(analysis).unwrap();
    assert_eq!(found, label(48_000, 1, 1, 1));
    let rows = found.rows().map(|row| (row.key, row.value));
    let said = |key: &str, value: &str| (key.to_owned(), value.to_owned());
    assert_eq!(
        rows,
        [
            said("rate_label", "48000"),
            said("rate_label_from_clock_hz", "1"),
            said("rate_label_from_clock_hz_coarse", "1"),
            said("rate_label_starts_without_audio", "1"),
        ]
    );
}

#[test]
fn clock_rows_that_cannot_be_paired_refuse_the_run() {
    let slope = |start: u32| format!("clock_hz\t64000.000\tHz\tstart {start}\n");
    let coarse = |start: u32| format!("clock_hz_coarse\t64000\tHz\tstart {start}\n");
    for (analysis, counts) in [
        ([slope(1), coarse(1), slope(2)].concat(), (2, 1)),
        ([slope(1), coarse(1), coarse(2)].concat(), (1, 2)),
        (coarse(1), (0, 1)),
    ] {
        let refused = Label::read(&analysis).unwrap_err();
        assert!(
            matches!(refused, LabelError::RowsUnpaired { slopes, coarse } if (slopes, coarse) == counts),
            "{refused}"
        );
    }
    let before_the_coarse_row = [slope(1), slope(2)].concat();
    assert_eq!(
        Label::read(&before_the_coarse_row).unwrap(),
        label(64_000, 2, 0, 0)
    );
}
