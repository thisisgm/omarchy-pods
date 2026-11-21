//! The analyse command against a run in a temporary store: what it writes, what it refuses and what it leaves alone.

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use pods_proto::sdulog::{Record, format_line};
use tempfile::TempDir;

use super::super::store::{Run, Session, StoreError, root};
use super::input::{InputError, LineRule};
use super::support::{NS_PER_MS, OPAQUE, SEED, SILENT_UNIT, audio, from_pods, shows};
use super::synth::synth;
use super::*;

/// A stamp of the shape `date -u +%Y%m%dT%H%M%SZ` prints.
const STAMP: &str = "20261120T120000Z";
/// The mode every file of a run has.
const OWNER_FILE: u32 = 0o600;
/// The marks of a run that starts at zero and stops after 62 seconds.
const MARKS: &str =
    "0\tconnect\t\n0\tstart\t\n62000000000\tstop\t\n63000000000\tend\tscript done\n";
/// The meta file of a soak.
const META: &str = "script=soak\ntool=xtask 0.0.0\nrealtime_at_zero_ns=1763640000000000000\n";
/// The audio SDUs `synth` makes in 61 s at 64 kHz, four units of 480 samples each: 61 x 64000 / 1920, rounded down.
const SOAK_SDUS: usize = 2033;
/// The audio SDUs `synth` makes in 31 s at the same clock: 31 x 64000 / 1920, rounded down.
const SHORT_SDUS: usize = 1033;
/// A device information packet with two made up strings.
const DEVICE_INFO: &[u8] =
    b"\x04\x00\x04\x00\x1d\x00\x02\xed\x00\x04\x00Probe Pods\x00SERIAL0XYZ\x00";

fn log_text(records: &[Record]) -> String {
    records
        .iter()
        .map(|record| format_line(record.at_ns, record.direction, &record.sdu).unwrap() + "\n")
        .collect()
}

/// A run named `name` in a store under `home`, holding the given files.
fn run_with(home: &TempDir, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let session = Session::open(&root(home.path().as_os_str()), STAMP).unwrap();
    let run = Run::create(&session, name).unwrap();
    for (file, contents) in files {
        run.create_file(file).unwrap().write_all(contents).unwrap();
    }
    session.path().join(name)
}

fn soak_run(home: &TempDir, name: &str, records: &[Record]) -> PathBuf {
    let log = log_text(records);
    let files: [(&str, &[u8]); 3] = [
        (LOG_FILE, log.as_bytes()),
        (MARKS_FILE, MARKS.as_bytes()),
        (META_FILE, META.as_bytes()),
    ];
    run_with(home, name, &files)
}

fn names_in(run: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(run)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

#[test]
fn writes_both_tables_into_the_run_and_returns_the_analysis() {
    let home = tempfile::tempdir().unwrap();
    let run = soak_run(&home, "soak-1", &synth(64_000, 480, 4, 61, 0, SEED));
    let table = analyse_run(&run).unwrap();

    assert!(table.starts_with("key\tvalue\tunit\tnote\nscript\tsoak\tname\tfrom meta.txt\n"));
    assert!(table.contains("\nclock_hz\t64000.000\tHz\t"), "{table}");
    assert_eq!(fs::read_to_string(run.join(ANALYSIS_FILE)).unwrap(), table);
    let units = fs::read_to_string(run.join(UNITS_FILE)).unwrap();
    assert!(
        units.starts_with("index\tsdu\tarrival_ns\tstamp\tlen\tmax_sfb\n0\t1\t30000000\t0\t2\t0\n")
    );
    let (header, units_per_sdu) = (1, 4);
    assert_eq!(units.lines().count(), header + SOAK_SDUS * units_per_sdu);
    assert_eq!(
        names_in(&run),
        [
            "analysis.tsv",
            "marks.tsv",
            "meta.txt",
            "sdu.log",
            "units.tsv"
        ]
    );
    assert_eq!(mode_of(&run.join(ANALYSIS_FILE)), OWNER_FILE);
    assert_eq!(mode_of(&run.join(UNITS_FILE)), OWNER_FILE);
}

#[test]
fn device_strings_go_to_their_own_file_and_nowhere_else() {
    let home = tempfile::tempdir().unwrap();
    let records = [
        from_pods(5, DEVICE_INFO),
        audio(30 * NS_PER_MS, &OPAQUE, &[(0, &SILENT_UNIT)]),
    ];
    let run = soak_run(&home, "voice-1", &records);
    let table = analyse_run(&run).unwrap();

    let strings = fs::read_to_string(run.join(DEVICE_INFO_FILE)).unwrap();
    assert_eq!(strings, "Probe Pods\nSERIAL0XYZ\n");
    assert_eq!(mode_of(&run.join(DEVICE_INFO_FILE)), OWNER_FILE);
    let units = fs::read_to_string(run.join(UNITS_FILE)).unwrap();
    for private in ["Probe", "SERIAL0XYZ"] {
        assert!(!shows(&table, private), "{table}");
        assert!(!shows(&units, private), "{units}");
    }
}

#[test]
fn a_second_analysis_is_refused_and_changes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let records = [audio(30 * NS_PER_MS, &OPAQUE, &[(0, &SILENT_UNIT)])];
    let run = soak_run(&home, "soak-2", &records);
    let table = analyse_run(&run).unwrap();
    let before = names_in(&run);

    let error = analyse_run(&run).unwrap_err();
    assert!(
        matches!(error, AnalyseError::Store(StoreError::FileExists { .. })),
        "{error}"
    );
    assert!(error.to_string().contains("analysis.tsv"), "{error}");
    assert_eq!(names_in(&run), before);
    assert_eq!(fs::read_to_string(run.join(ANALYSIS_FILE)).unwrap(), table);
}

#[test]
fn a_refused_input_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let inputs = ["marks.tsv", "meta.txt", "sdu.log"];
    let good_log = "1 < 00\n";
    let cases: [(&str, &str, &str, &str, InputError); 5] = [
        (
            "garbage",
            "1 < 00\n2 < 01\n3 < AA:BB\n",
            MARKS,
            META,
            InputError::Line {
                file: LOG_FILE,
                line: 3,
                rule: LineRule::Log(pods_proto::sdulog::LogError::BadHex { byte: 0 }),
            },
        ),
        ("empty", "", MARKS, META, InputError::EmptyLog),
        (
            "marks",
            good_log,
            "0\tstart\t\nx\tstop\t\n",
            META,
            InputError::Line {
                file: MARKS_FILE,
                line: 2,
                rule: LineRule::MarkBadStamp,
            },
        ),
        (
            "meta",
            good_log,
            MARKS,
            "tool=xtask\n",
            InputError::NoScript,
        ),
        (
            "order",
            "5 < 00\n4 < 00\n",
            MARKS,
            META,
            InputError::Line {
                file: LOG_FILE,
                line: 2,
                rule: LineRule::OutOfOrder,
            },
        ),
    ];
    for (name, log, marks, meta, expected) in cases {
        let files: [(&str, &[u8]); 3] = [
            (LOG_FILE, log.as_bytes()),
            (MARKS_FILE, marks.as_bytes()),
            (META_FILE, meta.as_bytes()),
        ];
        let run = run_with(&home, name, &files);
        let error = analyse_run(&run).unwrap_err();
        assert!(
            matches!(&error, AnalyseError::Input(found) if *found == expected),
            "{name}: {error}"
        );
        assert!(!error.to_string().contains("AA:BB"), "{error}");
        assert_eq!(names_in(&run), inputs, "{name}");
    }
}

#[test]
fn a_name_that_is_taken_refuses_the_analysis_before_any_file_is_made() {
    let home = tempfile::tempdir().unwrap();
    let log = log_text(&[
        from_pods(5, DEVICE_INFO),
        audio(30 * NS_PER_MS, &OPAQUE, &[(0, &SILENT_UNIT)]),
    ]);
    let cases = [
        ("units", UNITS_FILE, "units.tsv"),
        ("device", DEVICE_INFO_FILE, "device-info.txt"),
    ];
    for (name, in_the_way, shown) in cases {
        let files: [(&str, &[u8]); 4] = [
            (LOG_FILE, log.as_bytes()),
            (MARKS_FILE, MARKS.as_bytes()),
            (META_FILE, META.as_bytes()),
            (in_the_way, b"not from this analysis"),
        ];
        let run = run_with(&home, name, &files);
        let before = names_in(&run);

        let error = analyse_run(&run).unwrap_err();
        assert!(
            matches!(error, AnalyseError::Store(StoreError::FileExists { .. })),
            "{name}: {error}"
        );
        assert!(error.to_string().contains(shown), "{name}: {error}");
        assert_eq!(names_in(&run), before, "{name}");
        assert_eq!(
            fs::read(run.join(in_the_way)).unwrap(),
            b"not from this analysis",
            "{name}"
        );

        fs::remove_file(run.join(in_the_way)).unwrap();
        let table = analyse_run(&run).unwrap();
        assert_eq!(fs::read_to_string(run.join(ANALYSIS_FILE)).unwrap(), table);
    }
}

/// Makes `file` of `run` exactly `len` bytes: NUL bytes, which are text, closed by one newline.
fn grow(run: &Path, file: &str, len: u64) {
    let mut grown = fs::OpenOptions::new()
        .write(true)
        .open(run.join(file))
        .unwrap();
    grown
        .seek(SeekFrom::Start(len.checked_sub(1).unwrap()))
        .unwrap();
    grown.write_all(b"\n").unwrap();
}

#[test]
fn the_packet_log_is_read_past_the_general_bound_up_to_its_own() {
    let home = tempfile::tempdir().unwrap();
    let files: [(&str, &[u8]); 3] = [
        (LOG_FILE, b""),
        (MARKS_FILE, MARKS.as_bytes()),
        (META_FILE, META.as_bytes()),
    ];

    let long = run_with(&home, "long", &files);
    grow(&long, LOG_FILE, MAX_READ_BYTES + 1);
    let error = analyse_run(&long).unwrap_err();
    let unreadable = InputError::Line {
        file: LOG_FILE,
        line: 1,
        rule: LineRule::Log(pods_proto::sdulog::LogError::MissingField),
    };
    assert!(
        matches!(&error, AnalyseError::Input(found) if *found == unreadable),
        "{error}"
    );

    let too_long = run_with(&home, "too-long", &files);
    grow(&too_long, LOG_FILE, MAX_LOG_BYTES + 1);
    let error = analyse_run(&too_long).unwrap_err();
    assert!(
        matches!(error, AnalyseError::Store(StoreError::TooLarge { limit, .. }) if limit == MAX_LOG_BYTES),
        "{error}"
    );

    let long_marks = run_with(&home, "long-marks", &files);
    grow(&long_marks, MARKS_FILE, MAX_READ_BYTES + 1);
    let error = analyse_run(&long_marks).unwrap_err();
    assert!(
        matches!(error, AnalyseError::Store(StoreError::TooLarge { limit, .. }) if limit == MAX_READ_BYTES),
        "{error}"
    );
}

fn value_of<'a>(table: &'a str, key: &str) -> &'a str {
    let row = table
        .lines()
        .find(|line| line.split('\t').next() == Some(key));
    row.unwrap().split('\t').nth(1).unwrap()
}

/// A name, what follows the last whole line of the log and of the marks, and the two cut rows expected.
type CutCase = (
    &'static str,
    &'static [u8],
    &'static [u8],
    &'static str,
    &'static str,
);

#[test]
fn a_capture_killed_while_writing_is_analysed_without_its_cut_line() {
    let home = tempfile::tempdir().unwrap();
    let whole_log = log_text(&synth(64_000, 480, 4, 31, 0, SEED));
    let cases: [CutCase; 8] = [
        ("whole", b"", b"", "0", "0"),
        ("cut-odd", b"31000000000 < 040004005", b"", "1", "0"),
        ("cut-even", b"31000000000 < 0400040058", b"", "1", "0"),
        ("cut-stamp", b"310", b"", "1", "0"),
        ("cut-mark", b"", b"64000000000\ten", "0", "1"),
        ("cut-both", b"31000000000 <", b"64", "1", "1"),
        (
            "cut-in-a-character",
            b"",
            b"64000000000\tcue\tSay caf\xc3",
            "0",
            "1",
        ),
        ("cut-not-text", b"31000000000 < 04\xc3", b"", "1", "0"),
    ];
    for (name, log_tail, marks_tail, log_cut, marks_cut) in cases {
        let log = [whole_log.as_bytes(), log_tail].concat();
        let marks = [MARKS.as_bytes(), marks_tail].concat();
        let files: [(&str, &[u8]); 3] = [
            (LOG_FILE, &log),
            (MARKS_FILE, &marks),
            (META_FILE, META.as_bytes()),
        ];
        let table = analyse_run(&run_with(&home, name, &files)).unwrap();
        assert_eq!(value_of(&table, "sdu_log_cut_lines"), log_cut, "{name}");
        assert_eq!(value_of(&table, "marks_cut_lines"), marks_cut, "{name}");
        let packets = SHORT_SDUS.to_string();
        assert_eq!(value_of(&table, "packets_from_pods"), packets, "{name}");
        assert_eq!(value_of(&table, "uplink_cut_short"), "0", "{name}");
        assert_eq!(value_of(&table, "clock_hz"), "64000.000", "{name}");
    }

    let bad_line_before_the_cut = format!("{whole_log}31000000000 < 0\n31000000001 < 0");
    let files: [(&str, &[u8]); 3] = [
        (LOG_FILE, bad_line_before_the_cut.as_bytes()),
        (MARKS_FILE, MARKS.as_bytes()),
        (META_FILE, META.as_bytes()),
    ];
    let error = analyse_run(&run_with(&home, "bad-then-cut", &files)).unwrap_err();
    let expected = InputError::Line {
        file: LOG_FILE,
        line: SHORT_SDUS + 1,
        rule: LineRule::Log(pods_proto::sdulog::LogError::BadHex { byte: 0 }),
    };
    assert!(
        matches!(&error, AnalyseError::Input(found) if *found == expected),
        "{error}"
    );
}

#[test]
fn a_file_that_is_not_text_is_refused_by_name() {
    let home = tempfile::tempdir().unwrap();
    let files: [(&str, &[u8]); 3] = [
        (LOG_FILE, b"1 < 00\n"),
        (MARKS_FILE, b"0\tconnect\t\n1\t\xff\n"),
        (META_FILE, META.as_bytes()),
    ];
    let run = run_with(&home, "binary", &files);
    let error = analyse_run(&run).unwrap_err();
    assert_eq!(error.to_string(), "marks.tsv line 2: it is not UTF-8 text");
}

#[test]
fn a_missing_file_or_a_path_that_is_no_run_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let files: [(&str, &[u8]); 2] = [(LOG_FILE, b"1 < 00\n"), (META_FILE, META.as_bytes())];
    let run = run_with(&home, "no-marks", &files);
    let error = analyse_run(&run).unwrap_err();
    assert!(
        matches!(error, AnalyseError::Store(StoreError::Io { .. })),
        "{error}"
    );
    assert!(error.to_string().contains("marks.tsv"), "{error}");
    assert_eq!(names_in(&run), ["meta.txt", "sdu.log"]);

    let error = analyse_run(home.path()).unwrap_err();
    assert!(
        matches!(error, AnalyseError::Store(StoreError::NotARun { .. })),
        "{error}"
    );
    let absent = run.with_file_name("absent");
    let error = analyse_run(&absent).unwrap_err();
    assert!(
        matches!(error, AnalyseError::Store(StoreError::Io { .. })),
        "{error}"
    );
}
