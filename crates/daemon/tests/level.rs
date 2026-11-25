//! The level of the pods' audio through the built daemon into a recorder, against the fake pods and a private `PipeWire`: the gain on a known level, a tone under the knee left whole, a tap held under full scale, silence, and the count a leaving recorder logs; a level is the median of its windows, so one late packet of the fake moves none.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;
    use std::ops::RangeInclusive;
    use std::time::Duration;

    use pods_core::policy::mic::FIRST_AUDIO_PREFIX;
    use pods_fake::uplink::{PERIOD, TONE_LINE};
    use pods_mic::level::{LIMIT_CEILING_DBFS, LIMIT_KNEE_DBFS, MIC_GAIN_DB};
    use pods_mic::{RATE_HZ, SAMPLES_PER_UNIT};
    use pods_pwrig::Server;
    use pods_pwrig::tone::SETTLE;

    use super::support::HANG_LIMIT;
    use super::support::pipewire::{ready, record};
    use super::support::recording::{
        RECORDER_RATE, assert_left_counts, counts, counts_after_leaving, decoded_tone, first_sound,
        frames, line_hz, rms_dbfs, span_at, wait_packets,
    };
    use super::support::rig::{Device, Rig};
    use super::support::uplink::{read_up_to_now, wait_mic};

    /// The fake's level whose tone decodes nearest -35 dBFS, where quiet speech stands.
    const KNOWN_LEVEL: u8 = 174;
    const KNOWN_DBFS: f64 = -35.0;
    const KNOWN_WITHIN_DB: f64 = 2.0;
    /// The loudest level of the fake whose peak after the gain keeps this far under the knee; one step of the level is 1.5 dB.
    const SPEECH_LEVEL: u8 = 180;
    const UNDER_THE_KNEE_DB: RangeInclusive<f64> = 2.0..=3.5;
    /// The lowest level of the fake that decodes at or above the loudest tap measured from real pods.
    const TAP_LEVEL: u8 = 195;
    const TAP_DBFS: f64 = -1.4;
    /// Units more than 200 dB under the others: nothing a 16 bit file can hold.
    const SILENT_LEVEL: u8 = 0;
    /// How far a recorded level may stand from the decoder's own plus the gain.
    const GAIN_SLACK_DB: f64 = 0.5;
    /// The multiples of the tone a 16 kHz file can hold, and how far under the tone each must stay.
    const HARMONICS: RangeInclusive<u32> = 2..=7;
    const HARMONIC_MAX_DB: f64 = -60.0;
    /// What a converter to 16 kHz adds at most to the peak of a flattened wave.
    const CONVERTER_RISE_DB: f64 = 2.1;
    /// Under one step of a 16 bit file.
    const SILENCE_DBFS: f64 = -90.0;
    /// Recorded before the checked span, for the start of the uplink and the settle.
    const LEAD: Duration = Duration::from_millis(300);
    const CHECKED: Duration = Duration::from_millis(1200);
    /// The units a window holds: the decoder's tone repeats every unit, so every line of it stands on a bin of a window of whole units.
    const WINDOW_UNITS: u32 = 16;
    /// The fewest windows a level is the median of.
    const FEWEST_WINDOWS: usize = 5;
    /// How far the count of zero crossings may pass two a cycle, for the two ends of the span.
    const CROSSINGS_SLACK: f64 = 4.0;

    /// What Superwhisper's recorder got of the fake's tone at `level`: the lead, the settle and the checked span.
    fn recorded(rig: &Rig, device: &Device, pipewire: &Server, level: u8) -> Vec<i16> {
        device.pods.uplink().set_level(level);
        let recorder = record(rig, pipewire, "capture.wav");
        wait_mic(rig, "recording");
        let span = LEAD.saturating_add(SETTLE).saturating_add(CHECKED);
        recorder.wait_frames(frames(span), HANG_LIMIT);
        recorder.finish()
    }

    /// Each sample as a part of full scale.
    fn parts(samples: &[i16]) -> Vec<f64> {
        let full_scale = -f64::from(i16::MIN);
        samples
            .iter()
            .map(|&sample| f64::from(sample) / full_scale)
            .collect()
    }

    fn peak_dbfs(samples: &[f64]) -> f64 {
        20.0 * samples
            .iter()
            .fold(0.0_f64, |peak, sample| peak.max(sample.abs()))
            .log10()
    }

    /// The level and the peak of the decoder's own tone at `level`, in dBFS.
    fn decoded_dbfs(level: u8) -> (f64, f64) {
        let tone: Vec<f64> = decoded_tone(level).into_iter().map(f64::from).collect();
        (rms_dbfs(tone.iter().copied()), peak_dbfs(&tone))
    }

    /// The recording past its first sound and the settle, a part of full scale a sample.
    fn sounding(samples: &[i16]) -> Vec<f64> {
        parts(&samples[first_sound(samples).saturating_add(frames(SETTLE))..])
    }

    /// The median over the windows of `sounding` of what `measure` makes of each.
    fn median(sounding: &[f64], measure: impl Fn(&[f64]) -> f64) -> f64 {
        let unit = span_at(u64::from(SAMPLES_PER_UNIT), RATE_HZ);
        let mut measured: Vec<f64> = sounding
            .chunks_exact(frames(unit.saturating_mul(WINDOW_UNITS)))
            .map(measure)
            .collect();
        assert!(
            measured.len() >= FEWEST_WINDOWS,
            "the rig: {} windows of sound in the recording",
            measured.len()
        );
        measured.sort_by(f64::total_cmp);
        measured[measured.len() / 2]
    }

    fn level_dbfs(sounding: &[f64]) -> f64 {
        median(sounding, |window| rms_dbfs(window.iter().copied()))
    }

    /// The power of `window` at `hz`, under a raised cosine that keeps one line of the tone out of the next.
    fn power_at(window: &[f64], hz: f64) -> f64 {
        let length = f64::from(u32::try_from(window.len()).unwrap());
        let (mut real, mut imaginary) = (0.0, 0.0);
        for (at, sample) in (0_u32..).zip(window) {
            let at = f64::from(at);
            let shaped = sample * (0.5 - 0.5 * (TAU * at / length).cos());
            let turn = TAU * hz * at / f64::from(RECORDER_RATE);
            real += shaped * turn.cos();
            imaginary += shaped * turn.sin();
        }
        real * real + imaginary * imaginary
    }

    /// How far under the tone its loudest multiple stands, in dB, and which it is.
    fn loudest_harmonic(sounding: &[f64]) -> (f64, u32) {
        let tone = line_hz(TONE_LINE);
        let against_the_tone = |harmonic: u32| {
            median(sounding, |window| {
                let power = power_at(window, tone * f64::from(harmonic));
                10.0 * (power / power_at(window, tone)).log10()
            })
        };
        HARMONICS
            .map(|harmonic| (against_the_tone(harmonic), harmonic))
            .max_by(|(a, _), (b, _)| a.total_cmp(b))
            .unwrap()
    }

    /// Fails unless the recorded level is the decoder's own plus the gain.
    #[expect(clippy::print_stderr, reason = "the run's record of the level")]
    fn assert_raised_by_the_gain(sounding: &[f64], level: u8) {
        let (decoded, _) = decoded_dbfs(level);
        let recorded = level_dbfs(sounding);
        eprintln!("level {level}: decoded at {decoded:.2} dBFS, recorded at {recorded:.2} dBFS");
        let wanted = decoded + f64::from(MIC_GAIN_DB);
        assert!(
            (recorded - wanted).abs() <= GAIN_SLACK_DB,
            "recorded at {recorded:.2} dBFS; the decoder's {decoded:.2} and the gain make {wanted:.2}"
        );
    }

    #[test]
    fn a_known_level_comes_out_higher_by_the_gain() {
        let (decoded, _) = decoded_dbfs(KNOWN_LEVEL);
        assert!(
            (decoded - KNOWN_DBFS).abs() <= KNOWN_WITHIN_DB,
            "the fixture: level {KNOWN_LEVEL} decodes at {decoded:.2} dBFS"
        );
        let (rig, device, pipewire) = ready();
        let samples = recorded(&rig, &device, &pipewire, KNOWN_LEVEL);
        assert_raised_by_the_gain(&sounding(&samples), KNOWN_LEVEL);
        rig.finish();
    }

    #[test]
    #[expect(clippy::print_stderr, reason = "the run's record of the harmonics")]
    fn speech_peaks_under_the_knee_pass_untouched() {
        let (_, peak) = decoded_dbfs(SPEECH_LEVEL);
        let under = f64::from(LIMIT_KNEE_DBFS) - (peak + f64::from(MIC_GAIN_DB));
        assert!(
            UNDER_THE_KNEE_DB.contains(&under),
            "the fixture: level {SPEECH_LEVEL} peaks {under:.2} dB under the knee after the gain"
        );
        let (rig, device, pipewire) = ready();
        let samples = recorded(&rig, &device, &pipewire, SPEECH_LEVEL);
        let sounding = sounding(&samples);
        assert_raised_by_the_gain(&sounding, SPEECH_LEVEL);
        let (harmonic_db, harmonic) = loudest_harmonic(&sounding);
        eprintln!("the loudest harmonic is number {harmonic}, at {harmonic_db:.1} dB");
        assert!(
            harmonic_db <= HARMONIC_MAX_DB,
            "harmonic {harmonic} at {harmonic_db:.1} dB"
        );
        let counted = counts_after_leaving(&rig);
        assert_eq!((counted.limited, counted.nonfinite), (0, 0), "{counted:?}");
        rig.finish();
    }

    #[test]
    #[expect(clippy::print_stderr, reason = "the run's record of the peak")]
    fn a_full_scale_tap_never_wraps() {
        let (_, peak) = decoded_dbfs(TAP_LEVEL);
        assert!(
            peak >= TAP_DBFS,
            "the fixture: level {TAP_LEVEL} peaks at {peak:.2} dBFS"
        );
        let (rig, device, pipewire) = ready();
        let samples = recorded(&rig, &device, &pipewire, TAP_LEVEL);
        let recorded_peak = peak_dbfs(&parts(&samples));
        let steps = samples.array_windows::<2>();
        let step = steps.map(|[before, after]| after.abs_diff(*before)).max();
        eprintln!(
            "a tap at {peak:.2} dBFS: recorded peak {recorded_peak:.2} dBFS, largest step {step:?}"
        );
        // The ceiling and what the converter can add to it stand under full scale, so no sample is at full scale.
        let highest = f64::from(LIMIT_CEILING_DBFS) + CONVERTER_RISE_DB;
        assert!(
            highest < 0.0 && recorded_peak <= highest,
            "a peak of {recorded_peak:.2} dBFS, the ceiling and the converter allow {highest:.2}"
        );
        // A wave that wrapped would turn at every crest; this one turns as often as its tone and no more.
        let sounding = sounding(&samples);
        let checked = &sounding[..frames(CHECKED).min(sounding.len())];
        let turns = checked
            .array_windows::<2>()
            .filter(|[before, after]| (*before < 0.0) != (*after < 0.0))
            .count();
        let turns = f64::from(u32::try_from(turns).unwrap());
        let seconds = f64::from(u32::try_from(checked.len()).unwrap()) / f64::from(RECORDER_RATE);
        let most = 2.0 * line_hz(TONE_LINE) * seconds + CROSSINGS_SLACK;
        assert!(
            turns <= most,
            "{turns} zero crossings, the tone makes {most}"
        );
        let counted = counts_after_leaving(&rig);
        assert!(counted.limited > 0, "{counted:?}");
        assert_eq!(counted.nonfinite, 0, "{counted:?}");
        rig.finish();
    }

    #[test]
    fn silence_stays_silence() {
        let (_, peak) = decoded_dbfs(SILENT_LEVEL);
        assert!(
            peak < 2.0 * SILENCE_DBFS,
            "the fixture: level {SILENT_LEVEL} peaks at {peak:.2} dBFS"
        );
        let (mut rig, device, pipewire) = ready();
        let uplink = device.pods.uplink();
        uplink.set_level(SILENT_LEVEL);
        let recorder = record(&rig, &pipewire, "capture.wav");
        let before = counts(&wait_mic(&rig, "recording"));
        let daemon = rig.daemon();
        super::support::wait_for("the first audio's line", || {
            daemon.log().contains(FIRST_AUDIO_PREFIX).then_some(())
        });
        // The silent units reach the node for the whole checked span after the first of them did.
        let packets = u64::try_from(CHECKED.as_millis() / PERIOD.as_millis()).unwrap();
        wait_packets(&uplink, uplink.sent_packets() + packets);
        read_up_to_now(&rig, &device);
        recorder.wait_frames(frames(LEAD + CHECKED), HANG_LIMIT);
        let samples = recorder.finish();
        let recorded_peak = peak_dbfs(&parts(&samples));
        assert!(
            recorded_peak < SILENCE_DBFS,
            "silence recorded with a peak of {recorded_peak:.2} dBFS"
        );
        let counted = counts_after_leaving(&rig);
        assert_eq!(
            (counted.underruns, counted.concealed),
            (before.underruns, before.concealed),
            "underruns or concealment during silence"
        );
        assert_eq!((counted.limited, counted.nonfinite), (0, 0), "{counted:?}");
        rig.finish();
    }

    #[test]
    fn recorder_departures_log_each_spans_own_counters() {
        let (mut rig, device, pipewire) = ready();
        drop(recorded(&rig, &device, &pipewire, TAP_LEVEL));
        let first = wait_mic(&rig, "live");
        let total = counts(&first);
        let peak = first["mic_span_peak_dbfs"].as_f64().unwrap();
        let (_, decoded_peak) = decoded_dbfs(TAP_LEVEL);
        assert!((peak - decoded_peak).abs() <= GAIN_SLACK_DB, "{first}");
        assert!(first["mic_span_sdus"].as_u64().unwrap() > 0, "{first}");
        assert_eq!(first["mic_span_restarts"], 0);
        assert!(total.limited > 0, "{first}");
        assert_left_counts(
            rig.daemon(),
            &first,
            0,
            [total.underruns, total.concealed, total.limited],
        );

        let uplink = device.pods.uplink();
        uplink.set_level(KNOWN_LEVEL);
        // The uplink lingers between recorders, so let the loud samples pass the playout delay.
        let lead_packets = u64::try_from(LEAD.as_millis() / PERIOD.as_millis()).unwrap();
        wait_packets(&uplink, uplink.sent_packets().saturating_add(lead_packets));
        read_up_to_now(&rig, &device);
        let watch = rig.watch();
        assert_eq!(watch.next(HANG_LIMIT).unwrap().state["mic"], "live");
        let second = record(&rig, &pipewire, "second.wav");
        let start = watch.next(HANG_LIMIT).unwrap().state;
        drop(watch);
        assert_eq!(start["mic"], "recording", "{start}");
        for key in ["mic_span_sdus", "mic_span_restarts", "mic_span_gone_frames"] {
            assert_eq!(start[key], 0, "{start}");
        }
        assert!(start["mic_span_peak_dbfs"].is_null(), "{start}");
        assert_eq!(counts(&start).limited, total.limited);
        let during = rig.wait_status("the second span's audio", |status| {
            status["mic"] == "recording" && status["mic_span_sdus"].as_u64().unwrap() > 0
        });
        let peak = during["mic_span_peak_dbfs"].as_f64().unwrap();
        let (_, decoded_peak) = decoded_dbfs(KNOWN_LEVEL);
        assert!((peak - decoded_peak).abs() <= GAIN_SLACK_DB, "{during}");
        drop(second.finish());
        let end = wait_mic(&rig, "live");
        let lifetime = counts(&end);
        assert_eq!(lifetime.limited, total.limited, "{end}");
        assert_left_counts(
            rig.daemon(),
            &end,
            1,
            [
                lifetime.underruns - total.underruns,
                lifetime.concealed - total.concealed,
                0,
            ],
        );
        rig.finish();
    }
}
