//! The built binary's `probe analyse` command against generated runs in a store under a temporary `HOME`.

#[cfg(test)]
mod support;
#[cfg(test)]
#[path = "../src/probe/analyse/synth.rs"]
mod synth;
#[cfg(test)]
#[path = "../src/probe/xorshift.rs"]
mod xorshift;

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::{Command, Output};

    use pods_proto::sdulog::{Direction, Record};
    use pods_proto::uplink::encode_audio;
    use tempfile::TempDir;

    use super::support::{
        DEVICE_INFO, OWNER_FILE, PERMISSION_BITS, PRIVATE, RATE_HZ, SAMPLES_PER_UNIT, SHORT_RUN_S,
        UNITS_PER_SDU, log_text, make_run, names_in, probe, row, run_path, soak,
    };
    use super::synth::{NS_PER_MS, OPAQUE, shows, synth};

    /// The status the command exits with when it refuses; a panic would exit with 101.
    const REFUSED: i32 = 1;
    /// Ten minutes, the length of a soak.
    const TEN_MINUTES_S: u32 = 600;
    /// The SDU taken out of the ten minute log, far from both of its ends.
    const WITHHELD_SDU: u32 = 7000;
    const MILLION: f64 = 1e6;
    /// How close the reported clock must come to the generated one, in parts per million.
    const WITHIN_PPM: f64 = 20.0;
    /// Audio SDUs with a header and no unit: over the 50 packets a lead needs, and none of them delivers a frame.
    const HEADER_ONLY_SDUS: u64 = 60;
    /// 30 ms between two of them, so all arrive in the ten seconds after the start mark that a lead reads.
    const SDU_INTERVAL_NS: u64 = 30_000_000;
    /// What a lead row says when too few packets deliver audio.
    const TOO_FEW_FOR_A_LEAD: &str = "fewer than 50 packets with a whole unit in the first 10 s";
    /// Two packets from the pods too short for an opcode, and one sent to the pods, which no count from the pods reads.
    const NO_OPCODE_LOG: &str = "1 < 01\n2 < 010004\n3 > 040004000600\n";

    /// The SDU the lateness test holds back, far from both ends of the short run.
    const LATE_SDU: usize = 500;
    /// How long it is held back: over one interval, so the SDU due meanwhile lands with it, 10 ms late.
    const HELD_BACK_MS: u64 = 40;
    /// A peer 312 parts per million slow: the nearest whole rate past 300.
    const SLOW_HZ: u32 = 63_980;
    /// Arrivals with no delay at all, so every lateness in the table is one the test planted.
    const NO_JITTER_MS: u32 = 0;
    const SEED: u64 = 1;
    /// A clock 312 parts per million slow drifts about 0.3 ms over one window of about a second, so a lateness of 1 ms is past it.
    const WINDOW_DRIFT_LIMIT_MS: f64 = 1.0;

    /// With the signal ignored a write past the cap fails as it does on a full disk; `$0` is the command, `$1` the run, `$2` the cap.
    const NO_ROOM: &str = r#"trap '' XFSZ; ulimit -f "$2"; exec "$0" probe analyse "$1""#;
    /// The block `ulimit -f` counts in, in bytes.
    const BLOCK_BYTES: u64 = 512;
    /// One block is under the analysis table of the short run, so the first file written does not fit.
    const UNDER_BOTH_TABLES_BLOCKS: u64 = 1;
    /// 64 blocks are over the analysis table of the short run and under its units table, so only the second file does not fit.
    const BETWEEN_THE_TABLES_BLOCKS: u64 = 64;

    /// The analyse command on `run`, in a shell that caps a written file at `blocks` and ignores the signal sent for passing the cap.
    fn analyse_with_room_for(blocks: u64, home: &TempDir, run: &Path) -> Output {
        Command::new("sh")
            .args(["-c", NO_ROOM, env!("CARGO_BIN_EXE_xtask")])
            .arg(run)
            .arg(blocks.to_string())
            .env("HOME", home.path())
            .output()
            .unwrap()
    }

    #[test]
    fn analyse_reports_the_rate_a_ten_minute_log_was_generated_at() {
        let home = tempfile::tempdir().unwrap();
        for (name, rate_hz) in [
            ("soak-64000", 64_000_u32),
            ("soak-48000", 48_000),
            ("soak-63990", 63_990),
        ] {
            let records = soak(rate_hz, TEN_MINUTES_S);
            let run = make_run(&home, name, &log_text(&records));
            let done = probe(&home, "analyse", &run).output().unwrap();
            assert!(done.status.success(), "{done:?}");
            assert_eq!(String::from_utf8(done.stderr).unwrap(), "");
            let table = String::from_utf8(done.stdout).unwrap();
            assert_eq!(fs::read_to_string(run.join("analysis.tsv")).unwrap(), table);

            let (hz, note) = row(&table, "clock_hz");
            let hz: f64 = hz.parse().unwrap();
            let apart_ppm = ((hz / f64::from(rate_hz) - 1.0) * MILLION).abs();
            assert!(apart_ppm < WITHIN_PPM, "{name}: {hz}");
            assert!(!note.split("; ").any(|item| item == "gap"), "{note}");
            assert_eq!(row(&table, "gaps").0, "0");
            assert_eq!(row(&table, "audio_sdus").0, records.len().to_string());
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
        }
    }

    #[test]
    fn one_withheld_sdu_shows_as_one_gap() {
        let home = tempfile::tempdir().unwrap();
        let mut records = soak(RATE_HZ, TEN_MINUTES_S);
        let whole = make_run(&home, "whole", &log_text(&records));
        records.remove(usize::try_from(WITHHELD_SDU).unwrap());
        let first_stamp = WITHHELD_SDU * UNITS_PER_SDU * SAMPLES_PER_UNIT;
        let last_stamp = first_stamp + (UNITS_PER_SDU - 1) * SAMPLES_PER_UNIT;
        let withheld = make_run(&home, "withheld", &log_text(&records));

        let table =
            String::from_utf8(probe(&home, "analyse", &whole).output().unwrap().stdout).unwrap();
        assert_eq!(row(&table, "gaps").0, "0");
        assert!(!table.contains("\ngap\t"), "{table}");

        let table =
            String::from_utf8(probe(&home, "analyse", &withheld).output().unwrap().stdout).unwrap();
        assert_eq!(row(&table, "gaps").0, "1");
        let (_, note) = row(&table, "gap");
        let missing = format!("stamps {first_stamp} to {last_stamp} missing");
        assert_eq!(missing, "stamps 13440000 to 13441440 missing");
        assert!(note.contains(&missing), "{note}");
        let (_, note) = row(&table, "clock_hz");
        assert!(note.split("; ").any(|item| item == "gap"), "{note}");
    }

    #[test]
    fn a_garbage_or_empty_log_is_refused_and_no_number_is_printed() {
        let home = tempfile::tempdir().unwrap();
        let garbage = make_run(&home, "garbage", "1 < 00\n2 < 01\n3 < AA:BB:CC:DD:EE:FF\n");
        let refused = probe(&home, "analyse", &garbage).output().unwrap();
        assert!(!refused.status.success(), "{refused:?}");
        assert_eq!(refused.stdout, b"");
        let error = String::from_utf8(refused.stderr).unwrap();
        assert!(error.contains("sdu.log line 3: "), "{error}");
        assert!(!error.contains("AA:BB"), "{error}");
        assert_eq!(names_in(&garbage), ["marks.tsv", "meta.txt", "sdu.log"]);

        let empty = make_run(&home, "empty", "");
        let refused = probe(&home, "analyse", &empty).output().unwrap();
        assert!(!refused.status.success(), "{refused:?}");
        assert_eq!(refused.stdout, b"");
        let error = String::from_utf8(refused.stderr).unwrap();
        assert!(error.contains("sdu.log holds no packet"), "{error}");
        assert_eq!(names_in(&empty), ["marks.tsv", "meta.txt", "sdu.log"]);
    }

    #[test]
    fn audio_packets_that_hold_no_unit_give_no_lead() {
        let home = tempfile::tempdir().unwrap();
        let header_only = encode_audio(&OPAQUE, &[]).unwrap();
        let records: Vec<Record> = (1..=HEADER_ONLY_SDUS)
            .map(|k| Record {
                at_ns: k * SDU_INTERVAL_NS,
                direction: Direction::FromPods,
                sdu: header_only.clone(),
            })
            .collect();
        let run = make_run(&home, "header-only", &log_text(&records));

        let done = probe(&home, "analyse", &run).output().unwrap();
        assert!(done.status.success(), "{done:?}");
        let table = String::from_utf8(done.stdout).unwrap();
        assert_eq!(row(&table, "audio_sdus").0, HEADER_ONLY_SDUS.to_string());
        assert_eq!(
            row(&table, "segment_audio_sdus").0,
            HEADER_ONLY_SDUS.to_string()
        );
        for key in ["lead_ms", "burst_packets", "burst_frames", "burst_ms"] {
            let (value, note) = row(&table, key);
            assert_eq!(value, "refused", "{key}");
            assert!(
                note.split("; ").any(|item| item == TOO_FEW_FOR_A_LEAD),
                "{key}: {note}"
            );
        }
    }

    #[test]
    fn the_largest_packet_row_names_an_opcode_or_says_none() {
        let home = tempfile::tempdir().unwrap();
        let records = soak(RATE_HZ, SHORT_RUN_S);
        let longest = records.iter().map(|record| record.sdu.len()).max().unwrap();
        let run = make_run(&home, "with-opcodes", &log_text(&records));
        let table =
            String::from_utf8(probe(&home, "analyse", &run).output().unwrap().stdout).unwrap();
        let largest = format!("\nlargest_packet\t{longest}\tbytes\topcode 0x0058\n");
        assert!(table.contains(&largest), "{table}");

        let run = make_run(&home, "no-opcode", NO_OPCODE_LOG);
        let done = probe(&home, "analyse", &run).output().unwrap();
        assert!(done.status.success(), "{done:?}");
        let table = String::from_utf8(done.stdout).unwrap();
        let none = "\nlargest_packet\tnone\tbytes\tno packet from the pods carries an opcode\n";
        assert!(table.contains(none), "{table}");
        assert_eq!(row(&table, "packets_without_opcode").0, "2");
    }

    #[test]
    fn a_table_that_cannot_be_written_whole_leaves_no_table_in_the_run() {
        let home = tempfile::tempdir().unwrap();
        let log = log_text(&soak(RATE_HZ, SHORT_RUN_S));
        for (name, blocks, file) in [
            ("no-room", UNDER_BOTH_TABLES_BLOCKS, "/analysis.tsv: "),
            (
                "no-room-for-units",
                BETWEEN_THE_TABLES_BLOCKS,
                "/units.tsv: ",
            ),
        ] {
            let run = make_run(&home, name, &log);
            let refused = analyse_with_room_for(blocks, &home, &run);
            assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
            assert_eq!(refused.stdout, b"");
            let error = String::from_utf8(refused.stderr).unwrap();
            assert!(error.contains("could not write "), "{error}");
            assert!(error.contains(file), "{error}");
            assert_eq!(names_in(&run), ["marks.tsv", "meta.txt", "sdu.log"]);

            let done = probe(&home, "analyse", &run).output().unwrap();
            assert!(done.status.success(), "{done:?}");
            let table = String::from_utf8(done.stdout).unwrap();
            assert_eq!(fs::read_to_string(run.join("analysis.tsv")).unwrap(), table);
        }

        let run = run_path(&home, "no-room-for-units");
        let cap_bytes = BETWEEN_THE_TABLES_BLOCKS * BLOCK_BYTES;
        assert!(fs::metadata(run.join("analysis.tsv")).unwrap().len() < cap_bytes);
        assert!(fs::metadata(run.join("units.tsv")).unwrap().len() > cap_bytes);
    }

    #[test]
    fn device_strings_reach_their_own_file_and_neither_output() {
        let home = tempfile::tempdir().unwrap();
        let mut records = vec![Record {
            at_ns: 1,
            direction: Direction::FromPods,
            sdu: DEVICE_INFO.to_vec(),
        }];
        records.extend(soak(RATE_HZ, SHORT_RUN_S));
        let run = make_run(&home, "device", &log_text(&records));

        let done = probe(&home, "analyse", &run).output().unwrap();
        assert!(done.status.success(), "{done:?}");
        assert_eq!(String::from_utf8(done.stderr).unwrap(), "");
        let table = String::from_utf8(done.stdout).unwrap();
        let units = fs::read_to_string(run.join("units.tsv")).unwrap();
        assert_eq!(row(&table, "device_info_packets").0, "1");
        for private in PRIVATE {
            assert!(!shows(&table, private), "{table}");
            assert!(!shows(&units, private), "{units}");
        }
        let device_file = run.join("device-info.txt");
        assert_eq!(
            fs::read_to_string(&device_file).unwrap(),
            "Probe Pods\nSERIAL0XYZ\n"
        );
        let mode = fs::metadata(&device_file).unwrap().permissions().mode();
        assert_eq!(mode & PERMISSION_BITS, OWNER_FILE);

        let again = probe(&home, "analyse", &run).output().unwrap();
        assert_eq!(again.status.code(), Some(REFUSED), "{again:?}");
        assert_eq!(again.stdout, b"");
        let error = String::from_utf8(again.stderr).unwrap();
        assert!(error.contains("/analysis.tsv"), "{error}");
        for private in PRIVATE {
            assert!(!shows(&error, private), "{error}");
        }
    }

    #[test]
    fn analyse_takes_one_run_directory_and_no_other_argument() {
        let home = tempfile::tempdir().unwrap();
        let none = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["probe", "analyse"])
            .env("HOME", home.path())
            .output()
            .unwrap();
        assert_eq!(none.status.code(), Some(REFUSED), "{none:?}");
        assert_eq!(none.stdout, b"");
        let error = String::from_utf8(none.stderr).unwrap();
        assert!(
            error.contains("required arguments were not provided"),
            "{error}"
        );

        let run = make_run(
            &home,
            "two-arguments",
            &log_text(&soak(RATE_HZ, SHORT_RUN_S)),
        );
        let two = probe(&home, "analyse", &run)
            .arg("second")
            .output()
            .unwrap();
        assert_eq!(two.status.code(), Some(REFUSED), "{two:?}");
        assert_eq!(two.stdout, b"");
        let error = String::from_utf8(two.stderr).unwrap();
        assert!(error.contains("unexpected argument"), "{error}");
        assert_eq!(names_in(&run), ["marks.tsv", "meta.txt", "sdu.log"]);

        let empty = tempfile::tempdir().unwrap();
        let not_a_run = probe(&empty, "analyse", empty.path()).output().unwrap();
        assert_eq!(not_a_run.status.code(), Some(REFUSED), "{not_a_run:?}");
        assert_eq!(not_a_run.stdout, b"");
        let error = String::from_utf8(not_a_run.stderr).unwrap();
        assert!(error.contains("is not a run directory"), "{error}");
        assert_eq!(fs::read_dir(empty.path()).unwrap().count(), 0);
    }

    /// The analysis table of a run that holds `records`.
    fn table_of(name: &str, records: &[Record]) -> String {
        let home = tempfile::tempdir().unwrap();
        let run = make_run(&home, name, &log_text(records));
        let done = probe(&home, "analyse", &run).output().unwrap();
        assert!(done.status.success(), "{done:?}");
        String::from_utf8(done.stdout).unwrap()
    }

    #[test]
    fn the_lateness_table_finds_one_planted_late_packet() {
        let mut records = synth(
            RATE_HZ,
            SAMPLES_PER_UNIT,
            UNITS_PER_SDU,
            SHORT_RUN_S,
            NO_JITTER_MS,
            SEED,
        );
        let landed_ns = records[LATE_SDU].at_ns + HELD_BACK_MS * NS_PER_MS;
        records[LATE_SDU].at_ns = landed_ns;
        records[LATE_SDU + 1].at_ns = landed_ns;
        let table = table_of("late", &records);
        let value = |key: &str| row(&table, key).0.to_owned();

        assert_eq!(value("lateness_sdus"), records.len().to_string());
        assert_eq!(value("lateness_max_ms"), format!("{HELD_BACK_MS}.000"));
        assert_eq!(value("lateness_p99_ms"), "0.000");
        assert_eq!(value("lateness_p99_99_ms"), value("lateness_max_ms"));
        assert_eq!(value("lateness_over_30_ms"), "1");
        assert_eq!(value("lateness_over_35_ms"), "1");
        assert_eq!(value("lateness_over_45_ms"), "0");
        assert_eq!(value("lateness_bin_40_ms"), "1");
        assert_eq!(value("lateness_bin_10_ms"), "1");
        let on_time = records.len() - 2;
        assert_eq!(value("lateness_bin_0_ms"), on_time.to_string());
    }

    #[test]
    fn a_slow_clock_alone_is_no_lateness() {
        let records = synth(
            SLOW_HZ,
            SAMPLES_PER_UNIT,
            UNITS_PER_SDU,
            TEN_MINUTES_S,
            NO_JITTER_MS,
            SEED,
        );
        let table = table_of("slow", &records);
        assert_eq!(row(&table, "lateness_sdus").0, records.len().to_string());
        let largest: f64 = row(&table, "lateness_max_ms").0.parse().unwrap();
        assert!(
            largest > 0.0 && largest < WINDOW_DRIFT_LIMIT_MS,
            "{largest}"
        );
        assert_eq!(row(&table, "lateness_over_30_ms").0, "0");
    }
}
