//! Dictation through link loss, bearer loss and the logind sleep edge.

#[cfg(test)]
#[expect(dead_code, reason = "each suite uses part of the shared support")]
mod support;

#[cfg(test)]
mod tests {
    use std::f64::consts::{PI, TAU};
    use std::process::ExitStatus;
    use std::time::{Duration, Instant};

    use pods_core::policy::dictation::{
        BEARER_RETRY_MS, DOAP_STOP_ANSWER_MS, INHIBIT_WAIT_MS, MODE_SETTLE_MS,
    };
    use pods_fake::doap::FRAME;
    use pods_proto::doap::{STOP_REASON_UNSOLICITED, doap_mode_off, doap_mode_on};
    use pods_samples::opus::TONE_HZ;
    use pods_samples::uplink::START;

    use super::support::dictation::{CAPTURE, Fixture, HOST_STOP, opening, ready_to_read, status};
    use super::support::logind::INHIBIT;
    use super::support::pipewire::{record, wait_no_node};
    use super::support::recording::{
        assert_sounds, first_sound, frames, frames_at as at_rate, longest_zero_run, marked, span_at,
    };
    use super::support::sleep::{WATCHING, inhibitor_closed, logged, sleep, wake};
    use super::support::uplink::{assert_no_sink_call, lose};
    use super::support::{HANG_LIMIT, pause, wait_for, wait_within};

    const STOP_TIMEOUT: &str = "dictation: stop not confirmed";
    const INHIBIT_FAILED: &str = "dictation: cannot inhibit sleep via logind";
    const INHIBIT_TIMEOUT: &str = "dictation: logind inhibitor timed out, starting without it";
    const STOP_WAIT: Duration = Duration::from_millis(DOAP_STOP_ANSWER_MS);
    const MODE_SETTLE: Duration = Duration::from_millis(MODE_SETTLE_MS);
    const INHIBIT_WAIT: Duration = Duration::from_millis(INHIBIT_WAIT_MS);
    const AFTER_SETTLE: Duration = Duration::from_millis(MODE_SETTLE_MS + INHIBIT_WAIT_MS / 5);
    const AUDIO: Duration = Duration::from_millis(400);
    const DECODER_SETTLE: Duration = FRAME.saturating_mul(2);

    fn wait_for_inhibitor(fixture: &Fixture) {
        wait_for("the open logind delay inhibitor", || {
            let logind = fixture.rig.logind();
            (logind.inhibitors() == [None] && logind.method_calls() == [INHIBIT]).then_some(())
        });
    }

    fn mode_set_at(fixture: &Fixture) -> Instant {
        wait_for("DoAP mode on", || {
            fixture
                .device
                .pods
                .heard()
                .into_iter()
                .find_map(|heard| (heard.sdu == doap_mode_on()).then_some(heard.at))
        })
    }

    fn past_mode_settle(mode_at: Instant) {
        pause(AFTER_SETTLE.saturating_sub(mode_at.elapsed()));
    }

    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "a value inside full scale, rounded to the nearest sample"
    )]
    fn control_tone(frames: usize, rate: u32, hz: u32, peak: f64) -> Vec<i16> {
        let radians_per_sample = TAU * f64::from(hz) / f64::from(rate);
        (0..frames)
            .map(|index| {
                let phase = f64::from(u32::try_from(index).unwrap()) * radians_per_sample;
                (phase.sin() * peak).round() as i16
            })
            .collect()
    }

    fn check_stock_player(
        exit: Option<ExitStatus>,
        samples: &[i16],
        start: usize,
        minimum_frames: usize,
        bound: f64,
        rate: u32,
    ) -> Result<u16, String> {
        if exit.is_some() {
            return Err(format!(
                "the stock player ended inside the window with {exit:?}"
            ));
        }
        let window = samples.get(start..).unwrap_or_default();
        if window.len() < minimum_frames || window.iter().all(|&sample| sample == 0) {
            return Err(format!(
                "the stock player recording lacks {minimum_frames} frames or is silent in the window"
            ));
        }
        let step = RecordedStep::largest(samples, first_sound(samples), start);
        if f64::from(step.size()) > bound {
            return Err(format!(
                "the stock player step {} exceeds {bound:.1}\nstock player: {}",
                step.size(),
                step.describe(rate)
            ));
        }
        Ok(step.size())
    }

    struct RecordedStep {
        first: usize,
        index: usize,
        before: i16,
        after: i16,
    }

    impl RecordedStep {
        fn history(name: &str, samples: &[i16], rate: u32, caught_up: u32) -> Self {
            let zeroes = longest_zero_run(samples);
            assert!(
                zeroes < usize::try_from(rate / TONE_HZ).unwrap(),
                "{name} has a gap of {zeroes} zero samples"
            );
            // First sound anchors opening on each recorder's clock.
            let first = first_sound(samples);
            let last = samples.iter().rposition(|&sample| sample != 0).unwrap();
            let catch_up = Duration::from_millis(u64::from(caught_up));
            let required = at_rate(catch_up.saturating_add(AUDIO), rate);
            assert!(
                first.saturating_add(required) <= last,
                "{name} lacks {AUDIO:?} of tone after catch-up at {caught_up} ms"
            );
            let start = first.saturating_add(at_rate(DECODER_SETTLE, rate));
            Self::largest(samples, first, start)
        }

        fn largest(samples: &[i16], first: usize, start: usize) -> Self {
            let (offset, &[before, after]) = samples[start..]
                .array_windows::<2>()
                .enumerate()
                .max_by_key(|(_, [before, after])| after.abs_diff(*before))
                .unwrap();
            Self {
                first,
                index: start.saturating_add(offset).saturating_add(1),
                before,
                after,
            }
        }

        fn size(&self) -> u16 {
            self.after.abs_diff(self.before)
        }

        fn describe(&self, rate: u32) -> String {
            let recording_offset = span_at(u64::try_from(self.index).unwrap(), rate);
            let after_first = self.index.saturating_sub(self.first);
            let sound_offset = span_at(u64::try_from(after_first).unwrap(), rate);
            format!(
                "index {} at {recording_offset:?} from WAV start, {sound_offset:?} after first sound: {} -> {}",
                self.index, self.before, self.after
            )
        }
    }

    fn stream_history_opening(fixture: &Fixture) {
        // Keep enough pre-roll to force catch-up across the rig's 2048-frame quantum.
        const OPENING_PRE_ROLL: Duration = FRAME.saturating_mul(8);
        let mode = mode_set_at(fixture);
        fixture.start(1);
        pause(OPENING_PRE_ROLL.saturating_sub(mode.elapsed()));
        fixture.device.pods.doap().release_start();
        fixture.streaming(1);
    }

    #[test]
    #[expect(
        clippy::print_stderr,
        reason = "nextest captures the evidence for the retry"
    )]
    fn history_recorder_stays_click_free_across_opening_catchup_exit() {
        const MAX_ATTEMPTS: usize = 2;
        for attempt in 1..=MAX_ATTEMPTS {
            let Some(detail) = history_rig_miss() else {
                return;
            };
            assert!(attempt < MAX_ATTEMPTS, "attempt {attempt}: {detail}");
            eprintln!("attempt {attempt}: {detail}; retrying on a fresh server");
        }
    }

    fn history_rig_miss() -> Option<String> {
        use pods_mic::OPENING_MAX_PPM;
        use pods_mic::level::MIC_GAIN_DB;
        use pods_pwrig::Recorder;
        use pods_samples::opus::TONE_DBFS;

        use super::support::pipewire::node_name;
        use super::support::recording::{Pace, counts_after_leaving, record_stock_player};

        const SPEECH: Duration = Duration::from_secs(3);
        // Setup and close waits of a slow runner can pass HANG_LIMIT, and a tone that ran out there would blame the rig for its own length.
        const CONTROL_COVER: Duration = HANG_LIMIT.saturating_mul(4);
        const PARTS_PER_MILLION: f64 = 1_000_000.0;
        // Allow for the decoder and resampler's deviation from a pure sine.
        const STEP_TOLERANCE: f64 = 1.20;
        let mut fixture = Fixture::new();
        fixture.device.pods.doap().hold_start(true);
        let node = node_name();
        let dir = fixture.rig.session().dir.path();
        let history = Recorder::history(&fixture.server, &node, &dir.join("history.wav"));
        let witness = Recorder::program(&fixture.server, &node, &dir.join("witness.wav"));
        let rate = history.rate();
        let peak = 10_f64.powf(f64::from(TONE_DBFS + MIC_GAIN_DB) / 20.0) * f64::from(i16::MAX);
        let cover = at_rate(SPEECH.saturating_add(CONTROL_COVER), rate);
        let tone = control_tone(cover, rate, TONE_HZ, peak);
        let Pace {
            mut player,
            recorder: stock_recorder,
        } = record_stock_player(&fixture.rig, &fixture.server, rate, &tone);
        ready_to_read(&stock_recorder, AUDIO);
        // Both 48 kHz recorders must be linked before the listed capture starts DoAP.
        fixture.server.link(&node, history.node());
        fixture.server.link(&node, witness.node());
        let control_start = usize::try_from(stock_recorder.frames_on_disk()).unwrap();
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        stream_history_opening(&fixture);
        pause(SPEECH);
        let history_samples = history.finish();
        let witness_samples = witness.finish();
        let capture = recorder.finish();
        let control_samples = stock_recorder.finish();
        let player_exit = player.wait(Duration::ZERO);
        drop(player);
        fixture.stopped(1);
        let (behind, caught_up) = opening(&mut fixture);
        assert!(
            behind > 0,
            "history recorder did not exercise dictation opening"
        );
        let Some(caught_up) = caught_up else {
            panic!("the reader never caught up");
        };
        assert_sounds(&capture, "");
        let underruns = counts_after_leaving(&fixture.rig).underruns;
        assert_eq!(underruns, 0);
        let catch_up = Duration::from_millis(u64::from(caught_up));
        assert!(
            catch_up > DECODER_SETTLE,
            "catch-up after {caught_up} ms overlaps decoder settle"
        );
        let recordings = [("history", &history_samples), ("witness", &witness_samples)];
        let [history_step, witness_step] =
            recordings.map(|(name, samples)| RecordedStep::history(name, samples, rate, caught_up));
        let fastest_tone = f64::from(TONE_HZ) * (1.0 + OPENING_MAX_PPM / PARTS_PER_MILLION);
        let bound = 2.0 * (PI * fastest_tone / f64::from(rate)).sin() * peak * STEP_TOLERANCE;
        // A missed quantum in one recorder can also make a large step.
        let outcome = if f64::from(history_step.size().min(witness_step.size())) > bound {
            let detail = format!(
                "history step {} and witness step {} exceed {bound:.1} (opening {behind} ms behind, caught up after {caught_up} ms, underruns {underruns}, largest DoAP gap {:?})\nhistory: {}\nwitness: {}",
                history_step.size(),
                witness_step.size(),
                fixture.device.pods.doap().largest_gap(),
                history_step.describe(rate),
                witness_step.describe(rate)
            );
            let minimum = at_rate(SPEECH, rate);
            match check_stock_player(
                player_exit,
                &control_samples,
                control_start,
                minimum,
                bound,
                rate,
            ) {
                Ok(step) => panic!(
                    "the daemon produced a discontinuity with a clean stock player (largest step {step})\n{detail}"
                ),
                Err(reason) => Some(format!(
                    "the rig missed its timing, so this run cannot judge the daemon: {reason}\n{detail}"
                )),
            }
        } else {
            None
        };
        fixture.no_uplink();
        fixture.finish();
        outcome
    }

    #[test]
    fn the_channel_lost_mid_dictation_keeps_recording_on_the_uplink() {
        const FIRST_RETRY: Duration = Duration::from_millis(BEARER_RETRY_MS[0]);
        let mut fixture = Fixture::new();
        let pods = &fixture.device.pods;
        let doap = pods.doap();
        pods.uplink().mark(u32::MAX);
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        fixture.streaming(1);
        ready_to_read(&recorder, AUDIO);
        let lost_at = usize::try_from(recorder.frames_on_disk()).unwrap();
        assert!(doap.streaming());
        pods.hang_up_att();

        let wire = wait_within("0x58 START after bearer loss", FIRST_RETRY, || {
            let heard = pods.heard();
            heard
                .iter()
                .any(|packet| packet.sdu == START)
                .then_some(heard)
        });
        let start = wire.iter().position(|packet| packet.sdu == START).unwrap();
        assert!(
            wire[..start]
                .iter()
                .any(|packet| packet.sdu == doap_mode_off()),
            "0x58 START reached the pods before DoAP mode cleared"
        );
        status(&fixture.rig, "armed");
        assert_eq!(doap.selections(), 2);
        let rearmed_at = usize::try_from(recorder.frames_on_disk()).unwrap();
        recorder.wait_frames(rearmed_at.saturating_add(frames(AUDIO)), HANG_LIMIT);
        assert_eq!(doap.starts(), 1);
        assert!(!doap.streaming());

        let samples = recorder.finish();
        assert_sounds(&samples[..lost_at], " before bearer loss");
        assert!(
            !marked(&samples[rearmed_at..]).is_empty(),
            "the recorder did not receive the uplink's marked tone after re-arm"
        );
        let zeroes = longest_zero_run(&samples);
        assert!(
            zeroes < frames(FIRST_RETRY),
            "a gap of {zeroes} zero samples after bearer loss"
        );
        fixture
            .rig
            .wait_logged("dictation: this recording takes the uplink");
        fixture
            .rig
            .wait_logged("dictation: bearer lost, arming again");
        fixture.finish();
    }

    #[test]
    fn buds_out_keeps_talking_dictation_then_withdraws_on_silence() {
        use pods_core::policy::mic::{BUD_GRACE_MS, withdrawn_line};

        use super::support::pipewire::node_name;
        use super::support::uplink::take_out;

        const PAST_THE_GRACE: Duration =
            Duration::from_millis(BUD_GRACE_MS).saturating_add(Duration::from_secs(1));
        let mut fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        fixture.streaming(1);
        ready_to_read(&recorder, AUDIO);
        take_out(&fixture.rig, &fixture.device);
        fixture
            .rig
            .holds_for(PAST_THE_GRACE, "DoAP keeps the node", |status| {
                status["mic_source"] == node_name() && status["mic_dictation"] == "streaming"
            });
        doap.silent(u32::MAX);
        wait_no_node(&fixture.server);
        drop(recorder.finish());
        fixture.rig.wait_logged(&withdrawn_line("silent"));
        fixture.no_uplink();
        fixture.finish();
    }

    #[test]
    fn the_link_lost_mid_dictation_ends_the_recorder_cleanly() {
        let fixture = Fixture::new();
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        fixture.streaming(1);
        ready_to_read(&recorder, AUDIO);
        wait_for("the open logind delay inhibitor", || {
            (fixture.rig.logind().inhibitors() == [None]).then_some(())
        });
        let lost = Instant::now();
        lose(&fixture.rig, &fixture.device);
        assert!(inhibitor_closed(&fixture.rig, 0) >= lost);
        wait_no_node(&fixture.server);
        status(&fixture.rig, "off");
        assert_sounds(&recorder.finish(), "");
        assert_eq!(fixture.device.pods.doap().stops(), [0_u8; 0]);
        fixture.no_uplink();
        assert_eq!(fixture.rig.logind().method_calls(), [INHIBIT]);
        fixture.finish();
    }

    #[test]
    fn sleep_during_mode_set_with_a_pending_inhibitor_clears_the_mode() {
        sleep_with_a_pending_inhibitor(false);
    }

    #[test]
    fn sleep_while_start_waits_for_logind_cancels_it() {
        sleep_with_a_pending_inhibitor(true);
    }

    fn sleep_with_a_pending_inhibitor(after_settle: bool) {
        let mut fixture = Fixture::new();
        fixture.rig.wait_logged(WATCHING);
        let doap = fixture.device.pods.doap();
        let grant = fixture.rig.logind().hold_inhibit();
        fixture.list(CAPTURE);
        let recorder = record(&fixture.rig, &fixture.server, "dictation.wav");
        let mode_at = mode_set_at(&fixture);
        wait_for("the pending Inhibit", || {
            (fixture.rig.logind().method_calls() == [INHIBIT]).then_some(())
        });
        if after_settle {
            past_mode_settle(mode_at);
        }

        let asleep = sleep(&mut fixture.rig);
        assert_eq!(doap.starts(), 0, "START reached the pods before sleep");
        let mode_off = wait_within("mode off after sleep", MODE_SETTLE, || {
            fixture
                .device
                .pods
                .heard()
                .into_iter()
                .find_map(|heard| (heard.sdu == doap_mode_off()).then_some(heard.at))
        });
        assert_eq!(doap.starts(), 0, "START was sent after sleep canceled it");
        fixture.stopped(0);
        assert!(mode_off >= asleep, "mode cleared before the sleep edge");
        assert_eq!(
            fixture.rig.logind().method_calls(),
            [INHIBIT],
            "mode settle sent no Inhibit before being interrupted by sleep"
        );
        assert_eq!(fixture.rig.logind().inhibitors(), [None; 0]);
        grant.send(()).unwrap();
        // The held reply grants only after mode off, so this waits for the late grant to close.
        inhibitor_closed(&fixture.rig, 0);
        fixture
            .rig
            .holds_for(MODE_SETTLE, "DoAP stays armed after sleep", |status| {
                status["mic_dictation"] == "armed"
            });
        assert_eq!(doap.starts(), 0, "START across sleep with Inhibit pending");
        drop(recorder.finish());
        fixture.no_uplink();
        fixture.finish();
    }

    #[test]
    fn a_grant_after_settling_releases_start_at_once() {
        let mut fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        let grant = fixture.rig.logind().hold_inhibit();
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        let mode_at = mode_set_at(&fixture);
        wait_within("Inhibit at mode set before START", MODE_SETTLE / 2, || {
            let calls = fixture.rig.logind().method_calls();
            assert_eq!(doap.starts(), 0, "START reached the pods before Inhibit");
            (calls == [INHIBIT]).then_some(())
        });

        past_mode_settle(mode_at);
        assert_eq!(doap.starts(), 0, "START while logind still held the reply");
        grant.send(()).unwrap();
        wait_within("START after the granted Inhibit", INHIBIT_WAIT / 2, || {
            (doap.starts() >= 1).then_some(())
        });
        fixture.streaming(1);
        wait_for_inhibitor(&fixture);
        drop(recorder.finish());
        fixture.stopped(1);
        assert_eq!(
            logged(&mut fixture.rig, INHIBIT_TIMEOUT),
            0,
            "START came from the timeout, not the grant"
        );
        inhibitor_closed(&fixture.rig, 0);
        fixture.no_uplink();
        fixture.finish();
    }

    #[test]
    fn the_inhibitor_attempt_survives_a_retry_and_keeps_its_late_grant() {
        let fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        doap.refuse_starts(1);
        let grant = fixture.rig.logind().hold_inhibit();
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        fixture.streaming(2);
        assert_eq!(fixture.mode_writes(), 2);
        assert_eq!(fixture.rig.logind().method_calls(), [INHIBIT]);
        assert_eq!(fixture.rig.logind().inhibitors(), []);
        grant.send(()).unwrap();
        wait_for_inhibitor(&fixture);
        let left = Instant::now();
        drop(recorder.finish());
        fixture.stopped(1);
        let closed = inhibitor_closed(&fixture.rig, 0);
        assert!(closed >= left, "late grant closed before the recorder left");
        assert_eq!(fixture.rig.logind().inhibitors(), [Some(closed)]);
        fixture.no_uplink();
        fixture.finish();
    }

    #[test]
    fn a_pending_inhibitor_times_out_and_starts_once() {
        let mut fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        let _held_reply = fixture.rig.logind().hold_inhibit();
        fixture.list(CAPTURE);
        let recorder = record(&fixture.rig, &fixture.server, "dictation.wav");
        let mode_at = mode_set_at(&fixture);
        past_mode_settle(mode_at);
        assert_eq!(doap.starts(), 0, "START before the inhibitor bound");
        wait_within("START at the inhibitor timeout", INHIBIT_WAIT * 2, || {
            (doap.starts() == 1).then_some(())
        });
        fixture.streaming(1);
        assert_eq!(fixture.rig.logind().method_calls(), [INHIBIT]);
        assert_eq!(fixture.rig.logind().inhibitors(), []);
        drop(recorder.finish());
        fixture.stopped(1);
        assert_eq!(logged(&mut fixture.rig, INHIBIT_TIMEOUT), 1);
        fixture.no_uplink();
        fixture.finish();
    }

    #[test]
    fn sleep_while_dictating_stops_before_the_suspend() {
        let mut fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        doap.hold_stop();
        fixture.list(CAPTURE);
        let recorder = record(&fixture.rig, &fixture.server, "dictation.wav");
        fixture.streaming(1);
        wait_for_inhibitor(&fixture);
        let asleep = sleep(&mut fixture.rig);
        wait_for("sleep STOP", || (doap.stops() == [HOST_STOP]).then_some(()));
        status(&fixture.rig, "stopping");
        assert_eq!(fixture.rig.logind().inhibitors(), [None]);
        let released = Instant::now();
        doap.release_stop();
        let closed = inhibitor_closed(&fixture.rig, 0);
        assert!(
            closed >= released,
            "inhibitor closed before the STOP answer"
        );
        assert!(
            closed < asleep + STOP_WAIT,
            "inhibitor held until the deadline"
        );
        fixture.stopped(1);
        fixture.rig.wait_logged("dictation: stop sent (sleep)");
        assert_eq!(logged(&mut fixture.rig, STOP_TIMEOUT), 0);
        assert_no_sink_call(&fixture.rig, &fixture.audio);
        assert_eq!(fixture.device.pods.doap().stops(), [HOST_STOP]);
        lose(&fixture.rig, &fixture.device);
        drop(recorder.finish());
        fixture.no_uplink();
        fixture.finish();
    }

    #[test]
    fn sleep_during_start_holds_the_inhibitor_until_the_stream_ends() {
        let mut fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        doap.hold_start(true);
        doap.hold_stop();
        fixture.list(CAPTURE);
        let recorder = record(&fixture.rig, &fixture.server, "dictation.wav");
        fixture.start(1);
        status(&fixture.rig, "starting");
        wait_for_inhibitor(&fixture);

        sleep(&mut fixture.rig);
        status(&fixture.rig, "stopping");
        doap.release_start();
        wait_for("sleep STOP", || (doap.stops() == [HOST_STOP]).then_some(()));
        assert_eq!(
            fixture.rig.logind().inhibitors(),
            [None],
            "inhibitor closed before the streaming end"
        );

        let released = Instant::now();
        doap.release_stop();
        let closed = inhibitor_closed(&fixture.rig, 0);
        assert!(
            closed >= released,
            "inhibitor closed before the streaming end"
        );
        fixture.stopped(1);
        fixture.rig.wait_logged("dictation: stop sent (sleep)");
        let log = fixture.rig.daemon().log();
        let started = log
            .find("dictation: started after ")
            .expect("START answer logged");
        let stopped = log
            .find("dictation: stop sent (sleep)")
            .expect("sleep STOP logged");
        assert!(started < stopped, "STOP sent before the START answer");
        assert_eq!(logged(&mut fixture.rig, STOP_TIMEOUT), 0);
        assert_eq!(fixture.rig.logind().method_calls(), [INHIBIT]);
        assert_eq!(fixture.rig.logind().inhibitors(), [Some(closed)]);
        lose(&fixture.rig, &fixture.device);
        drop(recorder.finish());
        fixture.finish();
    }

    #[test]
    fn a_held_sleep_stop_closes_the_inhibitor_at_the_answer_deadline() {
        const SCHEDULING_SLACK: Duration = Duration::from_millis(150);
        const HELD_WINDOW: Duration = Duration::from_millis(200);
        let mut fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        doap.hold_stop();
        fixture.list(CAPTURE);
        let recorder = record(&fixture.rig, &fixture.server, "dictation.wav");
        fixture.streaming(1);
        wait_for_inhibitor(&fixture);
        let asleep = sleep(&mut fixture.rig);
        wait_for("sleep STOP", || (doap.stops() == [HOST_STOP]).then_some(()));
        let sent = Instant::now();
        wait_for("the held STOP window", || {
            assert_eq!(doap.stops(), [HOST_STOP], "a STOP inside the held window");
            assert!(
                doap.mode_seen(),
                "mode was cleared with the STOP still held"
            );
            (sent.elapsed() >= HELD_WINDOW).then_some(())
        });
        assert_eq!(fixture.rig.logind().method_calls(), [INHIBIT]);
        assert_eq!(fixture.rig.logind().inhibitors(), [None]);
        let closed = inhibitor_closed(&fixture.rig, 0);
        assert!(
            closed.saturating_duration_since(asleep) >= STOP_WAIT,
            "inhibitor closed before the deadline"
        );
        assert!(
            closed.saturating_duration_since(sent) <= STOP_WAIT.saturating_add(SCHEDULING_SLACK),
            "inhibitor held past the deadline"
        );
        status(&fixture.rig, "armed");
        wait_for("DoAP mode to clear after the held STOP", || {
            (!doap.mode_seen()).then_some(())
        });
        // Audio already in flight on ATT can still draw one recovery STOP.
        let stops = doap.stops();
        assert!(
            matches!(
                stops.as_slice(),
                [HOST_STOP] | [HOST_STOP, STOP_REASON_UNSOLICITED]
            ),
            "unexpected STOPs after the answer deadline: {stops:?}"
        );
        fixture.rig.wait_logged(STOP_TIMEOUT);
        assert_eq!(logged(&mut fixture.rig, STOP_TIMEOUT), 1);
        lose(&fixture.rig, &fixture.device);
        drop(recorder.finish());
        fixture.finish();
    }

    #[test]
    fn leaving_the_recorder_closes_its_only_inhibitor() {
        let mut fixture = Fixture::new();
        assert_eq!(fixture.rig.logind().method_calls(), [""; 0]);
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        fixture.streaming(1);
        wait_for_inhibitor(&fixture);
        let left = Instant::now();
        drop(recorder.finish());
        fixture.stopped(1);
        assert!(inhibitor_closed(&fixture.rig, 0) >= left);
        sleep(&mut fixture.rig);
        wake(&mut fixture.rig);
        assert_eq!(fixture.rig.logind().method_calls(), [INHIBIT]);
        assert_eq!(fixture.rig.logind().inhibitors().len(), 1);
        fixture.finish();
    }

    #[test]
    fn a_refused_inhibitor_still_dictates_and_stops_on_sleep() {
        let mut fixture = Fixture::new();
        fixture.rig.logind().refuse_inhibit();
        fixture.list(CAPTURE);
        let recorder = record(&fixture.rig, &fixture.server, "dictation.wav");
        let mode_at = mode_set_at(&fixture);
        let doap = fixture.device.pods.doap();
        let latest = mode_at + MODE_SETTLE + INHIBIT_WAIT / 2;
        wait_within(
            "START at the settle after logind refused",
            latest.saturating_duration_since(Instant::now()),
            || (doap.starts() == 1).then_some(()),
        );
        fixture.streaming(1);
        wait_for("the refused logind delay inhibitor", || {
            let logind = fixture.rig.logind();
            (logind.method_calls() == [INHIBIT] && logind.inhibitors().is_empty()).then_some(())
        });
        ready_to_read(&recorder, AUDIO);
        sleep(&mut fixture.rig);
        fixture.stopped(1);
        fixture.rig.wait_logged("dictation: stop sent (sleep)");
        assert_eq!(fixture.rig.logind().method_calls(), [INHIBIT]);
        lose(&fixture.rig, &fixture.device);
        assert_sounds(&recorder.finish(), "");
        fixture.no_uplink();
        assert_eq!(logged(&mut fixture.rig, INHIBIT_TIMEOUT), 0);
        let daemon = fixture.rig.daemon();
        let failures = wait_for("the inhibitor failure log", || {
            let failures: Vec<_> = daemon
                .log()
                .lines()
                .filter(|line| line.starts_with(INHIBIT_FAILED))
                .map(str::to_owned)
                .collect();
            (!failures.is_empty()).then_some(failures)
        });
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0],
            format!("{INHIBIT_FAILED}: org.freedesktop.DBus.Error.AccessDenied")
        );
        fixture.finish();
    }
}
