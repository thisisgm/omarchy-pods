//! The built binary's `probe audio` command on generated runs in a store under a temporary `HOME`.

#[cfg(test)]
#[path = "../src/probe/analyse/backlog.rs"]
mod backlog;
#[cfg(test)]
#[path = "support/dictation.rs"]
mod dictation;
#[cfg(test)]
#[path = "../src/probe/fixture.rs"]
mod fixture;
#[cfg(test)]
#[path = "support/oracles.rs"]
mod oracles;
#[cfg(test)]
#[path = "support/run.rs"]
mod run;
#[cfg(test)]
#[path = "../src/probe/audio/signal.rs"]
mod signal;
#[cfg(test)]
#[path = "../src/probe/analyse/synth.rs"]
mod synth;
#[cfg(test)]
#[path = "support/units.rs"]
mod units;
#[cfg(test)]
#[path = "../src/probe/wav/fixture.rs"]
mod wav;
#[cfg(test)]
#[path = "../src/probe/xorshift.rs"]
mod xorshift;

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod audio {
    mod band_edge;
    mod bounds;
    mod reference;
    mod refusals;
    mod short_starts;
    mod whole;

    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Output;

    use pods_proto::sdulog::{Direction, Record};
    use tempfile::TempDir;

    use super::run::{
        DEVICE_INFO, OWNER_FILE, PERMISSION_BITS, PRIVATE, RATE_HZ as RATE, SAMPLES_PER_UNIT,
        UNITS_PER_SDU, log_text, names_in, probe, put_file, row, rows, run_holding,
    };
    use super::signal::{
        BIN_HZ, ROOM, SEED, add, band_noise, close, noise, samples_in, sine, span,
    };
    use super::synth::{NS_PER_MS, shows, synth};
    use super::wav::{FLOAT, INTEGER, file};

    /// The status the command exits with when it refuses; a panic would exit with 101.
    const REFUSED: i32 = 1;
    /// The wall clock at the zero of every generated run.
    const REALTIME_AT_ZERO_NS: u64 = 1_763_640_000_000_000_000;
    const MS_PER_SECOND: u32 = 1000;
    /// The generated peer is never late.
    const NO_JITTER_MS: u32 = 0;
    /// The time one SDU of the generated peer holds: its units arrive together, this long after the first of their samples.
    const SDU_MS: u32 = SAMPLES_PER_UNIT * UNITS_PER_SDU * MS_PER_SECOND / RATE;
    /// The cues of the voice script, as a capture writes them.
    const TAP: &str = "Tap the left bud twice";
    const COUNT: &str = "Count one to ten slowly, then say the fox sentence";
    const QUIETLY: &str = "Say the fox sentence quietly";
    const LOUDLY: &str = "Say the fox sentence loudly";
    const SSS: &str = "Say sss for three seconds, then shh for three seconds";
    const JINGLE: &str = "Jingle your keys for five seconds";
    const SILENT: &str = "Stay silent";
    /// The cue `firstword` shows at its zero.
    const COUNT_ON: &str = "Count aloud from one and do not stop until told";

    /// The marks file: one line for each of a time in milliseconds, a kind and a text.
    fn marks(lines: &[(u32, &str, &str)]) -> String {
        let line = |&(at_ms, kind, text): &(u32, &str, &str)| {
            let at_ns = u64::from(at_ms).saturating_mul(NS_PER_MS);
            format!("{at_ns}\t{kind}\t{text}\n")
        };
        lines.iter().map(line).collect()
    }

    /// A start mark and the stop mark `seconds` after it for each of `starts`, in order.
    fn uplink_lines(starts: &[(u32, u32)]) -> Vec<(u32, &'static str, &'static str)> {
        let cycle = |&(start_ms, seconds): &(u32, u32)| {
            let stop_ms = start_ms.saturating_add(seconds.saturating_mul(MS_PER_SECOND));
            [(start_ms, "start", ""), (stop_ms, "stop", "")]
        };
        starts.iter().flat_map(cycle).collect()
    }

    /// A captured and analysed run: a peer that sent a device information packet, then silent units for `seconds` after each of `starts_ms`.
    fn analysed(home: &TempDir, script: &str, marks: &str, starts: &[(u32, u32)]) -> PathBuf {
        let mut sent = Vec::new();
        for &(start_ms, seconds) in starts {
            let start_ns = u64::from(start_ms).saturating_mul(NS_PER_MS);
            let units = (SAMPLES_PER_UNIT, UNITS_PER_SDU);
            let silent = synth(RATE, units.0, units.1, seconds, NO_JITTER_MS, SEED).into_iter();
            sent.extend(silent.map(|mut record| {
                record.at_ns = record.at_ns.saturating_add(start_ns);
                record
            }));
        }
        analysed_sending(home, script, marks, sent)
    }

    /// A captured and analysed run: a peer that sent a device information packet, then `sent`.
    fn analysed_sending(home: &TempDir, script: &str, marks: &str, sent: Vec<Record>) -> PathBuf {
        let mut records = vec![Record {
            at_ns: 0,
            direction: Direction::FromPods,
            sdu: DEVICE_INFO.to_vec(),
        }];
        records.extend(sent);
        let meta = format!("script={script}\nrealtime_at_zero_ns={REALTIME_AT_ZERO_NS}\n");
        let files = [
            ("sdu.log", log_text(&records)),
            ("marks.tsv", marks.to_owned()),
            ("meta.txt", meta),
        ];
        let files = files
            .each_ref()
            .map(|(file, text)| (*file, text.as_bytes()));
        let run = run_holding(home, "page", &files);
        let analysis = probe(home, "analyse", &run).output().unwrap();
        assert!(analysis.status.success(), "{analysis:?}");
        run
    }

    /// The samples as the bytes of a float WAV labelled `rate`.
    fn float_wav(rate: u32, samples: &[f32]) -> Vec<u8> {
        let data: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        file(FLOAT, 1, rate, 32, &data)
    }

    /// Puts `samples` into the run as what a decode labelled `rate` wrote.
    fn decoded(run: &Path, rate: u32, samples: &[f32]) {
        put_file(run, "pods.f32.wav", &float_wav(rate, samples));
    }

    /// The table of a command that succeeded, which is also what it left in the run with the mode of the store.
    fn table_of(measured: &Output, run: &Path) -> String {
        table_in(measured, run, "audio.tsv")
    }

    /// The same of a command that leaves its table under the name `file`.
    fn table_in(measured: &Output, run: &Path, file: &str) -> String {
        assert!(measured.status.success(), "{measured:?}");
        let table = String::from_utf8(measured.stdout.clone()).unwrap();
        assert_eq!(fs::read_to_string(run.join(file)).unwrap(), table);
        let mode = fs::metadata(run.join(file)).unwrap().permissions().mode();
        assert_eq!(mode & PERMISSION_BITS, OWNER_FILE);
        for word in PRIVATE {
            assert!(!shows(&table, word), "{word}");
        }
        // No run of letters in a line spells a number that is none, and a zero carries no sign.
        for line in table.lines().skip(1) {
            let mut words = line.split(|letter: char| !letter.is_ascii_alphabetic());
            assert!(!words.any(|word| ["NaN", "inf"].contains(&word)), "{line}");
            let value = line.split('\t').nth(1).unwrap();
            let signed_zero = value.starts_with('-') && value.parse() == Ok(0.0);
            assert!(!signed_zero, "{line}");
        }
        table
    }

    fn number(table: &str, key: &str) -> f64 {
        row(table, key).0.parse().unwrap()
    }

    /// What the command says when it refuses, having written nothing on its standard output and no table.
    fn refusal(refused: &Output, run: &Path) -> String {
        assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
        assert_eq!(refused.stdout, b"");
        assert!(!run.join("audio.tsv").exists());
        String::from_utf8(refused.stderr.clone()).unwrap()
    }

    /// Where in the stream the window edge of a mark falls, in milliseconds: at the first SDU that arrives at or after the mark.
    fn in_stream(mark_ms: u32) -> u32 {
        mark_ms
            .div_ceil(SDU_MS)
            .saturating_sub(1)
            .saturating_mul(SDU_MS)
    }

    /// Thirty seconds of the voice script as a decode would hold them, every figure planted.
    fn voice_stream() -> Vec<f32> {
        let mut seed = SEED;
        let mut stream = noise(&mut seed, ROOM, samples_in(30_000, RATE));
        // Nothing sounds before 600 ms, and the loud sentence is digital silence.
        stream[span(0, 600, RATE)].fill(0.0);
        stream[span(14_900, 1100, RATE)].fill(0.0);
        for click_ms in [2000, 2300, 29_000, 29_200] {
            let click = noise(&mut seed, 0.5, samples_in(1, RATE));
            add(&mut stream, samples_in(click_ms, RATE), &click);
        }
        // The first sentence stops for 3.3 s, in which only the room is left.
        for (from_ms, len_ms, amplitude) in [
            (5200, 1500, 0.25),
            (10_000, 900, 0.25),
            (12_010, 1900, 0.05),
        ] {
            let voice = sine(300.0, RATE, amplitude, 0.0, samples_in(len_ms, RATE));
            add(&mut stream, samples_in(from_ms, RATE), &voice);
        }
        let hiss = band_noise(&mut seed, 16_000.0, RATE, 0.01, samples_in(1900, RATE));
        add(&mut stream, samples_in(17_000, RATE), &hiss);
        let keys = band_noise(&mut seed, 10_000.0, RATE, 0.01, samples_in(1900, RATE));
        add(&mut stream, samples_in(20_000, RATE), &keys);
        // The silent window opens with 400 ms at -40 dBFS, which its quietest three seconds leave out.
        let cough = noise(&mut seed, ROOM * 31.6, samples_in(400, RATE));
        add(&mut stream, samples_in(23_000, RATE), &cough);
        stream
    }

    fn voice_marks() -> String {
        marks(&[
            (0, "connect", "channel open"),
            (0, "start", "uplink start"),
            (500, "cue", TAP),
            (4000, "cue", COUNT),
            (11_000, "cue", QUIETLY),
            (14_000, "cue", LOUDLY),
            (16_000, "cue", SSS),
            (19_000, "cue", JINGLE),
            (22_000, "cue", SILENT),
            (27_500, "cue", TAP),
            (30_100, "stop", "uplink stop"),
            (30_200, "end", "script done"),
        ])
    }

    fn peak_dbfs(samples: &[f32]) -> f64 {
        let peak = samples
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        20.0 * f64::from(peak).log10()
    }

    #[test]
    fn a_voice_run_reports_every_planted_figure() {
        let home = tempfile::tempdir().unwrap();
        let run = analysed(&home, "voice", &voice_marks(), &[(0, 30)]);
        let stream = voice_stream();
        decoded(&run, RATE, &stream);
        let table = table_of(&probe(&home, "audio", &run).output().unwrap(), &run);

        assert_eq!(row(&table, "stream_rate").0, "64000");
        assert_eq!(row(&table, "stream_samples").0, "1920000");
        assert_eq!(row(&table, "marks_cut_lines").0, "0");
        let tasks = [
            "taps", "speech", "speech", "speech", "sibilant", "jingle", "silence", "taps",
        ];
        for (cue, task) in (1..).zip(tasks) {
            assert_eq!(row(&table, &format!("cue_{cue}_task")).0, task);
        }
        // The first window runs from one second after its cue to the next cue.
        let first_window = samples_in(in_stream(4000).abs_diff(in_stream(1500)), RATE);
        assert_eq!(row(&table, "cue_1_samples").0, first_window.to_string());

        for (cue, clicks_ms) in [(1, [2000, 2300]), (8, [29_000, 29_200])] {
            assert_eq!(row(&table, &format!("cue_{cue}_taps")).0, "2");
            for (tap, click_ms) in (1..).zip(clicks_ms) {
                let (peak, note) = row(&table, &format!("cue_{cue}_tap_{tap}_peak"));
                let planted = peak_dbfs(&stream[span(click_ms, 10, RATE)]);
                assert!(
                    close(peak.parse().unwrap(), planted, 0.01),
                    "{peak} {planted}"
                );
                let at_s = f64::from(click_ms) / 1000.0;
                assert!(
                    note.contains(&format!("from {at_s:.3} s of the stream")),
                    "{note}"
                );
            }
        }

        assert!(
            close(number(&table, "cue_2_speech_rms"), -15.05, 0.05),
            "{table}"
        );
        assert!(
            close(number(&table, "cue_2_speech_peak"), -12.04, 0.05),
            "{table}"
        );
        assert!(
            row(&table, "cue_2_speech_rms").1.contains("the 120 frames"),
            "{table}"
        );
        assert!(close(number(&table, "cue_2_floor"), -70.0, 0.3), "{table}");
        // The two floors lie under half a hundredth of a decibel apart, the first the lower.
        assert_eq!(row(&table, "cue_2_floor_minus_silent").0, "0.00");
        assert!(
            close(number(&table, "cue_3_speech_rms"), -29.03, 0.05),
            "{table}"
        );
        let (floor, why) = row(&table, "cue_3_floor");
        assert_eq!(floor, "none");
        assert!(
            why.contains("shorter than the 3000 ms a floor is read from"),
            "{why}"
        );
        for key in ["cue_4_speech_rms", "cue_4_speech_peak"] {
            let (level, why) = row(&table, key);
            assert_eq!(level, "none");
            assert!(why.contains("digital silence"), "{why}");
        }
        assert_eq!(row(&table, "cue_4_near_zero").0, "100.00");

        assert!(
            close(number(&table, "cue_5_cutoff"), 16_000.0, f64::from(BIN_HZ)),
            "{table}"
        );
        assert!(
            close(number(&table, "cue_6_reach"), 10_000.0, f64::from(BIN_HZ)),
            "{table}"
        );
        assert!(
            close(number(&table, "cue_7_noise_rms"), -70.0, 0.3),
            "{table}"
        );
        assert_eq!(row(&table, "cue_7_exact_zeros").0, "no");
        // The room lies within one 16 bit step of zero 5.57 percent of the time, and the cough that opens the window a thirtieth of that.
        assert!(
            close(number(&table, "cue_7_near_zero"), 5.09, 0.15),
            "{table}"
        );
        assert!(
            close(number(&table, "start_1_first_audible"), 2000.0, 0.05),
            "{table}"
        );

        let again = probe(&home, "audio", &run).output().unwrap();
        assert_eq!(again.status.code(), Some(REFUSED), "{again:?}");
        let said = String::from_utf8(again.stderr).unwrap();
        assert!(said.contains("audio.tsv already exists"), "{said}");
        assert_eq!(fs::read_to_string(run.join("audio.tsv")).unwrap(), table);
    }

    /// The cutoff the command reports for three seconds that hold a hiss walled at 16 kHz, labelled `rate`.
    fn cutoff_labelled(rate: u32) -> f64 {
        let home = tempfile::tempdir().unwrap();
        let lines = [(0, "start", ""), (0, "cue", SSS), (3100, "stop", "")];
        let run = analysed(&home, "voice", &marks(&lines), &[(0, 3)]);
        let mut seed = SEED;
        let mut stream = noise(&mut seed, ROOM, samples_in(3000, RATE));
        let hiss = band_noise(&mut seed, 16_000.0, RATE, 0.01, samples_in(1900, RATE));
        add(&mut stream, samples_in(1000, RATE), &hiss);
        decoded(&run, rate, &stream);
        let table = table_of(&probe(&home, "audio", &run).output().unwrap(), &run);
        assert_eq!(row(&table, "stream_rate").0, rate.to_string());
        number(&table, "cue_1_cutoff")
    }

    #[test]
    fn the_same_samples_labelled_48_khz_move_the_cutoff_by_a_quarter() {
        let (true_label, wrong_label) = (cutoff_labelled(RATE), cutoff_labelled(48_000));
        assert!(
            close(true_label, 16_000.0, f64::from(BIN_HZ)),
            "{true_label}"
        );
        assert!(
            close(wrong_label, 12_000.0, f64::from(BIN_HZ)),
            "{wrong_label}"
        );
    }
}
