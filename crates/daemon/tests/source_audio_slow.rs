//! The pods' audio through the built daemon into a recorder at real time, against the fake pods and a private `PipeWire`: the tone whole, late packets unheard, a stall, a bud taken out and ten minutes; a gap the rig itself missed fails as the rig's.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use pods_core::policy::mic::{BUD_GRACE_MS, START_TRIES, UPLINK_STALL_MS};
    use pods_fake::uplink::{PERIOD, TONE_LINE};
    use pods_mic::{NODE_LATENCY_MS, RATE_HZ, SAMPLES_PER_UNIT};
    use pods_pwrig::tone::{CHECKED, SETTLE, assert_tone};
    use pods_samples::Placed;
    use serde_json::json;

    use super::support::HANG_LIMIT;
    use super::support::pipewire::{daemon_states_latency, node_name, ready, record, wait_node};
    use super::support::recording::{
        RECORDER_RATE, WORST_GAP, assert_rig_gap, counts_after_leaving, first_sound, frames,
        gap_every, largest_step, line_hz, longest_zero_run, mark_next, marked, own_amplitude, pace,
        packets_span, span_at, span_of, tone_step_bound, wait_packets,
    };
    use super::support::uplink::{start_failures, starts, wait_mic, wait_starts};

    /// How far each window of the recording may stand from the decoder's own tone.
    const TONE_TOLERANCE_DB: f64 = 1.0;
    /// Recorded before the checked span, for the silence before the first sound and the settle.
    const LEAD: Duration = Duration::from_millis(300);
    /// The gap asked of the fake: its late wake must stay inside the worst gap measured.
    const ASKED_GAP: Duration = Duration::from_millis(70);
    const GAP_EVERY: u64 = 10;
    const GAPS_RECORDED: Duration = Duration::from_secs(30);
    /// Past the jitter cover, so the reader runs dry once.
    const PAST_THE_COVER: Duration = Duration::from_millis(150);
    /// The fake's gap past the cover may run late by what the worst gap leaves over the asked one.
    const PAST_THE_COVER_LIMIT: Duration =
        PAST_THE_COVER.saturating_add(WORST_GAP.saturating_sub(ASKED_GAP));
    /// Packets between a mark and the gap, and between the gap and the next mark.
    const AROUND_THE_GAP: u64 = 10;
    /// One unit: the decoder's overlap carries a marked unit into the next past the windows `marked` finds.
    const MARK_EDGE: Duration = Duration::from_micros(7500);
    /// Three marked packets, as the fake makes them.
    const MARKED: u32 = 3;
    /// How far a run of marked windows may stand from the span of its packets.
    const MARK_SLACK: Duration = Duration::from_millis(10);
    /// A recorder on a live uplink hears audio this soon.
    const FIRST_AUDIO_WITHIN: Duration = Duration::from_millis(100);
    /// Twice the stall check's window, so the check fires well inside the silence.
    const STALL: Duration = Duration::from_millis(UPLINK_STALL_MS.saturating_mul(2));
    const STALLED: &str = "mic: stalled, no audio for ";
    const GRACE: Duration = Duration::from_millis(BUD_GRACE_MS);
    const PAST_THE_GRACE: Duration = GRACE.saturating_add(Duration::from_secs(1));
    /// The silence a recording must end in once the audio has fallen away.
    const SILENT_END: Duration = Duration::from_millis(500);
    /// The shares of the tone's peak a fall is timed between; the lower one stands above the ripple of a few counts that follows a cut.
    const FALL_FROM: f64 = 0.5;
    const FALL_TO: f64 = 0.0625;
    const LONG_RECORDING: Duration = Duration::from_mins(10);
    /// The stock player's silence past the recording, for the setup before the window; a setup that outlasts it fails naming the rig.
    const PACE_COVER: Duration = Duration::from_mins(5);
    /// One part in a thousand of the wall time.
    const LENGTH_PARTS: u32 = 1000;
    const NANOS_PER_MILLI: u64 = 1_000_000;

    #[test]
    #[ignore = "a tone judged window by window at real time; the slow job runs it"]
    fn a_recorder_gets_the_pods_tone() {
        let (rig, device, pipewire) = ready();
        let recorder = record(&rig, &pipewire, "capture.wav");
        wait_mic(&rig, "recording");
        recorder.wait_frames(frames(LEAD + SETTLE + CHECKED), HANG_LIMIT);
        let samples = recorder.finish();
        assert_rig_gap(&device.pods.uplink(), WORST_GAP);
        let amplitude = own_amplitude();
        assert_tone(
            &samples,
            RECORDER_RATE,
            line_hz(TONE_LINE),
            amplitude,
            TONE_TOLERANCE_DB,
        );
        rig.finish();
    }

    #[test]
    #[ignore = "the first buffers of a recorder at real time; the slow job runs it"]
    fn a_recorder_on_a_live_uplink_has_audio_in_its_first_100_ms() {
        let (rig, device, pipewire) = ready();
        let first = record(&rig, &pipewire, "first.wav");
        wait_mic(&rig, "recording");
        first.wait_frames(frames(LEAD), HANG_LIMIT);
        drop(first.finish());
        wait_mic(&rig, "live");
        let second = record(&rig, &pipewire, "second.wav");
        wait_mic(&rig, "recording");
        second.wait_frames(frames(LEAD), HANG_LIMIT);
        let samples = second.finish();
        assert_rig_gap(&device.pods.uplink(), WORST_GAP);
        let heard_at = span_of(first_sound(&samples));
        assert!(
            heard_at < FIRST_AUDIO_WITHIN,
            "the first sound at {heard_at:?}"
        );
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        rig.finish();
    }

    #[test]
    #[ignore = "the place of marked audio at real time; the slow job runs it"]
    fn a_marked_burst_arrives_once_and_in_order() {
        let (rig, device, pipewire) = ready();
        let uplink = device.pods.uplink();
        let recorder = record(&rig, &pipewire, "capture.wav");
        wait_mic(&rig, "recording");
        recorder.wait_frames(frames(LEAD), HANG_LIMIT);
        uplink.mark(MARKED);
        wait_packets(
            &uplink,
            uplink.sent_packets() + u64::from(MARKED) + AROUND_THE_GAP,
        );
        let samples = recorder.finish();
        assert_rig_gap(&uplink, WORST_GAP);
        let runs = marked(&samples);
        let [run] = runs.as_slice() else {
            panic!("one run of the second tone, not {runs:?}");
        };
        let span = span_of(run.len());
        let wanted = packets_span(u64::from(MARKED));
        assert!(
            span.abs_diff(wanted) <= MARK_SLACK,
            "the second tone for {span:?}, its packets hold {wanted:?}"
        );
        // Sound that is not the mark stands on both sides of it, with no unit of silence anywhere.
        assert!(run.start > first_sound(&samples), "{run:?}");
        let last = samples.iter().rposition(|&sample| sample != 0).unwrap();
        assert!(
            run.end + frames(PERIOD) < last,
            "{run:?}, the last sound at {last}"
        );
        let zeros = longest_zero_run(&samples);
        assert!(
            zeros < usize::from(SAMPLES_PER_UNIT),
            "a run of {zeros} zeros"
        );
        rig.finish();
    }

    #[test]
    #[ignore = "thirty seconds of late packets at real time; the slow job runs it"]
    fn the_worst_measured_gap_is_not_heard() {
        let (mut rig, device, pipewire) = ready();
        let uplink = device.pods.uplink();
        let recorder = record(&rig, &pipewire, "capture.wav");
        wait_mic(&rig, "recording");
        recorder.wait_frames(frames(LEAD), HANG_LIMIT);
        let packets = u64::try_from(GAPS_RECORDED.as_millis() / PERIOD.as_millis()).unwrap();
        gap_every(&uplink, GAP_EVERY, ASKED_GAP, packets);
        let samples = recorder.finish();
        let gap = assert_rig_gap(&uplink, WORST_GAP);
        assert!(
            gap >= ASKED_GAP,
            "the fake made no gap of {ASKED_GAP:?}: {gap:?}"
        );
        let counted = counts_after_leaving(&rig);
        assert_eq!(counted.underruns, 0, "{counted:?}");
        let zeros = longest_zero_run(&samples);
        assert!(
            zeros < usize::from(SAMPLES_PER_UNIT),
            "a run of {zeros} zeros"
        );
        let node = wait_node(&pipewire);
        if daemon_states_latency(rig.daemon().pid()) {
            let ns = u64::from(NODE_LATENCY_MS) * NANOS_PER_MILLI;
            let stated = json!([{"quantum": 0.0, "rate": 0, "ns": ns}]);
            assert_eq!(node.pointer("/info/params/ProcessLatency"), Some(&stated));
        }
        rig.finish();
    }

    #[test]
    #[ignore = "one gap past the cover at real time; the slow job runs it"]
    fn a_gap_past_the_cover_is_one_underrun_and_no_step_to_zero() {
        let (rig, device, pipewire) = ready();
        let uplink = device.pods.uplink();
        let recorder = record(&rig, &pipewire, "capture.wav");
        wait_mic(&rig, "recording");
        recorder.wait_frames(frames(LEAD), HANG_LIMIT);
        let before = mark_next(&uplink);
        wait_packets(&uplink, before + AROUND_THE_GAP);
        uplink.stall(PAST_THE_COVER);
        wait_packets(&uplink, before + 2 * AROUND_THE_GAP);
        let after = mark_next(&uplink);
        wait_packets(&uplink, after + AROUND_THE_GAP);
        let samples = recorder.finish();
        let gap = assert_rig_gap(&uplink, PAST_THE_COVER_LIMIT);
        assert!(
            gap >= PAST_THE_COVER,
            "the fake made no gap of {PAST_THE_COVER:?}: {gap:?}"
        );
        let counted = counts_after_leaving(&rig);
        assert_eq!(counted.underruns, 1, "{counted:?}");
        let runs = marked(&samples);
        let [first, second] = runs.as_slice() else {
            panic!("two marked packets, one each side of the gap, not {runs:?}");
        };
        let edge = frames(MARK_EDGE);
        let gap_steps = largest_step(&samples[first.end + edge..second.start - edge]);
        let bound = tone_step_bound();
        assert!(
            gap_steps <= bound,
            "a step of {gap_steps}, the tone's own reach {bound}"
        );
        // Nothing after the gap was delayed: the marks stand as far apart as their packets.
        let apart = span_of(second.start - first.start);
        let wanted = packets_span(after - before);
        assert!(
            apart.abs_diff(wanted) <= MARK_SLACK,
            "the marks {apart:?} apart, their packets {wanted:?}"
        );
        rig.finish();
    }

    #[test]
    #[ignore = "two seconds without audio at real time; the slow job runs it"]
    fn no_audio_for_two_seconds_is_a_stall_and_start_is_sent_again() {
        let (mut rig, device, pipewire) = ready();
        let uplink = device.pods.uplink();
        let recorder = record(&rig, &pipewire, "capture.wav");
        wait_mic(&rig, "recording");
        recorder.wait_frames(frames(LEAD), HANG_LIMIT);
        let first = wait_starts(&device, 1);
        let asked = Instant::now();
        uplink.stall(STALL);
        let sent = uplink.sent_packets();
        let again = wait_starts(&device, 2);
        assert!(again[1].at > asked, "{again:?}");
        wait_packets(&uplink, sent + 1);
        drop(recorder.finish());
        let daemon = rig.daemon();
        let log = super::support::wait_for("the stall's line", || {
            let log = daemon.log();
            log.lines()
                .any(|line| line.starts_with(STALLED))
                .then_some(log)
        });
        for line in log.lines().filter(|line| line.starts_with(STALLED)) {
            let ms: u64 = line
                .strip_prefix(STALLED)
                .unwrap()
                .strip_suffix(" ms")
                .unwrap()
                .parse()
                .unwrap();
            assert!(ms >= UPLINK_STALL_MS, "{line}");
        }
        let heard = starts(&device).len();
        assert!(
            heard <= usize::try_from(START_TRIES).unwrap(),
            "{heard} STARTs"
        );
        assert!(start_failures(&rig) >= 1);
        assert_ne!(super::support::uplink::mic(&rig), "failed");
        assert_eq!(first.len(), 1);
        rig.finish();
    }

    #[test]
    #[ignore = "the grace of a bud taken out at real time; the slow job runs it"]
    fn a_bud_out_while_recording_fades_to_silence_and_keeps_the_node() {
        let (mut rig, device, pipewire) = ready();
        let recorder = record(&rig, &pipewire, "capture.wav");
        wait_mic(&rig, "recording");
        recorder.wait_frames(frames(LEAD + SETTLE), HANG_LIMIT);
        device.pods.set_ear(Placed::InCase, Placed::InCase);
        rig.holds_for(PAST_THE_GRACE, "the node kept", |status| {
            status["mic_source"] == node_name()
        });
        let samples = recorder.finish();
        assert_rig_gap(&device.pods.uplink(), WORST_GAP);
        let tail = samples.len() - frames(SILENT_END);
        assert!(
            samples[tail..].iter().all(|&sample| sample == 0),
            "no silence at the end"
        );
        let fade = largest_step(&samples[first_sound(&samples) + frames(SETTLE)..]);
        let bound = tone_step_bound();
        assert!(
            fade <= bound,
            "a step of {fade}, the tone's own reach {bound}"
        );
        // A cut falls between the two shares within a few samples wherever it lands; the reader's fall over one unit takes about a third of it.
        let peak = own_amplitude() * f64::from(i16::MAX);
        let last_above = |share: f64| {
            samples
                .iter()
                .rposition(|&sample| f64::from(sample.unsigned_abs()) > peak * share)
                .unwrap()
        };
        let fall = span_of(last_above(FALL_TO) - last_above(FALL_FROM));
        let period = Duration::from_secs_f64(1.0 / line_hz(TONE_LINE));
        let unit = span_at(u64::from(SAMPLES_PER_UNIT), RATE_HZ);
        assert!(
            period <= fall && fall <= unit,
            "the sound fell from {FALL_FROM} of its peak to {FALL_TO} in {fall:?}, not between the tone's period of {period:?} and a unit of {unit:?}"
        );
        let log = rig.daemon().log();
        assert!(!log.contains(STALLED), "a stall with no bud worn: {log}");
        rig.finish();
    }

    #[test]
    #[ignore = "ten minutes of recording at real time; the slow job runs it"]
    fn ten_minutes_of_recording_has_no_underrun_and_the_right_length() {
        let (rig, device, pipewire) = ready();
        let mut pace = pace(&rig, &pipewire, LONG_RECORDING.saturating_add(PACE_COVER));
        let recorder = record(&rig, &pipewire, "capture.wav");
        wait_mic(&rig, "recording");
        recorder.wait_frames(1, HANG_LIMIT);
        pace.recorder.wait_frames(1, HANG_LIMIT);
        let (ours, paced) = (recorder.frames_on_disk(), pace.recorder.frames_on_disk());
        let started = Instant::now();
        let wanted = usize::try_from(ours).unwrap() + frames(LONG_RECORDING);
        recorder.wait_frames(wanted, LONG_RECORDING + HANG_LIMIT);
        let wall = started.elapsed();
        if let Some(status) = pace.player.wait(Duration::ZERO) {
            panic!(
                "the rig failed: the stock player, {PACE_COVER:?} longer than the recording, ended inside the window with {status}: {}",
                pace.player.log()
            );
        }
        let length = span_of(usize::try_from(recorder.frames_on_disk() - ours).unwrap());
        let graph = span_at(pace.recorder.frames_on_disk() - paced, pace.recorder.rate());
        let slack = wall / LENGTH_PARTS;
        assert!(
            graph.abs_diff(wall) <= slack,
            "the rig missed its timing: the graph gave a stock player {graph:?} over {wall:?}, so this run cannot judge the daemon"
        );
        assert!(
            length.abs_diff(wall) <= slack,
            "{length:?} of audio over {wall:?}"
        );
        let samples = recorder.finish();
        assert_rig_gap(&device.pods.uplink(), WORST_GAP);
        let counted = counts_after_leaving(&rig);
        assert_eq!(counted.underruns, 0, "{counted:?}");
        let zeros = longest_zero_run(&samples);
        assert!(
            zeros < usize::from(SAMPLES_PER_UNIT),
            "a run of {zeros} zeros"
        );
        rig.finish();
    }
}
