//! The built binary's `probe decode-opus` command on generated dictation runs in a store under a temporary `HOME`.

#[cfg(test)]
#[expect(
    dead_code,
    reason = "the readers of the analysis table go unused where the decode table is what is read"
)]
#[path = "support/att.rs"]
mod att_run;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the generator of whole sessions is here only because the shared builders name its session"
)]
#[path = "../src/probe/analyse/att/synth.rs"]
mod att_synth;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the unit of the uplink decoder goes unused where every run is a dictation run"
)]
#[path = "../src/probe/fixture.rs"]
mod fixture;
#[cfg(test)]
#[path = "support/opus.rs"]
mod opus_run;
#[cfg(test)]
#[path = "support/oracles.rs"]
mod oracles;
#[cfg(test)]
#[expect(
    dead_code,
    unused_imports,
    reason = "the builders of uplink runs go unused where every run is a dictation run"
)]
mod support;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the uplink generator is here only because the shared builders name it"
)]
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

    use super::fixture::ffprobe;
    use super::opus_run::{
        Decoded, FRAME, REFUSED, SAMPLE_BYTES, Sent, count, decode, decoded,
        expected_mutation_refusals, seeded, silent, stand_in,
    };
    use super::oracles::opus_oracles_installed as oracles_installed;
    use super::support::{OWNER_FILE, PERMISSION_BITS, names_in, probe, row};

    /// The frames of the runs that need no more.
    const FRAMES: usize = 200;
    /// The seed of every run of seeded frames here, and the seed of a second session.
    const SEED: u64 = 0x5eed_0018;
    const OTHER_SEED: u64 = 0x5eed_0028;
    const ANALYSED: [&str; 9] = [
        "analysis-att.tsv",
        "analysis.tsv",
        "att-handles.tsv",
        "att.log",
        "frames.tsv",
        "marks.tsv",
        "meta.txt",
        "sdu.log",
        "units.tsv",
    ];
    const DECODED: [&str; 7] = [
        "decode-opus.tsv",
        "dictation.f32.wav",
        "dictation.opus",
        "ffopus.f32",
        "ffopus.stderr",
        "libopus.f32",
        "libopus.stderr",
    ];
    /// What ffprobe is asked for about the Ogg stream and about the WAV file.
    const SHOW_MEDIA: &str = "stream=codec_name,sample_rate,channels,nb_read_packets";
    const SHOW_WAV: &str = "stream=codec_name,sample_rate,channels,duration_ts";
    /// A peak no silence has.
    const HEARD_LSB: f64 = 100.0;
    /// The largest 16 bit sample, in its own steps.
    const FULL_SCALE_LSB: f64 = 32768.0;
    /// Two frames agree when they differ by at most the larger of this and a share of their peak.
    const TOLERANCE_LSB: f64 = 1.0;
    const AGREEMENT_OF_PEAK: f64 = 1e-3;
    /// The table of contents byte of two frames in one packet, which decodes to 40 ms.
    const TWO_FRAMES_TOC: u8 = 0xb9;
    /// The mutations the third experiment makes.
    const MUTATIONS: f64 = 10_000.0;
    /// The frame the second experiment conceals, counted from 0.
    const CONCEALED: usize = 500;
    /// Frames one of whose mutations, a first byte replaced by 5d, overflows a multiply inside the decoder, which panics there.
    const PANIC_SEED: u64 = 0x5eed_001a;
    /// The head packet of the stream: version 1, one channel, no pre-skip, an input rate of 16000, no gain, mapping family 0.
    const HEAD: &[u8; 19] = b"OpusHead\x01\x01\x00\x00\x80\x3e\x00\x00\x00\x00\x00";
    /// The tags packet: the vendor `probe` under its length, and no comment.
    const TAGS: &[u8; 21] = b"OpusTags\x05\x00\x00\x00probe\x00\x00\x00\x00";
    /// Where the two packets start: a page is 27 bytes of header and one lacing value before its packet.
    const HEAD_AT: usize = 28;
    const TAGS_AT: usize = 75;
    /// The header type of the last page of a stream.
    const END_OF_STREAM: u8 = 0x04;

    fn every_name() -> Vec<&'static str> {
        let mut names = [ANALYSED.as_slice(), &DECODED].concat();
        names.sort_unstable();
        names
    }

    fn samples(frames: usize) -> String {
        frames.saturating_mul(FRAME).to_string()
    }

    #[test]
    fn ogg_of_synthetic_frames_is_read_by_ffprobe_as_opus_mono() {
        if !oracles_installed() {
            return;
        }
        for (name, frames) in [("alone", silent(FRAMES)), ("seeded", seeded(SEED, FRAMES))] {
            let found = decoded(name, &[frames]);
            assert_eq!(
                ffprobe(&found.run.join("dictation.opus"), SHOW_MEDIA),
                format!(
                    "codec_name=opus\nsample_rate=48000\nchannels=1\nnb_read_packets={FRAMES}\n"
                ),
                "{name}"
            );
            let ogg = fs::read(found.run.join("dictation.opus")).unwrap();
            assert!(ogg[HEAD_AT..].starts_with(HEAD), "{name}");
            assert!(ogg[TAGS_AT..].starts_with(TAGS), "{name}");
            // Sample input: `OggS 00 04` then eight bytes of position, the pattern, the version, the header type and the count of samples at 48 kHz up to the end of the page.
            let last = ogg.windows(4).rposition(|four| four == b"OggS").unwrap();
            let last = &ogg[last..];
            let position = u64::try_from(FRAMES * FRAME).unwrap().to_le_bytes();
            let marked = [&[END_OF_STREAM][..], &position].concat();
            assert_eq!(last[5..14], marked, "{name}");
        }
    }

    #[test]
    fn the_oracle_decoder_is_named_not_defaulted() {
        let oracle = stand_in(FRAMES, "exit 0");
        let found = Decoded::with(&oracle, "named", &[silent(FRAMES)]);
        // Sample input: `-nostdin\n-hide_banner\n` and so on, one argument a line, with an empty line after the last of each run.
        let asked = fs::read_to_string(oracle.path().join("asked")).unwrap();
        let runs: Vec<Vec<&str>> = asked
            .split_terminator("\n\n")
            .map(|run| run.lines().collect())
            .collect();
        let before = "-nostdin -hide_banner -v error -xerror -c:a ";
        let after = "-map 0:a:0 -c:a pcm_f32le -f f32le pipe:1";
        assert_eq!(runs.len(), 2, "{asked}");
        // The wrapper of the reference library is also asked for floats, or it rounds to 16 bits and clips.
        let named = ["libopus -request_sample_fmt flt", "opus"];
        for (run, decoder) in runs.iter().zip(named) {
            let before = format!("{before}{decoder} -i");
            let (before, after): (Vec<&str>, Vec<&str>) =
                (before.split(' ').collect(), after.split(' ').collect());
            assert_eq!(run.len(), before.len() + 1 + after.len(), "{run:?}");
            let (first, rest) = run.split_at(before.len());
            assert_eq!((first, &rest[1..]), (&before[..], &after[..]), "{decoder}");
            // The stream is handed over as one argument that names an open file of this process, never a path in the run.
            let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
            let input: Vec<&str> = rest[0].split('/').collect();
            let [_, _, pid, _, fd] = input[..] else {
                panic!("{decoder}: {}", rest[0]);
            };
            assert_eq!(rest[0], format!("/proc/{pid}/fd/{fd}"), "{decoder}");
            assert!(digits(pid) && digits(fd), "{decoder}: {}", rest[0]);
        }
        assert_eq!(found.value("libopus_refused"), "0");
        assert_eq!(found.value("ffopus_refused"), "0");
    }

    #[test]
    fn oracle_decodes_synthetic_frames_to_960_samples_each() {
        if !oracles_installed() {
            return;
        }
        for (name, frames) in [("alone", silent(FRAMES)), ("seeded", seeded(SEED, FRAMES))] {
            let found = decoded(name, &[frames]);
            for oracle in ["libopus", "ffopus"] {
                assert_eq!(found.value(&format!("{oracle}_refused")), "0", "{name}");
                let samples = samples(FRAMES);
                assert_eq!(found.value(&format!("{oracle}_samples")), samples, "{name}");
                let compared = found.value(&format!("{oracle}_frames_compared"));
                assert_eq!(compared, FRAMES.to_string(), "{name} {oracle}");
                assert_eq!(found.value(&format!("{oracle}_offset_units")), "0");
                let written = fs::metadata(found.run.join(format!("{oracle}.f32"))).unwrap();
                let written = usize::try_from(written.len()).unwrap();
                assert_eq!(written, FRAMES * FRAME * SAMPLE_BYTES);
                let said = fs::read(found.run.join(format!("{oracle}.stderr"))).unwrap();
                assert_eq!(said, b"", "{name} {oracle}");
            }
        }
    }

    #[test]
    fn candidate_decodes_synthetic_frames_to_960_samples_each_and_to_320_at_16_khz() {
        if !oracles_installed() {
            return;
        }
        for (name, frames) in [("alone", silent(FRAMES)), ("seeded", seeded(SEED, FRAMES))] {
            let found = decoded(name, &[frames]);
            let frames = FRAMES.to_string();
            for (key, value) in [
                ("frames_in", frames.as_str()),
                ("frames_not_b8", "0"),
                ("candidate_decoded", &frames),
                ("candidate_errors", "0"),
                ("candidate_panics", "0"),
                ("candidate_samples", &samples(FRAMES)),
                ("rate_16k_candidate_decoded", &frames),
                ("rate_16k_candidate_errors", "0"),
                ("rate_16k_candidate_panics", "0"),
                ("rate_16k_candidate_samples", &(FRAMES * 320).to_string()),
            ] {
                assert_eq!(found.value(key), value, "{name} {key}");
            }
            assert_eq!(found.candidate().len(), FRAMES * FRAME, "{name}");
            let mean_us = found.number("candidate_decode_us_mean");
            assert!(mean_us > 0.0, "{name}");
            assert!(found.number("candidate_decode_us_max") >= mean_us, "{name}");
            let (_, calls) = row(&found.table, "candidate_decode_us_mean");
            assert!(calls.starts_with(&format!("{frames} calls ")), "{calls}");
            assert_eq!(
                ffprobe(&found.run.join("dictation.f32.wav"), SHOW_WAV),
                format!(
                    "codec_name=pcm_f32le\nsample_rate=48000\nchannels=1\nduration_ts={}\n",
                    samples(FRAMES)
                ),
                "{name}"
            );
        }
    }

    #[test]
    fn decode_opus_names_every_file_once_and_refuses_a_second_decode() {
        if !oracles_installed() {
            return;
        }
        let found = decoded("once", &[silent(FRAMES)]);
        assert_eq!(names_in(&found.run), every_name());
        for file in DECODED {
            let mode = fs::metadata(found.run.join(file)).unwrap().permissions();
            assert_eq!(mode.mode() & PERMISSION_BITS, OWNER_FILE, "{file}");
        }
        let kept = |file: &&str| fs::read(found.run.join(file)).unwrap();
        let before: Vec<Vec<u8>> = DECODED.iter().map(kept).collect();

        let again = probe(&found.home, "decode-opus", &found.run)
            .output()
            .unwrap();
        assert_eq!(again.status.code(), Some(REFUSED), "{again:?}");
        assert_eq!(again.stdout, b"");
        let error = String::from_utf8(again.stderr).unwrap();
        assert!(error.contains("/dictation.opus already exists"), "{error}");
        assert_eq!(names_in(&found.run), every_name());
        let after: Vec<Vec<u8>> = DECODED.iter().map(kept).collect();
        assert!(after == before, "the refused decode changed a file");
    }

    #[test]
    fn candidate_and_both_oracles_agree_exactly_on_frames_of_b8_alone() {
        if !oracles_installed() {
            return;
        }
        let found = decoded("alone", &[silent(FRAMES)]);
        for (key, value) in [
            ("candidate_peak_lsb", "0.000"),
            ("libopus_peak_lsb", "0.000"),
            ("libopus_max_lsb", "0.000"),
            ("libopus_frames_over_one_lsb", "0"),
            ("libopus_max_of_peak", "0.000e0"),
            ("libopus_frames_over_gate", "0"),
            ("ffopus_peak_lsb", "0.000"),
            ("ffopus_max_lsb", "0.000"),
            ("ffopus_frames_over_one_lsb", "0"),
            ("ffopus_max_of_peak", "0.000e0"),
            ("ffopus_frames_over_gate", "0"),
            // Agreement on silence proves nothing, and the control says so.
            ("second_byte_removed_candidate_frames_over_gate", "0"),
            ("second_byte_removed_discriminates", "no"),
        ] {
            assert_eq!(found.value(key), value, "{key}");
        }
    }

    /// Asserts the rows of the comparison with `oracle` are what the files of the run give, and returns the frames those files put outside the gate.
    fn frames_outside(found: &Decoded, oracle: &str) -> usize {
        let laid = found.frames_apart(oracle);
        let largest = laid.iter().map(|(apart, _)| *apart).fold(0.0, f64::max);
        let shares = laid.iter().map(|(apart, peak)| apart / peak);
        let of_peak = shares.fold(0.0, f64::max);
        let outside =
            |(apart, peak): &&(f64, f64)| *apart > TOLERANCE_LSB.max(AGREEMENT_OF_PEAK * peak);
        let over = laid.iter().filter(outside).count();
        let past_one = laid.iter().filter(|(apart, _)| *apart > TOLERANCE_LSB);
        for (key, recomputed) in [
            ("max_lsb", format!("{largest:.3}")),
            ("frames_over_one_lsb", past_one.count().to_string()),
            ("max_of_peak", format!("{of_peak:.3e}")),
            ("frames_over_gate", over.to_string()),
        ] {
            let (value, note) = row(&found.table, &format!("{oracle}_{key}"));
            assert_eq!(value, recomputed, "{oracle} {key}");
            let not_a_gate = note.starts_with("not a gate; ");
            assert_eq!(not_a_gate, oracle == "ffopus", "{note}");
        }
        over
    }

    #[test]
    fn candidate_and_libopus_agree_on_seeded_frames() {
        if !oracles_installed() {
            return;
        }
        let found = decoded("seeded", &[seeded(SEED, FRAMES)]);
        // Every frame of this stream is heard, so none agrees for being silence.
        let heard = |(_, peak): &(f64, f64)| *peak > HEARD_LSB;
        assert!(found.frames_apart("libopus").iter().all(heard));
        assert_eq!(frames_outside(&found, "libopus"), 0);
        // FFmpeg's own decoder is no gate here either: its rows are read from the files again and its count is left open.
        frames_outside(&found, "ffopus");
        // The candidate passes full scale by more than the gate at its peak, so a wrapper left to round to 16 bits and clip would put a frame outside it.
        let peak = found.number("candidate_peak_lsb");
        let gate = TOLERANCE_LSB.max(AGREEMENT_OF_PEAK * peak);
        assert!(peak - FULL_SCALE_LSB > gate, "{peak}");
        let control = found.number("second_byte_removed_candidate_frames_over_gate");
        assert!(control > 0.0, "{control}");
    }

    /// A shell command that writes one frame of `sample`.
    // Sample input: `\000\000\000\077`, the four bytes of the float 0.5 in octal, which the shell prints 960 times.
    fn frame_of(sample: &str) -> String {
        format!("i=0; while [ $i -lt {FRAME} ]; do printf '{sample}'; i=$((i+1)); done")
    }

    #[test]
    fn the_gate_takes_the_peak_of_either_decode_and_a_sample_that_is_not_a_number_is_outside_it() {
        for (name, sample, max_lsb, of_peak, outside) in [
            // One step of a 16 bit sample is the gate itself, and a frame at the gate agrees.
            ("one-step", r"\000\000\000\070", "1.000", "1.000e0", "0"),
            ("half", r"\000\000\000\077", "16384.000", "1.000e0", "1"),
            ("not-a-number", r"\000\000\300\177", "inf", "inf", "1"),
        ] {
            let oracle = stand_in(FRAMES - 1, &frame_of(sample));
            let found = Decoded::with(&oracle, name, &[silent(FRAMES)]);
            for (key, value) in [
                ("libopus_offset_units", "0"),
                ("libopus_max_lsb", max_lsb),
                ("libopus_max_of_peak", of_peak),
                ("libopus_frames_over_gate", outside),
            ] {
                assert_eq!(found.value(key), value, "{name} {key}");
            }
        }
        // An oracle that gives silence under a candidate that is heard: every frame is its whole peak away.
        let oracle = stand_in(FRAMES, "exit 0");
        let found = Decoded::with(&oracle, "deaf", &[seeded(SEED, FRAMES)]);
        assert_eq!(found.value("libopus_max_of_peak"), "1.000e0");
        let peak = found.value("candidate_peak_lsb");
        assert_eq!(found.value("libopus_max_lsb"), peak);
        let compared = found.value("libopus_frames_compared");
        assert_eq!(found.value("libopus_frames_over_gate"), compared);
    }

    #[test]
    fn an_oracle_one_frame_late_is_compared_at_that_offset() {
        let deaf = stand_in(FRAMES, "exit 0");
        let first = Decoded::with(&deaf, "first", &[seeded(SEED, FRAMES)]);
        // Each stand-in gives one frame of silence and then the candidate's own decode without its last frame; the second holds a frame that is not a number where the offsets are judged.
        let late = first.replay(0, FRAMES - 1);
        let not_a_number = frame_of(r"\000\000\300\177");
        let (to, from) = (first.replay(0, 4), first.replay(5, FRAMES - 6));
        let holed = format!("{to}; {not_a_number}; {from}");
        for (name, then, max_lsb, outside) in
            [("late", late, "0.000", "0"), ("holed", holed, "inf", "1")]
        {
            let oracle = stand_in(1, &then);
            let found = Decoded::with(&oracle, name, &[seeded(SEED, FRAMES)]);
            for (key, value) in [
                ("libopus_offset_units", "-1"),
                ("libopus_frames_compared", &(FRAMES - 1).to_string()),
                ("libopus_max_lsb", max_lsb),
                ("libopus_frames_over_gate", outside),
            ] {
                assert_eq!(found.value(key), value, "{name} {key}");
            }
        }
    }

    #[test]
    fn an_oracle_that_gives_no_number_names_no_offset() {
        let bytes = FRAMES * FRAME * SAMPLE_BYTES;
        // Every byte set is a float that is not a number, so no frame is left to judge an offset on.
        let oracle = stand_in(0, &format!(r"head -c {bytes} /dev/zero | tr '\000' '\377'"));
        let found = Decoded::with(&oracle, "never", &[seeded(SEED, FRAMES)]);
        let why = "every frame the offsets are judged on meets a sample that is not a number at one of them, so no offset is named; the frames are compared at no offset";
        assert_eq!(row(&found.table, "libopus_offset_units"), ("refused", why));
        let frames = FRAMES.to_string();
        for key in ["libopus_frames_compared", "libopus_frames_over_gate"] {
            assert_eq!(found.value(key), frames, "{key}");
        }
        assert_eq!(found.value("libopus_max_lsb"), "inf");
    }

    #[test]
    fn a_frame_with_its_second_byte_removed_changes_the_output() {
        if !oracles_installed() {
            return;
        }
        let found = decoded("seeded", &[seeded(SEED, FRAMES)]);
        assert_eq!(
            found.value("second_byte_removed_candidate_decoded"),
            FRAMES.to_string()
        );
        assert_eq!(found.value("second_byte_removed_candidate_errors"), "0");
        let changed = found.number("second_byte_removed_candidate_frames_over_gate");
        assert!(changed > 0.0 && changed <= count(FRAMES), "{changed}");
        assert_eq!(found.value("second_byte_removed_discriminates"), "yes");
    }

    #[test]
    fn a_frame_that_is_not_b8_is_refused_before_decoding() {
        if !oracles_installed() {
            return;
        }
        let mut frames = seeded(SEED, FRAMES);
        // Decoded, this frame would be 40 ms: one frame too many from the oracle and an error from the candidate.
        frames[FRAMES / 2].data[0] = TWO_FRAMES_TOC;
        let found = decoded("odd", &[frames]);
        let frames = FRAMES.to_string();
        for (key, value) in [
            ("frames_in", frames.as_str()),
            ("frames_not_b8", "1"),
            ("candidate_decoded", &frames),
            ("candidate_errors", "0"),
            ("candidate_samples", &samples(FRAMES)),
            ("libopus_samples", &samples(FRAMES)),
            ("libopus_frames_compared", &frames),
            ("libopus_offset_units", "0"),
            ("ffopus_samples", &samples(FRAMES)),
            ("rate_16k_candidate_decoded", &frames),
        ] {
            assert_eq!(found.value(key), value, "{key}");
        }
        let sizes = ffprobe(&found.run.join("dictation.opus"), "packet=size");
        let sizes: Vec<&str> = sizes.lines().collect();
        assert_eq!(sizes.len(), FRAMES, "{sizes:?}");
        assert_eq!(sizes[FRAMES / 2], "size=1");
        assert_eq!(sizes.iter().filter(|size| **size == "size=1").count(), 1);
    }

    #[test]
    fn concealment_returns_a_full_frame() {
        if !oracles_installed() {
            return;
        }
        let found = decoded("long", &[seeded(SEED, 3 * FRAMES)]);
        assert_eq!(found.value("concealed_frame"), CONCEALED.to_string());
        assert_eq!(found.value("concealed_samples"), FRAME.to_string());
        let recover = found.number("concealed_frames_to_recover");
        assert!(
            recover >= 1.0 && recover < count(3 * FRAMES - CONCEALED - 1),
            "{recover}"
        );
    }

    #[test]
    fn a_stream_with_no_frame_to_conceal_or_none_to_come_back_in_says_so() {
        let oracle = stand_in(CONCEALED + 1, "exit 0");
        let found = Decoded::with(&oracle, "last", &[seeded(SEED, CONCEALED + 1)]);
        assert_eq!(found.value("concealed_samples"), FRAME.to_string());
        let recover = row(&found.table, "concealed_frames_to_recover");
        assert_eq!(recover, ("refused", "no frame follows frame 500"));
        // A frame nobody decodes is concealed in the whole decode too, so withholding it would measure nothing.
        let mut odd = seeded(SEED, CONCEALED + 2);
        odd[CONCEALED].data[0] = TWO_FRAMES_TOC;
        for (name, frames, why) in [
            (
                "short",
                seeded(SEED, CONCEALED),
                "the stream has no frame 500",
            ),
            (
                "odd",
                odd,
                "frame 500 is not b8, so the whole decode conceals it too",
            ),
        ] {
            let oracle = stand_in(frames.len(), "exit 0");
            let found = Decoded::with(&oracle, name, &[frames]);
            assert_eq!(row(&found.table, "concealed_frame"), ("refused", why));
            assert!(!found.table.contains("concealed_samples"), "{name}");
        }
    }

    #[test]
    fn mutations_never_escape_as_a_panic() {
        if !oracles_installed() {
            return;
        }
        let frames = seeded(SEED, FRAMES);
        let (expected_refusals, _) = expected_mutation_refusals(&frames, 10_000);
        let found = decoded("seeded", &[frames]);
        assert_eq!(found.number("mutations"), MUTATIONS);
        let outcomes = ["mutations_decoded", "mutations_errors", "mutations_panics"];
        let counted: f64 = outcomes.iter().map(|key| found.number(key)).sum();
        assert_eq!(counted, MUTATIONS);
        assert_eq!(found.value("mutations_panics"), "0");
        assert_eq!(
            found.value("mutations_errors"),
            expected_refusals.to_string()
        );
        assert_eq!(
            found.value("mutations_decoded"),
            (10_000_usize - expected_refusals).to_string()
        );
        // How long a call took is this machine's and this run's: the row is a count, and no count of it is wrong.
        assert!(found.value("mutations_over_100_ms").parse::<u32>().is_ok());
        let how = row(&found.table, "mutations").1;
        assert!(
            how.contains("one bit flipped, cut short or its first byte replaced"),
            "{how}"
        );
    }

    #[test]
    fn hostile_toc_5d_mutation_seed_completes_without_panicking() {
        let frames = seeded(PANIC_SEED, FRAMES);
        let (expected_refusals, toc_5d) = expected_mutation_refusals(&frames, 10_000);
        let toc_5d = toc_5d.expect("PANIC_SEED must generate a 0x5d TOC mutation");
        assert_eq!(
            pods_opus::Decoder::new_48k().decode(&toc_5d),
            Err(pods_opus::Error::UnsupportedToc(0x5d))
        );
        let oracle = stand_in(FRAMES, "exit 0");
        let (home, analysed) = super::opus_run::analysed("panic", &[frames]);
        let done = decode(&home, &analysed.run, Some(&oracle));
        assert!(done.status.success(), "{done:?}");
        let table = String::from_utf8(done.stdout).unwrap();
        let number = |key: &str| row(&table, key).0.parse::<f64>().unwrap();
        assert_eq!(number("mutations_panics"), 0.0, "{table}");
        assert_eq!(
            row(&table, "mutations_errors").0,
            expected_refusals.to_string(),
            "{table}"
        );
        assert_eq!(
            row(&table, "mutations_decoded").0,
            (10_000_usize - expected_refusals).to_string(),
            "{table}"
        );
        assert_eq!(number("candidate_panics"), 0.0, "{table}");
        assert_eq!(names_in(&analysed.run), every_name());
    }

    #[test]
    fn oracle_exit_code_is_read_directly() {
        let say = "echo 'a line from the stand-in' >&2";
        for (name, then, refused, said) in [
            (
                "clean",
                "exit 0",
                "0",
                "exited with success and wrote nothing to libopus.stderr",
            ),
            ("failed", "exit 3", "1", "exit status: 3; 0 lines said"),
            (
                "said",
                say,
                "1",
                "exit status: 0; 1 lines said, first: a line from the stand-in",
            ),
        ] {
            // Only the run that names libopus ends as `then` says, so the other oracle shows what a sound run reads as.
            let then = format!("case \"$*\" in *libopus*) {then};; esac");
            let oracle = stand_in(FRAMES, &then);
            let found = Decoded::with(&oracle, name, &[silent(FRAMES)]);
            let (value, note) = row(&found.table, "libopus_refused");
            assert_eq!((value, note), (refused, said), "{name}");
            assert_eq!(found.value("ffopus_refused"), "0", "{name}");
            assert_eq!(found.value("ffopus_samples"), samples(FRAMES), "{name}");
            let sound = refused == "0";
            for key in [
                "libopus_samples",
                "libopus_peak_lsb",
                "libopus_frames_compared",
            ] {
                assert_eq!(found.value(key) != "refused", sound, "{name} {key}");
            }
        }
    }

    #[test]
    fn an_oracle_that_wrote_another_count_of_samples_is_reported_not_compared() {
        // One frame at 16 kHz in place of each: a third of the samples, as a reader that did not hand over 48000 would give.
        let oracle = stand_in(FRAMES / 3, "exit 0");
        let found = Decoded::with(&oracle, "third", &[silent(FRAMES)]);
        assert_eq!(found.value("libopus_samples"), samples(FRAMES / 3));
        for key in [
            "libopus_frames_compared",
            "libopus_max_lsb",
            "libopus_frames_over_one_lsb",
            "libopus_max_of_peak",
            "libopus_frames_over_gate",
        ] {
            let (value, why) = row(&found.table, key);
            assert_eq!(value, "refused", "{key}");
            assert!(why.contains("more than 3 apart"), "{key}: {why}");
        }
    }

    #[test]
    fn the_flag_and_level_rows_are_those_of_the_decoded_frames() {
        let sessions = [seeded(SEED, FRAMES), seeded(OTHER_SEED, FRAMES)];
        let found = Decoded::with(&stand_in(2 * FRAMES, "exit 0"), "two", &sessions);
        let sent: Vec<&Sent> = sessions.iter().flatten().collect();
        let candidate = found.candidate();
        let db: Vec<f64> = candidate.chunks(FRAME).map(Decoded::level_db).collect();
        let (first, second) = db.split_at(FRAMES);
        let loudest = |session: &[f64]| session.iter().copied().fold(f64::MIN, f64::max);
        let tops = [loudest(first), loudest(second)];
        let is_loud = |frame: usize| db[frame] >= tops[frame / FRAMES] - 20.0;
        // One loudest frame for the whole run must class some frame another way, or it would pass for one a session.
        let top = tops[0].max(tops[1]);
        let moved = (0..2 * FRAMES).filter(|&frame| is_loud(frame) != (db[frame] >= top - 20.0));
        assert!(moved.count() > 0, "{tops:?}");
        let share = |loud: bool| {
            let group: Vec<usize> = (0..2 * FRAMES)
                .filter(|&frame| is_loud(frame) == loud)
                .collect();
            let flagged = group.iter().filter(|&&frame| sent[frame].flag != 0).count();
            let share = count(flagged) / count(group.len());
            (format!("{share:.3}"), flagged, group.len())
        };
        for (key, loud) in [("flagged_share_loud", true), ("flagged_share_rest", false)] {
            let (value, flagged, of) = share(loud);
            let (found_value, note) = row(&found.table, key);
            assert_eq!(found_value, value, "{key}");
            assert!(
                note.starts_with(&format!("{flagged} of {of} frames ")),
                "{key}: {note}"
            );
        }
        let levels: Vec<f64> = sent.iter().map(|frame| f64::from(frame.level)).collect();
        let expected = Decoded::correlation(&levels, &db);
        assert_eq!(
            found.value("level_rms_correlation"),
            format!("{expected:.3}")
        );
        // The seeded level byte follows the loudness of nothing, and a correlation near 1 would be no measure of it.
        assert!(expected.abs() < 0.5, "{expected}");
    }

    #[test]
    fn a_level_byte_that_follows_the_loudness_correlates() {
        let oracle = stand_in(FRAMES, "exit 0");
        let quiet = Decoded::with(&oracle, "plain", &[seeded(SEED, FRAMES)]);
        let db: Vec<f64> = quiet
            .candidate()
            .chunks(FRAME)
            .map(Decoded::level_db)
            .collect();
        let top = db.iter().copied().fold(f64::MIN, f64::max);
        // The same frames, each now flagged when it is loud and levelled by its own loudness.
        let mut frames = seeded(SEED, FRAMES);
        for (frame, db) in frames.iter_mut().zip(&db) {
            frame.flag = if *db >= top - 20.0 { 0x80 } else { 0x00 };
            frame.level = format!("{:.0}", db.clamp(-200.0, 50.0) + 200.0)
                .parse()
                .unwrap();
        }
        let found = Decoded::with(&oracle, "following", &[frames]);
        assert_eq!(found.value("flagged_share_loud"), "1.000");
        assert_eq!(found.value("flagged_share_rest"), "0.000");
        assert!(found.number("level_rms_correlation") > 0.99);
    }

    #[test]
    fn the_flag_and_level_rows_need_fifty_frames_and_the_correlation_fifty_that_are_heard() {
        let thin = Decoded::with(&stand_in(49, "exit 0"), "thin", &[seeded(SEED, 49)]);
        let fifty = Decoded::with(&stand_in(50, "exit 0"), "fifty", &[seeded(SEED, 50)]);
        for key in [
            "flagged_share_loud",
            "flagged_share_rest",
            "level_rms_correlation",
        ] {
            let refused = ("refused", "49 decoded frames; needs 50");
            assert_eq!(row(&thin.table, key), refused, "{key}");
            assert!(fifty.number(key).abs() <= 1.0, "{key}");
        }
        let after_silence = |name: &str, quiet: usize, heard: usize| {
            let frames: Vec<Sent> = silent(quiet)
                .into_iter()
                .chain(seeded(SEED, heard))
                .collect();
            Decoded::with(&stand_in(frames.len(), "exit 0"), name, &[frames])
        };
        // Two frames that are heard after 58 of silence: two points lie on a line whatever their bytes mean.
        let found = after_silence("quiet", 58, 2);
        let few = ("refused", "2 frames that are not silence; needs 50");
        assert_eq!(row(&found.table, "level_rms_correlation"), few);
        assert_eq!(found.value("flagged_share_rest"), "0.000");
        // One frame of silence leaves 49 that are heard of the 50 decoded, which is one too few.
        let found = after_silence("nearly", 1, 49);
        let few = ("refused", "49 frames that are not silence; needs 50");
        assert_eq!(row(&found.table, "level_rms_correlation"), few);
    }

    #[test]
    fn decode_opus_without_the_oracle_or_the_frames_table_writes_nothing() {
        let (home, analysed) = super::opus_run::analysed("no-oracle", &[silent(FRAMES)]);
        let nowhere = tempfile::tempdir().unwrap();
        let mut command = probe(&home, "decode-opus", &analysed.run);
        command.env("PATH", nowhere.path());
        let refused = command.output().unwrap();
        assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
        assert_eq!(refused.stdout, b"");
        let error = String::from_utf8(refused.stderr).unwrap();
        assert!(error.contains("could not start ffmpeg"), "{error}");
        assert_eq!(names_in(&analysed.run), ANALYSED);

        // One line fewer in the frames table than the log holds frames: the two are not of one run.
        let short = analysed
            .frames
            .lines()
            .take(FRAMES)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        for (name, table, why) in [
            (
                "short",
                short.as_str(),
                "frames.tsv lists 199 frames and att.log holds 200",
            ),
            (
                "ragged",
                "header\nno tabs here\n",
                "line 2 of frames.tsv is not 9 fields",
            ),
        ] {
            let run = super::opus_run::with_frames_table(&home, name, &analysed, table);
            let before = names_in(&run);
            let refused = decode(&home, &run, None);
            assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
            assert_eq!(refused.stdout, b"");
            let error = String::from_utf8(refused.stderr).unwrap();
            assert!(error.contains(why), "{name}: {error}");
            assert_eq!(names_in(&run), before);
        }
        let run = super::opus_run::without_frames_table(&home, "raw", &analysed);
        let refused = decode(&home, &run, None);
        assert_eq!(refused.status.code(), Some(REFUSED), "{refused:?}");
        let error = String::from_utf8(refused.stderr).unwrap();
        assert!(error.contains("/frames.tsv"), "{error}");
        assert_eq!(names_in(&run), ["att.log", "meta.txt"]);
    }

    #[test]
    fn decode_opus_refuses_a_run_of_no_frame_and_one_of_too_many() {
        let oracle = stand_in(0, "exit 0");
        for (name, frames, why) in [
            ("none", 0, "att.log holds no frame"),
            (
                "many",
                60_001,
                "att.log holds 60001 frames, and one decode takes at most 60000",
            ),
        ] {
            let (home, analysed) = super::opus_run::analysed(name, &[silent(frames)]);
            let before = names_in(&analysed.run);
            let refused = decode(&home, &analysed.run, Some(&oracle));
            assert_eq!(refused.status.code(), Some(REFUSED), "{name}: {refused:?}");
            assert_eq!(refused.stdout, b"");
            let error = String::from_utf8(refused.stderr).unwrap();
            assert!(error.contains(why), "{name}: {error}");
            assert_eq!(names_in(&analysed.run), before);
        }
    }
}
