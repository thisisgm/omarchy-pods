//! The three readers of a run's text files: what they accept, what they refuse and what an error may show.

use pods_proto::sdulog::Direction;
use proptest::prelude::*;

use super::*;

/// An address shaped string no error may repeat.
const SECRET: &str = "AA:BB:CC:DD:EE:FF";
/// A fragment of `SECRET` short enough to survive any quoting.
const SECRET_PART: &str = "AA:BB";
/// The longest kind the marks reader takes.
const LONGEST_KIND: usize = 16;
/// The longest script name the meta reader takes.
const LONGEST_SCRIPT: usize = 32;

fn line_error(file: &'static str, line: usize, rule: LineRule) -> InputError {
    InputError::Line { file, line, rule }
}

#[test]
fn reads_every_line_of_a_log() {
    let records = parse_log("5 > 0400\n7 < 0401\n7 < 02\n").unwrap();
    let seen: Vec<_> = records
        .iter()
        .map(|record| (record.at_ns, record.direction, record.sdu.clone()))
        .collect();
    assert_eq!(
        seen,
        [
            (5, Direction::ToPods, vec![0x04, 0x00]),
            (7, Direction::FromPods, vec![0x04, 0x01]),
            (7, Direction::FromPods, vec![0x02]),
        ]
    );
    assert_eq!(parse_log("9 < 00").unwrap().len(), 1);
}

#[test]
fn whole_lines_leaves_out_a_last_line_without_its_newline() {
    let cases: [(&[u8], &[u8], bool); 10] = [
        (b"", b"", false),
        (b"\n", b"\n", false),
        (b"1 < 00\n", b"1 < 00\n", false),
        (b"1 < 00\n\n", b"1 < 00\n\n", false),
        (b"1 < 00\n2 < 0", b"1 < 00\n", true),
        (b"1 < 00\n2 < 01", b"1 < 00\n", true),
        (b"1 < 00", b"", true),
        (b"1\tstart\t\n2\tst", b"1\tstart\t\n", true),
        (
            b"1\tcue\tcaf\xc3\xa9\n2\tcue\tcaf\xc3",
            b"1\tcue\tcaf\xc3\xa9\n",
            true,
        ),
        (b"\xff\n\xff", b"\xff\n", true),
    ];
    for (bytes, whole, cut) in cases {
        assert_eq!(whole_lines(bytes), (whole, cut), "{bytes:?}");
    }
    let (whole, cut) = whole_lines(b"1 < 00\n2 < 01\n3 < 0");
    assert!(cut);
    let whole = text_of(LOG_FILE, whole).unwrap();
    assert_eq!(parse_log(whole).unwrap().len(), 2);
    let bad_before_the_cut = whole_lines(b"1 < 00\n2 < 0\n3 < 0").0;
    let bad_hex = LineRule::Log(LogError::BadHex { byte: 0 });
    assert_eq!(
        parse_log(text_of(LOG_FILE, bad_before_the_cut).unwrap()),
        Err(line_error(LOG_FILE, 2, bad_hex))
    );
}

#[test]
fn garbage_log_is_refused_with_the_line_number() {
    let text = format!("1 < 00\n2 < 01\n3 < {SECRET}\n4 < 02\n");
    let error = parse_log(&text).unwrap_err();
    let bad_hex = LineRule::Log(LogError::BadHex { byte: 0 });
    assert_eq!(error, line_error(LOG_FILE, 3, bad_hex));
    let shown = error.to_string();
    assert!(shown.starts_with("sdu.log line 3: "), "{shown}");
    assert!(shown.contains("bad hex"), "{shown}");
    assert!(!shown.contains(SECRET_PART), "{shown}");

    for (text, line, rule) in [
        ("zz\n", 1, LogError::MissingField),
        ("1 < 00\n\n", 2, LogError::MissingField),
        ("1 < 00\n2 < 0", 2, LogError::BadHex { byte: 0 }),
        ("1 < 00\n2 x 00\n", 2, LogError::BadDirection),
        ("1 < 00\n2 < 00\n03 < 00\n", 3, LogError::BadStamp),
    ] {
        let expected = line_error(LOG_FILE, line, LineRule::Log(rule));
        assert_eq!(parse_log(text), Err(expected), "{text:?}");
    }
}

#[test]
fn an_empty_log_is_refused() {
    assert_eq!(parse_log(""), Err(InputError::EmptyLog));
    let shown = InputError::EmptyLog.to_string();
    assert!(shown.contains("sdu.log"), "{shown}");
    assert!(!shown.bytes().any(|byte| byte.is_ascii_digit()), "{shown}");
}

#[test]
fn a_log_whose_stamps_go_back_is_refused() {
    assert_eq!(
        parse_log("5 < 00\n5 < 01\n4 < 02\n"),
        Err(line_error(LOG_FILE, 3, LineRule::OutOfOrder))
    );
    assert_eq!(parse_log("5 < 00\n5 < 01\n6 < 02\n").unwrap().len(), 3);
}

#[test]
fn reads_marks_and_names_only_the_kinds_it_uses() {
    let text = "0\tconnect\t\n10\tstart\t\n20\tcue\tSay left\tnow\n30\tstop\t\n30\ttruncated\t5000\n50\tend\tdone\n";
    let marks = parse_marks(text).unwrap();
    let seen: Vec<_> = marks.iter().map(|mark| (mark.at_ns, mark.kind)).collect();
    assert_eq!(
        seen,
        [
            (0, MarkKind::Other),
            (10, MarkKind::Start),
            (20, MarkKind::Other),
            (30, MarkKind::Stop),
            (30, MarkKind::Truncated),
            (50, MarkKind::Other),
        ]
    );
    assert_eq!(parse_marks(""), Ok(Vec::new()));
}

#[test]
fn a_bad_mark_is_refused_with_the_line_number() {
    let too_long_kind = format!("1\t{}\t", "a".repeat(LONGEST_KIND + 1));
    for (line, rule) in [
        ("10 start", LineRule::MarkMissingField),
        ("10\tstart", LineRule::MarkMissingField),
        ("", LineRule::MarkMissingField),
        ("x\tstart\t", LineRule::MarkBadStamp),
        ("01\tstart\t", LineRule::MarkBadStamp),
        ("+1\tstart\t", LineRule::MarkBadStamp),
        ("\tstart\t", LineRule::MarkBadStamp),
        ("18446744073709551616\tstart\t", LineRule::MarkBadStamp),
        ("1\tStart\t", LineRule::MarkBadKind),
        ("1\t\t", LineRule::MarkBadKind),
        ("1\tstart stop\t", LineRule::MarkBadKind),
        ("1\tsta_rt\t", LineRule::MarkBadKind),
        (too_long_kind.as_str(), LineRule::MarkBadKind),
    ] {
        let text = format!("0\tconnect\t\n{line}\n");
        let expected = line_error(MARKS_FILE, 2, rule);
        assert_eq!(parse_marks(&text), Err(expected), "{line:?}");
    }
    assert_eq!(
        parse_marks("5\tstart\t\n4\tstop\t\n"),
        Err(line_error(MARKS_FILE, 2, LineRule::OutOfOrder))
    );
    let longest_kind = format!("1\t{}\t", "a".repeat(LONGEST_KIND));
    assert_eq!(parse_marks(&longest_kind).unwrap().len(), 1);
    let shown = line_error(MARKS_FILE, 2, LineRule::MarkBadKind).to_string();
    assert!(shown.starts_with("marks.tsv line 2: "), "{shown}");
}

#[test]
fn reads_the_script_from_meta() {
    let text = "script=soak\ntool=xtask 0.0.0\nrealtime_at_zero_ns=1763640000000000000\n";
    assert_eq!(parse_meta(text).unwrap().script, "soak");
    let longest = "a".repeat(LONGEST_SCRIPT);
    let text = format!("tool=x=y\nscript={longest}\n");
    assert_eq!(parse_meta(&text).unwrap().script, longest);
    assert_eq!(
        parse_meta("script=first-word-2").unwrap().script,
        "first-word-2"
    );
}

#[test]
fn a_bad_meta_is_refused() {
    assert_eq!(
        parse_meta("script=soak\ntool\n"),
        Err(line_error(META_FILE, 2, LineRule::MetaMissingEquals))
    );
    assert_eq!(
        parse_meta("script=soak\nscript=voice\n"),
        Err(line_error(META_FILE, 2, LineRule::MetaScriptTwice))
    );
    let too_long = "a".repeat(LONGEST_SCRIPT + 1);
    for value in ["", "So", "a b", "a/b", "a\tb", "soak ", too_long.as_str()] {
        let text = format!("tool=xtask\nscript={value}\n");
        let expected = line_error(META_FILE, 2, LineRule::MetaBadScript);
        assert_eq!(parse_meta(&text), Err(expected), "{value:?}");
    }
    for text in ["", "tool=xtask\n", "scripts=soak\n", " script=soak\n"] {
        assert_eq!(parse_meta(text), Err(InputError::NoScript), "{text:?}");
    }
    let shown = line_error(META_FILE, 2, LineRule::MetaBadScript).to_string();
    assert!(shown.starts_with("meta.txt line 2: "), "{shown}");
    assert!(InputError::NoScript.to_string().contains("meta.txt"));
}

#[test]
fn errors_never_repeat_the_line() {
    let logs = [
        format!("{SECRET}\n"),
        format!("1 < 00\n{SECRET} < 00\n"),
        format!("1 {SECRET} 00\n"),
        format!("1 < 00{SECRET}\n"),
    ];
    let marks = [
        format!("{SECRET}\n"),
        format!("{SECRET}\tstart\t\n"),
        format!("1\t{SECRET}\t\n"),
        format!("2\tstart\t{SECRET}\n1\tstop\t{SECRET}\n"),
    ];
    let metas = [
        format!("{SECRET}\n"),
        format!("script={SECRET}\n"),
        format!("script=soak\nscript={SECRET}\n"),
        format!("tool={SECRET}\n"),
    ];
    let errors = logs
        .iter()
        .map(|text| parse_log(text).unwrap_err())
        .chain(marks.iter().map(|text| parse_marks(text).unwrap_err()))
        .chain(metas.iter().map(|text| parse_meta(text).unwrap_err()));
    for error in errors {
        let shown = format!("{error} {error:?}");
        assert!(!shown.contains(SECRET_PART), "{shown}");
    }
}

#[test]
fn text_must_be_utf8() {
    assert_eq!(text_of(MARKS_FILE, b"1\tstart\t\n"), Ok("1\tstart\t\n"));
    let cases: [(&[u8], usize); 4] = [
        (b"1 \xff", 1),
        (b"\xff\n2 < 00\n", 1),
        (b"1 < 00\n2 < 00\n3 \xff< 00\n4 < 00\n", 3),
        (b"1 < 00\n\n\n\xc3", 4),
    ];
    for (bytes, line) in cases {
        let error = text_of(LOG_FILE, bytes).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("sdu.log line {line}: it is not UTF-8 text")
        );
    }
}

/// Lines a reader takes, lines close to them and arbitrary text, so the generated files reach past the first check.
fn lines(valid: &'static [&'static str]) -> impl Strategy<Value = String> {
    let line = prop_oneof![
        proptest::sample::select(valid).prop_map(str::to_owned),
        "[0-9]{1,3}[ \t][a-z<>]{0,6}[ \t][0-9a-f]{0,8}",
        "[a-z]{0,8}=[a-z0-9 -]{0,8}",
        any::<String>(),
    ];
    proptest::collection::vec(line, 0..8).prop_map(|lines| lines.join("\n"))
}

proptest! {
    #[test]
    fn parse_log_never_panics(text in lines(&["1 < 00", "2 > 0400", "3 < 0400040058000100"])) {
        match parse_log(&text) {
            Ok(records) => {
                prop_assert!(!records.is_empty());
                prop_assert!(records.is_sorted_by_key(|record| record.at_ns));
            }
            Err(error) => prop_assert!(!error.to_string().is_empty()),
        }
    }

    #[test]
    fn parse_marks_never_panics(text in lines(&["1\tstart\t", "2\tstop\t", "3\tcue\tSay left", "4\ttruncated\t5000"])) {
        match parse_marks(&text) {
            Ok(marks) => prop_assert!(marks.is_sorted_by_key(|mark| mark.at_ns)),
            Err(error) => prop_assert!(!error.to_string().is_empty()),
        }
    }

    #[test]
    fn parse_meta_never_panics(text in lines(&["script=soak", "tool=xtask 0.0.0", "realtime_at_zero_ns=17"])) {
        match parse_meta(&text) {
            Ok(meta) => {
                prop_assert!((1..=LONGEST_SCRIPT).contains(&meta.script.len()));
                prop_assert!(meta.script.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'));
            }
            Err(error) => prop_assert!(!error.to_string().is_empty()),
        }
    }

    #[test]
    fn whole_lines_never_panics_and_splits_at_the_last_newline(bytes in proptest::collection::vec(prop_oneof![Just(b'\n'), any::<u8>()], 0..64)) {
        let (whole, cut) = whole_lines(&bytes);
        prop_assert!(bytes.starts_with(whole));
        prop_assert!(whole.is_empty() || whole.ends_with(b"\n"));
        let left_out = bytes.strip_prefix(whole).unwrap();
        prop_assert!(!left_out.contains(&b'\n'));
        prop_assert_eq!(cut, !left_out.is_empty());
    }

    #[test]
    fn text_of_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
        if let Err(error) = text_of(LOG_FILE, &bytes) {
            prop_assert!(!error.to_string().is_empty());
        }
    }
}
