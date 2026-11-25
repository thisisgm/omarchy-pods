//! Dictation through link loss, bearer loss and the logind sleep edge.

#[cfg(test)]
#[expect(dead_code, reason = "each suite uses part of the shared support")]
mod support;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use pods_core::policy::dictation::{DOAP_STOP_ANSWER_MS, INHIBIT_WAIT_MS, MODE_SETTLE_MS};
    use pods_proto::doap::{doap_mode_off, doap_mode_on};

    use super::support::dictation::{CAPTURE, Fixture, HOST_STOP, ready_to_read, status};
    use super::support::logind::INHIBIT;
    use super::support::pipewire::{record, wait_no_node};
    use super::support::recording::{assert_sounds, first_sound, frames, longest_zero_run};
    use super::support::sleep::{WATCHING, inhibitor_closed, logged, sleep, wake};
    use super::support::uplink::{assert_no_sink_call, lose};
    use super::support::{pause, wait_for, wait_within};

    const STOP_TIMEOUT: &str = "dictation: stop not confirmed";
    const INHIBIT_FAILED: &str = "dictation: cannot inhibit sleep via logind";
    const INHIBIT_TIMEOUT: &str = "dictation: logind inhibitor timed out, starting without it";
    const STOP_WAIT: Duration = Duration::from_millis(DOAP_STOP_ANSWER_MS);
    const MODE_SETTLE: Duration = Duration::from_millis(MODE_SETTLE_MS);
    const INHIBIT_WAIT: Duration = Duration::from_millis(INHIBIT_WAIT_MS);
    const AFTER_SETTLE: Duration = Duration::from_millis(MODE_SETTLE_MS + INHIBIT_WAIT_MS / 5);
    const AUDIO: Duration = Duration::from_millis(400);

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

    #[test]
    fn the_channel_lost_mid_dictation_fades_and_arms_again() {
        const MIN_SILENCE: Duration = Duration::from_millis(150);
        let mut fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        fixture.list(CAPTURE);
        let recorder = fixture.capture();
        fixture.streaming(1);
        ready_to_read(&recorder, AUDIO);
        fixture.device.pods.hang_up_att();
        status(&fixture.rig, "arming");
        wait_for("the attribute service to be armed again", || {
            (doap.selections() == 2).then_some(())
        });
        fixture.streaming(2);
        let resumed_at = usize::try_from(recorder.frames_on_disk()).unwrap();
        let more = resumed_at.saturating_add(frames(AUDIO));
        recorder.wait_frames(more, super::support::HANG_LIMIT);
        let samples = recorder.finish();
        assert_sounds(&samples, "");
        let sound = &samples[first_sound(&samples)..];
        assert!(
            longest_zero_run(sound) >= frames(MIN_SILENCE),
            "no silent gap after bearer loss"
        );
        assert_sounds(&samples[resumed_at..], " after re-arming");
        fixture.stopped(1);
        fixture
            .rig
            .wait_logged("dictation: bearer lost, arming again");
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
        fixture.stopped(1);
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
        assert!(
            failures[0].contains("inhibitor refused"),
            "missing logind error: {}",
            failures[0]
        );
        fixture.finish();
    }
}
