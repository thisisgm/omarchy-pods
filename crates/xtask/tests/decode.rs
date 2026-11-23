//! The built binary's `probe decode` command on generated runs in a store under a temporary `HOME`.

#[cfg(test)]
#[path = "../src/probe/fixture.rs"]
mod fixture;
#[cfg(test)]
#[path = "../src/probe/decode/refused.rs"]
mod refused;
#[cfg(test)]
mod support;
#[cfg(test)]
#[path = "../src/probe/analyse/synth.rs"]
mod synth;
#[cfg(test)]
#[path = "support/units.rs"]
mod units;
#[cfg(test)]
#[path = "../src/probe/xorshift.rs"]
mod xorshift;

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::env;
    use std::ffi::OsString;
    use std::fs::{self, OpenOptions, Permissions};
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Output;

    use pods_proto::sdulog::{Direction, Record};
    use pods_samples::uplink;
    use tempfile::TempDir;

    use super::fixture::{TONE_UNIT, ffprobe, installed};
    use super::refused::{REFUSED_MESSAGE, REFUSED_UNIT};
    use super::support::{
        DEVICE_INFO, OWNER_FILE, PERMISSION_BITS, PRIVATE, RATE_HZ, SAMPLES_PER_UNIT, SHORT_RUN_S,
        UNITS_PER_SDU, log_text, make_run, names_in, probe, row, run_path, soak,
    };
    use super::synth::{NS_PER_MS, SILENT_UNIT, shows};
    use super::units::with_units;
    use super::xorshift::next_random;

    /// The status the command exits with when it refuses; a panic would exit with 101.
    const REFUSED: i32 = 1;
    /// The units of the short run at 64 kHz: 31 s of whole SDUs.
    const UNITS: u32 = SHORT_RUN_S * RATE_HZ / (SAMPLES_PER_UNIT * UNITS_PER_SDU) * UNITS_PER_SDU;
    const SAMPLES: u32 = UNITS * SAMPLES_PER_UNIT;
    /// The files a run holds once it is analysed, the device strings among them.
    const ANALYSED: [&str; 6] = [
        "analysis.tsv",
        "device-info.txt",
        "marks.tsv",
        "meta.txt",
        "sdu.log",
        "units.tsv",
    ];
    const DECODED: [&str; 5] = [
        "decode.tsv",
        "eld.mp4",
        "ffmpeg.f32",
        "ffmpeg.stderr",
        "pods.f32.wav",
    ];
    /// What ffprobe is asked for about the media file.
    const SHOW_MEDIA: &str = "stream=codec_name,profile,channels,nb_read_packets";
    /// What ffprobe is asked for about the WAV file.
    const SHOW_WAV: &str = "stream=codec_name,sample_rate,channels,duration_ts";
    const MS_PER_SECOND: u32 = 1000;
    /// The time between two SDUs of the generated stream at 64 kHz.
    const ONE_SDU_MS: u32 = UNITS_PER_SDU * SAMPLES_PER_UNIT * MS_PER_SECOND / RATE_HZ;
    const MAX_UNITS: u32 = 160_000;
    /// A second over the time that many units last at 64 kHz.
    const TOO_LONG_S: u32 = MAX_UNITS * SAMPLES_PER_UNIT / RATE_HZ + 1;
    /// The rate the configuration declares, and a clock within one percent of neither rate.
    const DECLARED_RATE_HZ: u32 = 48_000;
    const NEITHER_RATE_HZ: u32 = 56_000;
    /// How near the measured clock of a generated run is to the rate it was generated at.
    const CLOCK_WITHIN_HZ: f64 = 1.0;
    /// The mutations the last experiment makes.
    const MUTATIONS: u32 = 10_000;
    /// The bytes of one sample the oracle writes.
    const SAMPLE_BYTES: u32 = 4;
    /// The most two samples may differ by and still agree, in steps of a 16 bit sample.
    const TOLERANCE_LSB: f64 = 1.0;
    /// A peak no silence has: the tone unit decodes to more than twice this.
    const LOUD_LSB: f64 = 1000.0;
    /// The furthest two decodes may differ in length and still be compared, in units.
    const SEARCH_UNITS: u32 = 3;
    /// A unit that decodes to silence with or without its first byte.
    const LONG_SILENT_UNIT: [u8; 3] = [0x00, 0x00, 0x00];
    /// The frames after a frame that its samples still reach: the window of this coder is four frames long.
    const WINDOW_REACH_FRAMES: f64 = 3.0;
    /// The seed of the run that mixes tone and silence; under it the units around the spliced one are not all alike, so the splice shows.
    const MIX_SEED: u64 = 0x5eed_0008;
    /// The samples one stand-in leaves out: fewer than a unit holds, so what it writes is no whole number of units.
    const RAGGED_SAMPLES: u32 = 100;
    /// The unit of one run the candidate refuses, counted from 0: any but the spliced one would do.
    const REFUSED_AT: u32 = 10;
    /// The mode of a stand-in for FFmpeg: its owner may run it.
    const RUNNABLE: u32 = 0o700;
    const STAND_IN_LINE: &str = "a line from the stand-in";
    /// The offset basis and the prime of the 64 bit FNV-1a hash.
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    /// The hash of the WAV file the probe wrote for the mixed run while the decoder was the probe's own.
    const PINNED_WAV: u64 = 0x3c5f_47e3_b4fe_df7c;

    /// A short run clocked at `rate_hz` that opens with a device information packet and holds what `unit_of` gives as each unit in turn, analysed.
    fn analysed_units<'unit>(
        home: &TempDir,
        name: &str,
        rate_hz: u32,
        unit_of: impl FnMut() -> &'unit [u8],
    ) -> PathBuf {
        let mut records = vec![Record {
            at_ns: 1,
            direction: Direction::FromPods,
            sdu: DEVICE_INFO.to_vec(),
        }];
        records.extend(with_units(&soak(rate_hz, SHORT_RUN_S), unit_of));
        let run = make_run(home, name, &log_text(&records));
        let analysed = probe(home, "analyse", &run).output().unwrap();
        assert!(analysed.status.success(), "{analysed:?}");
        run
    }

    /// Such a run with `unit` as every unit.
    fn analysed_run(home: &TempDir, name: &str, rate_hz: u32, unit: &[u8]) -> PathBuf {
        analysed_units(home, name, rate_hz, || unit)
    }

    /// Such a run whose units are the tone or silence, as a generator seeded `MIX_SEED` draws them.
    fn mixed_run(home: &TempDir, name: &str) -> PathBuf {
        let mut random = MIX_SEED;
        analysed_units(home, name, RATE_HZ, || {
            if next_random(&mut random).is_multiple_of(2) {
                &TONE_UNIT[..]
            } else {
                &SILENT_UNIT[..]
            }
        })
    }

    fn fnv1a(bytes: &[u8]) -> u64 {
        bytes.iter().fold(FNV_OFFSET, |hash, &byte| {
            (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
        })
    }

    /// What every file a decode adds holds.
    fn decoded_files(run: &Path) -> Vec<Vec<u8>> {
        DECODED
            .iter()
            .map(|file| fs::read(run.join(file)).unwrap())
            .collect()
    }

    fn every_name() -> Vec<&'static str> {
        let mut names = [ANALYSED.as_slice(), &DECODED].concat();
        names.sort_unstable();
        names
    }

    /// The table of a decode that succeeded, which is also what it left in the run.
    fn table_of(decoded: &Output, run: &Path) -> String {
        assert!(decoded.status.success(), "{decoded:?}");
        let table = String::from_utf8(decoded.stdout.clone()).unwrap();
        assert_eq!(fs::read_to_string(run.join("decode.tsv")).unwrap(), table);
        assert_eq!(names_in(run), every_name());
        table
    }

    fn number(table: &str, key: &str) -> f64 {
        row(table, key).0.parse().unwrap()
    }

    /// A directory that holds one program named ffmpeg: a shell script that writes `bytes` zero bytes, then does `then`.
    fn stand_in(bytes: u32, then: &str) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("ffmpeg");
        fs::write(
            &script,
            format!("#!/bin/sh\nhead -c {bytes} /dev/zero\n{then}\n"),
        )
        .unwrap();
        fs::set_permissions(&script, Permissions::from_mode(RUNNABLE)).unwrap();
        dir
    }

    /// `dir` ahead of the path the tests run with, so a program in it is found first.
    fn path_with(dir: &Path) -> OsString {
        let rest = env::var_os("PATH").unwrap();
        env::join_paths(
            [dir.to_path_buf()]
                .into_iter()
                .chain(env::split_paths(&rest)),
        )
        .unwrap()
    }

    #[test]
    fn decode_of_a_silent_run_agrees_and_names_every_file() {
        if !(installed("ffmpeg") && installed("ffprobe")) {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let run = analysed_run(&home, "silent", RATE_HZ, &SILENT_UNIT);
        assert_eq!(names_in(&run), ANALYSED);

        let decoded = probe(&home, "decode", &run).output().unwrap();
        assert_eq!(String::from_utf8(decoded.stderr.clone()).unwrap(), "");
        let table = table_of(&decoded, &run);
        for private in PRIVATE {
            assert!(!shows(&table, private), "{table}");
        }
        let units = UNITS.to_string();
        let samples = SAMPLES.to_string();
        for (key, value) in [
            ("units_in", units.as_str()),
            ("rate_label", &RATE_HZ.to_string()),
            ("candidate_config", "accepted"),
            ("candidate_decoded", &units),
            ("candidate_errors", "0"),
            ("candidate_panics", "0"),
            ("candidate_samples", &samples),
            ("candidate_peak_lsb", "0.000"),
            ("oracle_refused", "0"),
            ("oracle_samples", &samples),
            ("oracle_peak_lsb", "0.000"),
            ("frames_compared", &units),
            ("offset_units", "0"),
            ("max_lsb", "0.000"),
            ("frames_over_one_lsb", "0"),
            ("splice_candidate_frames_changed", "0"),
            ("splice_oracle_frames_changed", "0"),
            ("mutations", &MUTATIONS.to_string()),
            ("mutations_panics", "0"),
        ] {
            assert_eq!(row(&table, key).0, value, "{key}");
        }
        let outcomes = ["mutations_decoded", "mutations_errors", "mutations_panics"];
        let counted: f64 = outcomes.iter().map(|key| number(&table, key)).sum();
        assert_eq!(counted, f64::from(MUTATIONS));
        assert!(number(&table, "candidate_decode_us_mean") > 0.0);
        assert!(
            number(&table, "candidate_decode_us_max") >= number(&table, "candidate_decode_us_mean")
        );
        assert!(
            row(&table, "candidate_decode_us_mean")
                .1
                .contains(&format!("{units} calls"))
        );

        for file in DECODED {
            let mode = fs::metadata(run.join(file)).unwrap().permissions().mode();
            assert_eq!(mode & PERMISSION_BITS, OWNER_FILE, "{file}");
        }
        assert_eq!(fs::read(run.join("ffmpeg.stderr")).unwrap(), b"");
        let from_oracle = fs::metadata(run.join("ffmpeg.f32")).unwrap().len();
        assert_eq!(from_oracle, u64::from(SAMPLES * SAMPLE_BYTES));
        assert_eq!(
            ffprobe(&run.join("eld.mp4"), SHOW_MEDIA),
            format!("codec_name=aac\nprofile=ELD\nchannels=1\nnb_read_packets={units}\n")
        );
        assert_eq!(
            ffprobe(&run.join("pods.f32.wav"), SHOW_WAV),
            format!(
                "codec_name=pcm_f32le\nsample_rate={RATE_HZ}\nchannels=1\nduration_ts={samples}\n"
            )
        );

        let before = decoded_files(&run);
        let again = probe(&home, "decode", &run).output().unwrap();
        assert_eq!(again.status.code(), Some(REFUSED), "{again:?}");
        assert_eq!(again.stdout, b"");
        let error = String::from_utf8(again.stderr).unwrap();
        assert!(error.contains("/eld.mp4 already exists"), "{error}");
        assert_eq!(names_in(&run), every_name());
        assert!(
            decoded_files(&run) == before,
            "the refused decode changed a file"
        );
    }

    #[test]
    fn decode_of_a_run_that_is_not_silence_agrees_and_its_control_discriminates() {
        if !installed("ffmpeg") {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let run = mixed_run(&home, "mixed");
        let table = table_of(&probe(&home, "decode", &run).output().unwrap(), &run);

        let units = UNITS.to_string();
        for (key, value) in [
            ("candidate_decoded", units.as_str()),
            ("candidate_errors", "0"),
            ("oracle_refused", "0"),
            ("frames_compared", &units),
            ("offset_units", "0"),
            ("frames_over_one_lsb", "0"),
            ("first_byte_removed_oracle_refused", "1"),
            ("first_byte_removed_discriminates", "yes"),
        ] {
            assert_eq!(row(&table, key).0, value, "{key}");
        }
        assert!(number(&table, "candidate_peak_lsb") > LOUD_LSB, "{table}");
        assert!(number(&table, "oracle_peak_lsb") > LOUD_LSB, "{table}");
        assert!(number(&table, "max_lsb") <= TOLERANCE_LSB, "{table}");
        // Taking a unit out of a stream that does not repeat changes the frames whose window reaches over the splice, and no other.
        for decoder in ["candidate", "oracle"] {
            let changed = number(&table, &format!("splice_{decoder}_frames_changed"));
            assert!(
                (1.0..=WINDOW_REACH_FRAMES).contains(&changed),
                "{decoder}: {table}"
            );
            assert!(number(&table, &format!("splice_{decoder}_step_lsb")) > 0.0);
        }
    }

    #[test]
    fn the_probe_decodes_to_the_same_samples_after_the_move() {
        let home = tempfile::tempdir().unwrap();
        let run = mixed_run(&home, "pinned");
        // A stand-in for FFmpeg, so the pin holds where FFmpeg is not installed; what it writes is not pinned.
        let oracle = stand_in(SAMPLES * SAMPLE_BYTES, "exit 0");
        let mut decode = probe(&home, "decode", &run);
        decode.env("PATH", path_with(oracle.path()));
        let table = table_of(&decode.output().unwrap(), &run);
        assert_eq!(row(&table, "candidate_decoded").0, UNITS.to_string());
        assert!(number(&table, "candidate_peak_lsb") > LOUD_LSB, "{table}");
        let wav = fnv1a(&fs::read(run.join("pods.f32.wav")).unwrap());
        assert_eq!(wav, PINNED_WAV, "the hash is {wav:#018x}");
    }

    #[test]
    fn the_sample_units_are_the_units_of_the_pin() {
        // The pin's tone holds its one line at the last place of the first band, under scale factor 0xb4.
        assert_eq!(uplink::tone_unit(3, 0xb4), TONE_UNIT);
        assert_eq!(uplink::silent_unit(), SILENT_UNIT);
    }

    #[test]
    fn a_control_that_shows_nothing_says_the_comparison_proves_nothing() {
        if !installed("ffmpeg") {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let run = analysed_run(&home, "long-silent", RATE_HZ, &LONG_SILENT_UNIT);
        let table = table_of(&probe(&home, "decode", &run).output().unwrap(), &run);
        let units = UNITS.to_string();
        for (key, value) in [
            ("frames_compared", units.as_str()),
            ("first_byte_removed_candidate_decoded", &units),
            ("first_byte_removed_oracle_refused", "0"),
            ("first_byte_removed_candidate_frames_changed", "0"),
            ("first_byte_removed_oracle_frames_changed", "0"),
            ("first_byte_removed_discriminates", "no"),
        ] {
            assert_eq!(row(&table, key).0, value, "{key}");
        }
    }

    #[test]
    fn the_wav_is_labelled_48000_when_the_clock_is() {
        if !(installed("ffmpeg") && installed("ffprobe")) {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let run = analysed_run(&home, "slow", DECLARED_RATE_HZ, &SILENT_UNIT);
        let table = table_of(&probe(&home, "decode", &run).output().unwrap(), &run);
        assert_eq!(row(&table, "rate_label").0, DECLARED_RATE_HZ.to_string());
        let labelled = ffprobe(&run.join("pods.f32.wav"), SHOW_WAV);
        let rate = format!("\nsample_rate={DECLARED_RATE_HZ}\n");
        assert!(labelled.contains(&rate), "{labelled}");
    }

    #[test]
    fn decode_refuses_a_run_whose_clock_is_neither_rate() {
        let home = tempfile::tempdir().unwrap();
        let run = analysed_run(&home, "odd", NEITHER_RATE_HZ, &SILENT_UNIT);

        let refused = probe(&home, "decode", &run).output().unwrap();
        assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
        assert_eq!(refused.stdout, b"");
        let error = String::from_utf8(refused.stderr).unwrap();
        // Sample input: `xtask probe: clock_hz 55999.871 is within 1 percent of neither 64000 Hz nor 48000 Hz, so ...`.
        let (_, measured) = error.split_once("clock_hz ").unwrap();
        let hz: f64 = measured.split(' ').next().unwrap().parse().unwrap();
        assert!(
            (hz - f64::from(NEITHER_RATE_HZ)).abs() < CLOCK_WITHIN_HZ,
            "{error}"
        );
        let neither = format!("within 1 percent of neither {RATE_HZ} Hz nor {DECLARED_RATE_HZ} Hz");
        assert!(error.contains(&neither), "{error}");
        assert_eq!(names_in(&run), ANALYSED);
    }

    #[test]
    fn decode_refuses_an_analysis_that_counts_other_units_than_the_log_holds() {
        let home = tempfile::tempdir().unwrap();
        let run = analysed_run(&home, "grown", RATE_HZ, &SILENT_UNIT);
        let mut late = soak(RATE_HZ, SHORT_RUN_S).pop().unwrap();
        late.at_ns += u64::from(ONE_SDU_MS) * NS_PER_MS;
        let mut log = OpenOptions::new()
            .append(true)
            .open(run.join("sdu.log"))
            .unwrap();
        log.write_all(log_text(&[late]).as_bytes()).unwrap();

        let refused = probe(&home, "decode", &run).output().unwrap();
        assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
        assert_eq!(refused.stdout, b"");
        let error = String::from_utf8(refused.stderr).unwrap();
        let grown = UNITS + UNITS_PER_SDU;
        let sentence = format!("analysis.tsv does not count the {grown} units that sdu.log holds");
        assert!(error.contains(&sentence), "{error}");
        assert_eq!(names_in(&run), ANALYSED);
    }

    #[test]
    fn decode_refuses_a_run_of_more_units_than_one_decode_takes() {
        let home = tempfile::tempdir().unwrap();
        let run = make_run(&home, "long", &log_text(&soak(RATE_HZ, TOO_LONG_S)));
        let analysed = probe(&home, "analyse", &run).output().unwrap();
        assert!(analysed.status.success(), "{analysed:?}");
        let before = names_in(&run);

        let refused = probe(&home, "decode", &run).output().unwrap();
        assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
        assert_eq!(refused.stdout, b"");
        let error = String::from_utf8(refused.stderr).unwrap();
        let sentence = format!("units, and one decode takes at most {MAX_UNITS}");
        assert!(error.contains(&sentence), "{error}");
        assert_eq!(names_in(&run), before);
    }

    #[test]
    fn an_experiment_the_oracle_refuses_says_why_in_its_own_row() {
        if !installed("ffmpeg") {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let run = analysed_run(&home, "experiment", RATE_HZ, &SILENT_UNIT);
        let table = table_of(&probe(&home, "decode", &run).output().unwrap(), &run);

        assert_eq!(row(&table, "oracle_refused").0, "0");
        let (refused, note) = row(&table, "config_64k_oracle_refused");
        assert_eq!(refused, "1");
        // The kept file holds what the whole decode said, which is nothing here, so the row must not send the reader to it.
        let said = note.split_once("lines said, first: ").map(|(_, said)| said);
        assert!(said.is_some_and(|said| !said.trim().is_empty()), "{note}");
        assert!(!note.contains("ffmpeg.stderr"), "{note}");
    }

    #[test]
    fn a_refusal_by_the_oracle_is_reported_and_its_messages_are_kept() {
        if !installed("ffmpeg") {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let run = analysed_run(&home, "refused", RATE_HZ, &REFUSED_UNIT);
        let table = table_of(&probe(&home, "decode", &run).output().unwrap(), &run);

        let (refused, note) = row(&table, "oracle_refused");
        assert_eq!(refused, "1");
        assert!(note.starts_with("exit status: "), "{note}");
        assert!(!note.starts_with("exit status: 0;"), "{note}");
        // Neither decoder decoded a unit, so no row that needs a decode reads as a number.
        assert_eq!(row(&table, "candidate_decoded").0, "0");
        assert_eq!(row(&table, "candidate_samples").0, "0");
        for key in [
            "candidate_peak_lsb",
            "oracle_samples",
            "oracle_peak_lsb",
            "frames_compared",
            "offset_units",
            "max_lsb",
            "frames_over_one_lsb",
            "first_byte_removed_candidate_frames_changed",
            "first_byte_removed_oracle_frames_changed",
            "first_byte_removed_discriminates",
            "splice_candidate_frames_changed",
            "splice_candidate_step_lsb",
            "splice_oracle_frames_changed",
            "splice_oracle_step_lsb",
        ] {
            assert_eq!(row(&table, key).0, "refused", "{key}");
        }
        let messages = fs::read_to_string(run.join("ffmpeg.stderr")).unwrap();
        assert!(messages.contains(REFUSED_MESSAGE), "{messages}");
    }

    #[test]
    fn a_failed_exit_or_a_message_of_the_oracle_is_a_refusal_whatever_it_wrote() {
        let home = tempfile::tempdir().unwrap();
        let say = format!("echo '{STAND_IN_LINE}' >&2");
        for (name, then, refused, said) in [
            ("clean", "exit 0", "0", "exited with success"),
            ("failed", "exit 3", "1", "exit status: 3; 0 lines"),
            ("said", say.as_str(), "1", "exit status: 0; 1 lines"),
        ] {
            let run = analysed_run(&home, name, RATE_HZ, &SILENT_UNIT);
            let oracle = stand_in(SAMPLES * SAMPLE_BYTES, then);
            let mut decode = probe(&home, "decode", &run);
            decode.env("PATH", path_with(oracle.path()));
            let table = table_of(&decode.output().unwrap(), &run);

            let (value, note) = row(&table, "oracle_refused");
            assert_eq!(value, refused, "{name}");
            assert!(note.contains(said), "{name}: {note}");
            // What a refused oracle wrote is no decode: its samples are not counted, measured or compared.
            let (samples, peak, compared) = if refused == "0" {
                (SAMPLES.to_string(), "0.000".to_owned(), UNITS.to_string())
            } else {
                let refused = || "refused".to_owned();
                (refused(), refused(), refused())
            };
            assert_eq!(row(&table, "oracle_samples").0, samples, "{name}");
            assert_eq!(row(&table, "oracle_peak_lsb").0, peak, "{name}");
            assert_eq!(row(&table, "frames_compared").0, compared, "{name}");
        }
        let kept = fs::read_to_string(run_path(&home, "said").join("ffmpeg.stderr")).unwrap();
        assert_eq!(kept, format!("{STAND_IN_LINE}\n"));
    }

    #[test]
    fn an_oracle_that_wrote_too_little_or_too_much_is_not_compared() {
        let home = tempfile::tempdir().unwrap();
        let frame = SAMPLES_PER_UNIT * SAMPLE_BYTES;
        let short = (UNITS - SEARCH_UNITS - 1).to_string();
        for (name, bytes, samples, why) in [
            ("nothing", 0, "0", format!("{UNITS} units against 0")),
            (
                "four-units-short",
                (UNITS - SEARCH_UNITS - 1) * frame,
                "",
                format!("{UNITS} units against {short}: more than {SEARCH_UNITS} apart"),
            ),
            (
                "ragged",
                (SAMPLES - RAGGED_SAMPLES) * SAMPLE_BYTES,
                "",
                "samples are not a whole number of units".to_owned(),
            ),
        ] {
            let run = analysed_run(&home, name, RATE_HZ, &SILENT_UNIT);
            let oracle = stand_in(bytes, "exit 0");
            let mut decode = probe(&home, "decode", &run);
            decode.env("PATH", path_with(oracle.path()));
            let table = table_of(&decode.output().unwrap(), &run);

            assert_eq!(row(&table, "oracle_refused").0, "0", "{name}");
            let written = (bytes / SAMPLE_BYTES).to_string();
            assert_eq!(row(&table, "oracle_samples").0, written, "{name}");
            if samples == "0" {
                assert_eq!(row(&table, "oracle_peak_lsb").0, "refused", "{name}");
            }
            for key in [
                "frames_compared",
                "offset_units",
                "max_lsb",
                "frames_over_one_lsb",
            ] {
                let (value, note) = row(&table, key);
                assert_eq!(value, "refused", "{name}: {key}");
                assert!(note.contains(&why), "{name}: {note}");
            }
        }

        // As far apart as an offset explains, the two are compared on the frames both hold; two units over the whole stream is three over the spliced one.
        for (name, written, compared) in [
            (
                "three-units-short",
                UNITS - SEARCH_UNITS,
                UNITS - SEARCH_UNITS,
            ),
            ("two-units-long", UNITS + SEARCH_UNITS - 1, UNITS),
        ] {
            let run = analysed_run(&home, name, RATE_HZ, &SILENT_UNIT);
            let oracle = stand_in(written * frame, "exit 0");
            let mut decode = probe(&home, "decode", &run);
            decode.env("PATH", path_with(oracle.path()));
            let table = table_of(&decode.output().unwrap(), &run);
            let samples = (written * SAMPLES_PER_UNIT).to_string();
            assert_eq!(row(&table, "oracle_samples").0, samples, "{name}");
            let compared = compared.to_string();
            assert_eq!(row(&table, "frames_compared").0, compared, "{name}");
        }

        // Bytes that are no whole samples, and a unit more than a decode of the spliced units may add, are no decode at all.
        for (name, bytes, why) in [
            (
                "cut-sample",
                SAMPLES * SAMPLE_BYTES - 1,
                "is not a whole number of samples",
            ),
            (
                "three-units-long",
                (UNITS + SEARCH_UNITS) * frame,
                "ffmpeg.f32 is larger than",
            ),
        ] {
            let run = analysed_run(&home, name, RATE_HZ, &SILENT_UNIT);
            let oracle = stand_in(bytes, "exit 0");
            let mut decode = probe(&home, "decode", &run);
            decode.env("PATH", path_with(oracle.path()));
            let refused = decode.output().unwrap();
            assert_eq!(refused.status.code(), Some(REFUSED), "{name}: {refused:?}");
            assert_eq!(refused.stdout, b"", "{name}");
            let error = String::from_utf8(refused.stderr).unwrap();
            assert!(error.contains(why), "{name}: {error}");
            assert_eq!(names_in(&run), ANALYSED, "{name}");
        }
    }

    #[test]
    fn a_unit_the_candidate_refuses_leaves_its_decode_uncompared() {
        let home = tempfile::tempdir().unwrap();
        let mut order = (0..UNITS).map(|unit| {
            if unit == REFUSED_AT {
                &REFUSED_UNIT[..]
            } else {
                &SILENT_UNIT[..]
            }
        });
        let run = analysed_units(&home, "one-refused", RATE_HZ, || order.next().unwrap());
        let oracle = stand_in(SAMPLES * SAMPLE_BYTES, "exit 0");
        let mut decode = probe(&home, "decode", &run);
        decode.env("PATH", path_with(oracle.path()));
        let table = table_of(&decode.output().unwrap(), &run);

        // The frames the candidate gave are one short of the oracle's, which an offset would explain if they were compared.
        let decoded = (UNITS - 1).to_string();
        for (key, value) in [
            ("candidate_decoded", decoded.as_str()),
            ("candidate_errors", "1"),
            ("oracle_refused", "0"),
        ] {
            assert_eq!(row(&table, key).0, value, "{key}");
        }
        // The reason is the one the chain's decoder gives, which keeps none of the library's.
        let (_, said) = row(&table, "candidate_errors");
        assert!(said.contains("the decoder refused the unit"), "{said}");
        let not_every_unit = "did not decode every unit";
        let not_whole = "one of the two decodes is not whole";
        for (key, why) in [
            ("frames_compared", not_every_unit),
            ("offset_units", not_every_unit),
            ("max_lsb", not_every_unit),
            ("frames_over_one_lsb", not_every_unit),
            ("splice_candidate_frames_changed", not_whole),
            ("splice_candidate_step_lsb", not_whole),
        ] {
            let (value, note) = row(&table, key);
            assert_eq!(value, "refused", "{key}");
            assert!(note.contains(why), "{key}: {note}");
        }
    }

    #[test]
    fn decode_without_the_oracle_or_the_analysis_writes_nothing() {
        let home = tempfile::tempdir().unwrap();
        let run = analysed_run(&home, "no-oracle", RATE_HZ, &SILENT_UNIT);
        let nowhere = tempfile::tempdir().unwrap();
        let mut decode = probe(&home, "decode", &run);
        decode.env("PATH", nowhere.path());
        let refused = decode.output().unwrap();
        assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
        assert_eq!(refused.stdout, b"");
        let error = String::from_utf8(refused.stderr).unwrap();
        assert!(error.contains("could not start ffmpeg"), "{error}");
        assert_eq!(names_in(&run), ANALYSED);

        let raw = make_run(
            &home,
            "not-analysed",
            &log_text(&soak(RATE_HZ, SHORT_RUN_S)),
        );
        let before = names_in(&raw);
        let refused = probe(&home, "decode", &raw).output().unwrap();
        assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
        assert_eq!(refused.stdout, b"");
        let error = String::from_utf8(refused.stderr).unwrap();
        assert!(error.contains("/analysis.tsv"), "{error}");
        assert_eq!(names_in(&raw), before);

        let absent = probe(&home, "decode", &run_path(&home, "absent"))
            .output()
            .unwrap();
        assert_eq!(absent.status.code(), Some(REFUSED), "{absent:?}");
        assert_eq!(absent.stdout, b"");
        let error = String::from_utf8(absent.stderr).unwrap();
        assert!(error.contains("could not open"), "{error}");
        assert!(error.contains("/absent"), "{error}");
        assert!(!run_path(&home, "absent").exists());
    }
}
