//! Recorder routing and `DoAP` sessions through the daemon binary.

#[cfg(test)]
#[expect(dead_code, reason = "each suite uses part of the shared support")]
mod support;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use pods_core::policy::dictation::DOAP_STOP_ANSWER_MS;
    use pods_proto::doap::{STOP_REASON_UNSOLICITED, doap_mode_off};
    use pods_samples::NAME;

    use super::support::audio::Recorder as Listed;
    use super::support::dictation::{
        CAPTURE, Fixture, HOST_STOP, NO_RECORDER, ready_to_read, status, uplink,
    };
    use super::support::pipewire::record;
    use super::support::uplink::assert_no_sink_call;
    use super::support::{HANG_LIMIT, pause, wait_for};

    #[test]
    fn a_dictation_recorder_starts_doap_and_never_the_uplink() {
        let fixture = Fixture::new();
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        fixture.streaming(1);
        drop(recorder.finish());
        fixture.stopped(1);
        fixture.no_uplink();
        fixture.finish();
    }

    #[test]
    fn the_recorder_leaving_stops_doap_at_once_and_makes_no_sink_call() {
        const STOP_LIMIT: Duration = Duration::from_millis(300);
        let mut fixture = Fixture::new();
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        fixture.streaming(1);
        let left = Instant::now();
        drop(recorder.finish());
        fixture.stopped(1);
        assert!(
            left.elapsed() < STOP_LIMIT,
            "STOP waited after the recorder left"
        );
        fixture.rig.wait_logged(super::support::dictation::LEFT);
        assert_no_sink_call(&fixture.rig, &fixture.audio);
        fixture.finish();
    }

    #[test]
    fn twenty_dictations_with_music_make_no_sink_call_and_no_uplink_command() {
        let mut fixture = Fixture::new();
        for count in 1..=20 {
            let listed = fixture.list(CAPTURE);
            let recorder = fixture.capture();
            fixture.streaming(count);
            drop(recorder.finish());
            fixture.stopped(usize::try_from(count).unwrap());
            fixture.audio.remove_recorder(listed);
            wait_for("the empty recorder list", || {
                (fixture
                    .rig
                    .daemon()
                    .log()
                    .lines()
                    .filter(|line| *line == NO_RECORDER)
                    .count()
                    == usize::try_from(count).unwrap())
                .then_some(())
            });
        }
        assert_no_sink_call(&fixture.rig, &fixture.audio);
        fixture.no_uplink();
        fixture.finish();
    }

    #[test]
    fn two_dictations_one_second_apart_each_start_and_stop_once() {
        let fixture = Fixture::new();
        fixture.list(CAPTURE);
        for count in 1..=2 {
            let recorder = fixture.capture();
            fixture.streaming(count);
            drop(recorder.finish());
            fixture.stopped(usize::try_from(count).unwrap());
            if count == 1 {
                fixture
                    .rig
                    .holds_for(Duration::from_secs(1), "the source stays armed", |value| {
                        value["mic_dictation"] == "armed"
                    });
            }
        }
        assert_eq!(fixture.mode_writes(), 2);
        fixture.no_uplink();
        fixture.finish();
    }

    #[test]
    fn a_call_recorder_takes_the_uplink() {
        let fixture = Fixture::new();
        fixture.list(Listed::Program);
        let call = fixture.call();
        uplink(&fixture);
        assert_eq!(fixture.device.pods.doap().starts(), 0);
        assert_eq!(fixture.mode_writes(), 0);
        drop(call.finish());
        fixture.finish();
    }

    #[test]
    fn a_call_after_dictation_routes_from_its_delayed_list() {
        let mut fixture = Fixture::new();
        let log_path = fixture.rig.daemon().log.clone();
        let log = || fs::read_to_string(&log_path).unwrap();
        let listed = fixture.list(CAPTURE);
        fixture
            .rig
            .wait_logged("mic: recorders (dictation 1, other 0)");
        let recorder = fixture.capture();
        fixture.streaming(1);

        fixture.audio.hold_replies(true);
        fixture.audio.remove_recorder(listed);
        drop(recorder.finish());
        fixture.stopped(1);
        fixture.list(Listed::Program);
        let call = fixture.call();
        wait_for(
            "the second recorder to link before its list arrives",
            || {
                (log()
                    .lines()
                    .filter(|line| *line == "mic: recorder linked")
                    .count()
                    == 2)
                    .then_some(())
            },
        );
        fixture.audio.hold_replies(false);

        let route = wait_for("the second recording's route", || {
            log()
                .lines()
                .filter(|line| line.starts_with("mic: route "))
                .nth(1)
                .map(str::to_owned)
        });
        assert_eq!(route, "mic: route uplink (1 recorders)");
        uplink(&fixture);
        assert_eq!(fixture.device.pods.doap().starts(), 1);
        assert_eq!(fixture.mode_writes(), 1);
        drop(call.finish());
        fixture.finish();
    }

    #[test]
    fn a_dictation_during_a_call_stays_on_the_uplink() {
        let mut fixture = Fixture::new();
        fixture.list(Listed::Program);
        let call = fixture.call();
        uplink(&fixture);
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        ready_to_read(&recorder, Duration::from_millis(100));
        drop(call.finish());
        ready_to_read(&recorder, Duration::from_millis(300));
        drop(recorder.finish());
        fixture
            .rig
            .wait_logged("mic: recorders (dictation 1, other 1)");
        assert_eq!(fixture.mode_writes(), 0);
        fixture.finish();
    }

    #[test]
    fn a_call_during_a_dictation_stays_on_doap_until_all_leave() {
        let mut fixture = Fixture::new();
        let listed = fixture.list(CAPTURE);
        let recorder = fixture.capture();
        fixture.streaming(1);
        fixture.list(Listed::Program);
        let call = fixture.call();
        ready_to_read(&call, Duration::from_millis(100));
        fixture.audio.remove_recorder(listed);
        drop(recorder.finish());
        ready_to_read(&call, Duration::from_millis(300));
        assert_eq!(fixture.device.pods.doap().stops(), [0_u8; 0]);
        fixture.streaming(1);
        drop(call.finish());
        fixture.stopped(1);
        fixture
            .rig
            .wait_logged("mic: recorders (dictation 0, other 1)");
        fixture.no_uplink();
        fixture.finish();
    }

    #[test]
    fn a_recorder_leaving_before_the_answer_owes_the_stop() {
        let mut fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        doap.hold_start(true);
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        fixture.start(1);
        drop(recorder.finish());
        status(&fixture.rig, "stopping");
        assert_eq!(doap.stops(), [0_u8; 0]);
        doap.release_start();
        fixture.stopped(1);
        fixture.rig.wait_logged(super::support::dictation::LEFT);
        fixture.finish();
    }

    #[test]
    fn a_list_that_never_comes_takes_doap_after_the_wait() {
        let mut fixture = Fixture::new();
        let recorder = fixture.capture();
        fixture.streaming(1);
        drop(recorder.finish());
        fixture.stopped(1);
        assert!(fixture.rig.daemon().log().lines().any(|line| {
            line.starts_with("mic: route unknown after ") && line.ends_with(" ms, dictation")
        }));
        fixture.finish();
    }

    #[test]
    fn a_refusal_sends_the_mode_again_and_one_more_start() {
        let mut fixture = Fixture::new();
        fixture.device.pods.doap().refuse_starts(1);
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        fixture.streaming(2);
        drop(recorder.finish());
        fixture.stopped(1);
        assert_eq!(fixture.mode_writes(), 2);
        fixture
            .rig
            .wait_logged("dictation: refused (reason 6), mode sent again");
        fixture.finish();
    }

    #[test]
    fn two_refusals_hand_the_recording_to_the_uplink() {
        let mut fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        doap.refuse_starts(2);
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        uplink(&fixture);
        assert_eq!(doap.starts(), 2);
        assert_eq!(fixture.mode_writes(), 2);
        wait_for("the stem returned at fallback", || {
            (!doap.mode_seen()).then_some(())
        });
        drop(recorder.finish());
        fixture
            .rig
            .wait_logged("dictation: refused twice, this recording takes the uplink");
        fixture.finish();
    }

    #[test]
    fn an_end_from_the_pods_mid_dictation_starts_again_once() {
        let fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        fixture.streaming(1);
        doap.end_unasked();
        fixture.streaming(2);
        drop(recorder.finish());
        fixture.stopped(1);
        assert_eq!(doap.starts(), 2);
        fixture.no_uplink();
        fixture.finish();
    }

    #[test]
    fn a_burst_of_unsolicited_audio_gets_one_stop() {
        const BURST_PACKETS: usize = 4;
        let fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        for _ in 0..BURST_PACKETS {
            doap.unsolicited_audio();
        }
        wait_past_att(&fixture);
        assert_eq!(doap.stops(), [STOP_REASON_UNSOLICITED]);

        doap.hold_stop();
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        fixture.streaming(1);
        drop(recorder.finish());
        status(&fixture.rig, "armed");
        wait_for("the mode to clear without an end", || {
            (!doap.mode_seen()).then_some(())
        });
        doap.unsolicited_audio();
        wait_past_att(&fixture);
        assert_eq!(
            doap.stops(),
            [STOP_REASON_UNSOLICITED, HOST_STOP, STOP_REASON_UNSOLICITED]
        );
        fixture.finish();
    }

    #[test]
    fn audio_with_no_session_is_stopped_with_reason_6() {
        let mut fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        doap.unsolicited_audio();
        wait_for("STOP for unsolicited audio", || {
            (doap.stops() == [STOP_REASON_UNSOLICITED]).then_some(())
        });
        let line = "dictation: audio with no session, stopped";
        doap.end_unsolicited_audio();
        doap.unsolicited_audio();
        wait_for("STOP again for unsolicited audio", || {
            (doap.stops() == [STOP_REASON_UNSOLICITED; 2]).then_some(())
        });
        fixture.rig.wait_queued_inputs_applied();
        assert_eq!(
            fixture
                .rig
                .daemon()
                .log()
                .lines()
                .filter(|logged| *logged == line)
                .count(),
            1
        );
        assert_eq!(doap.starts(), 0);
        fixture.finish();
    }

    #[test]
    fn unsolicited_audio_gets_another_stop_after_an_unanswered_window() {
        let fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        // With no START, the fake hears STOP but sends no streaming end.
        doap.unsolicited_audio();
        wait_past_att(&fixture);
        assert_eq!(doap.stops(), [STOP_REASON_UNSOLICITED]);

        doap.unsolicited_audio();
        wait_past_att(&fixture);
        assert_eq!(doap.stops(), [STOP_REASON_UNSOLICITED]);

        pause(Duration::from_millis(DOAP_STOP_ANSWER_MS));
        doap.unsolicited_audio();
        wait_past_att(&fixture);
        assert_eq!(
            doap.stops(),
            [STOP_REASON_UNSOLICITED; 2],
            "stray audio after an unanswered STOP window"
        );
        fixture.finish();
    }

    fn wait_past_att(fixture: &Fixture) {
        let doap = fixture.device.pods.doap();
        let (_, confirmed) = doap.answered();
        doap.indicate();
        wait_for("the confirmation behind the audio", || {
            (doap.answered().1 > confirmed).then_some(())
        });
        fixture.rig.wait_queued_inputs_applied();
        // The adapter writes the driver's queued STOPs before it reads this second indication.
        doap.indicate();
        wait_for("the confirmation behind queued requests", || {
            (doap.answered().1 > confirmed.saturating_add(1)).then_some(())
        });
    }

    #[test]
    fn garbled_frames_are_counted_and_the_daemon_lives() {
        let fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        fixture.streaming(1);
        ready_to_read(&recorder, Duration::from_millis(300));
        doap.garble(1);
        ready_to_read(&recorder, Duration::from_millis(600));
        drop(recorder.finish());
        fixture.stopped(1);
        fixture
            .rig
            .wait_status("the decoder counted the frame", |value| {
                value["mic_decode_errors"]
                    .as_u64()
                    .is_some_and(|count| count > 0)
            });
        fixture.finish();
    }

    #[test]
    fn exit_while_dictating_stops_and_waits_for_the_end() {
        const HOLD: Duration = Duration::from_millis(200);
        let mut fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        doap.hold_stop();
        fixture.list(CAPTURE);
        let recorder = record(&fixture.rig, &fixture.server, "dictation.wav");
        fixture.streaming(1);
        fixture.rig.term();
        wait_for("exit STOP", || (doap.stops() == [HOST_STOP]).then_some(()));
        let stopped = Instant::now();
        wait_for("the held STOP window", || {
            assert!(
                fixture.rig.daemon().child.try_wait().unwrap().is_none(),
                "the daemon exited before the end"
            );
            assert!(doap.mode_seen(), "mode was cleared with the end still held");
            (stopped.elapsed() >= HOLD).then_some(())
        });
        doap.release_stop();
        assert!(fixture.rig.exited(HANG_LIMIT).success());
        assert!(
            fixture
                .device
                .pods
                .heard()
                .iter()
                .any(|heard| heard.sdu == doap_mode_off())
        );
        fixture.rig.wait_logged("dictation: stop sent (exit)");
        drop(recorder.finish());
        fixture.finish();
    }

    #[test]
    fn the_log_holds_no_address_and_no_name() {
        let mut fixture = Fixture::new();
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        fixture.streaming(1);
        drop(recorder.finish());
        fixture.stopped(1);
        let log = fixture.rig.daemon().log().to_lowercase();
        let octets = fixture
            .device
            .address
            .octets()
            .map(|octet| format!("{octet:02x}"));
        for identifier in [octets.join("_"), octets.join(":"), NAME.to_lowercase()] {
            assert!(
                !log.contains(&identifier),
                "the log contains a device identifier"
            );
        }
        fixture.finish();
    }
}
