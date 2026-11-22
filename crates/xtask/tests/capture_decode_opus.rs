//! The built binary's `probe capture`, `probe analyse` and `probe decode-opus` commands in a row, on the dictation stream the fake pods send through the socket trap.

#[cfg(test)]
#[expect(
    dead_code,
    reason = "the parts of the fake pods only the unit tests of the capture loop read go unused here"
)]
#[path = "../src/probe/capture/tests/att_pods.rs"]
mod att_pods;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "what the tests of the capture itself read of a run goes unused where the run is only decoded"
)]
#[path = "support/capture.rs"]
mod capture;
#[cfg(test)]
#[path = "support/oracles.rs"]
mod oracles;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the parts of the fake pods only the unit tests of the capture loop read go unused here"
)]
#[path = "../src/probe/capture/tests/pods.rs"]
mod pods;

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use super::att_pods::AttPods;
    use super::capture::{A_FEW, Analysed, HANG_LIMIT, files_of, probe, read};
    use super::oracles::opus_oracles_installed;

    #[test]
    fn a_captured_dictation_stream_decodes_with_the_candidate_and_both_oracles() {
        if !opus_oracles_installed() {
            return;
        }
        let att = AttPods {
            interrupt_after: Some(A_FEW),
            ..AttPods::default()
        };
        let found = Analysed::of(att, HANG_LIMIT);
        let run = found.run.to_str().unwrap();
        let decoded = probe(found.home.path(), &["decode-opus", run])
            .output()
            .unwrap();
        assert!(decoded.status.success(), "{decoded:?}");
        assert_eq!(String::from_utf8(decoded.stderr).unwrap(), "");
        let table = String::from_utf8(decoded.stdout).unwrap();
        assert_eq!(read(&found.run, "decode-opus.tsv"), table);

        // Sample input: `frames_in\t163\tframes\twhole frames of the audio notifications in the log, in order`, read for the key `frames_in` as `163`.
        let value = |key: &str| {
            let rows = table.lines().map(|line| line.split('\t'));
            let mut named: Vec<_> = rows.filter(|row| row.clone().next() == Some(key)).collect();
            assert_eq!(named.len(), 1, "{key}\n{table}");
            named[0].nth(1).unwrap()
        };
        let frames = found.frames_sent().to_string();
        // Each oracle gave as many samples as the candidate; what one frame holds is pinned where the command is tested alone.
        let samples = value("candidate_samples");
        for (key, expected) in [
            ("frames_in", frames.as_str()),
            ("frames_not_b8", "0"),
            ("candidate_decoded", &frames),
            ("candidate_errors", "0"),
            ("candidate_panics", "0"),
            ("libopus_refused", "0"),
            ("libopus_samples", samples),
            ("libopus_frames_compared", &frames),
            ("libopus_offset_units", "0"),
            ("ffopus_refused", "0"),
            ("ffopus_samples", samples),
            ("ffopus_frames_compared", &frames),
            ("ffopus_offset_units", "0"),
            ("rate_16k_candidate_decoded", &frames),
            ("rate_16k_candidate_errors", "0"),
            ("second_byte_removed_discriminates", "yes"),
        ] {
            assert_eq!(value(key), expected, "{key}\n{table}");
        }
        for key in [
            "libopus_max_lsb",
            "libopus_max_of_peak",
            "ffopus_max_lsb",
            "ffopus_max_of_peak",
        ] {
            let figure: f64 = value(key).parse().unwrap();
            assert!(figure.is_finite(), "{key}\n{table}");
        }
        let files = "analysis-att.tsv analysis.tsv att-handles.tsv att.log decode-opus.tsv dictation.f32.wav dictation.opus ffopus.f32 ffopus.stderr frames.tsv handles.tsv libopus.f32 libopus.stderr marks.tsv meta.txt sdu.log units.tsv";
        assert_eq!(files_of(&found.run).join(" "), files);
    }
}
