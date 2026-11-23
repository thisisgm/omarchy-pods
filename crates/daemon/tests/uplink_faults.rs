//! The uplink when the pods do not behave, through the built daemon against the fake pods, the fake audio server and the fake logind: tries, markers nobody asked for, a sleep and a lost link.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use pods_core::policy::mic::{
        START_TRIES, STOP_WAITS_LINE, STOPPED_BY_THE_PODS_LINE, start_sent_line, unanswered_line,
    };
    use pods_fake::Profile;
    use pods_samples::{MODEL_NUMBER, Placed};
    use pulseaudio::protocol::SinkState;

    use super::support::audio::FakeAudio;
    use super::support::rig::{Device, REAL_TIME_SLACK, Rig, assert_gap};
    use super::support::settings::set;
    use super::support::sleep::{logged, sleep, wake};
    use super::support::uplink::{
        START_ANSWER, STOP_ANSWER, TRY_SPACING, all_tries, link, link_heard, lose, mic, move_sink,
        read_up_to_now, rig_with, rig_with_audio, start_failures, starts, stops, wait_mic,
        wait_restore, wait_starts, wait_stop,
    };
    use super::support::{pause, wait_for};

    /// Past the deadline of a START, so a try that was still due would have shown.
    const PAST_THE_ANSWER: Duration = START_ANSWER.saturating_add(REAL_TIME_SLACK);

    #[test]
    fn the_second_try_answered_goes_live_and_counts_one_failure() {
        let (mut rig, device) = rig_with("always", &[MODEL_NUMBER], Profile::default());
        device.pods.uplink().hold_starts(true);
        let linked = Instant::now();
        link(&rig, &device);
        let tries = wait_starts(&device, 2);
        assert_gap(
            "the second START after the link",
            linked,
            tries[1].at,
            &(START_ANSWER..),
        );
        assert_gap(
            "the second START after the first",
            tries[0].at,
            tries[1].at,
            &(TRY_SPACING..),
        );
        device.pods.uplink().release_start();
        wait_mic(&rig, "live");
        read_up_to_now(&rig, &device);
        assert_eq!(starts(&device).len(), 2, "{:?}", device.pods.heard());
        assert_eq!(start_failures(&rig), 1);
        assert_eq!(logged(&mut rig, &unanswered_line(1)), 1);
        assert_eq!(logged(&mut rig, &start_sent_line(2)), 1);
        rig.finish();
    }

    #[test]
    fn no_stop_is_sent_for_a_start_never_answered() {
        let (rig, device) = rig_with("always", &[MODEL_NUMBER], Profile::default());
        device.pods.uplink().never_answer(true);
        link(&rig, &device);
        wait_mic(&rig, "failed");
        read_up_to_now(&rig, &device);
        assert_eq!(
            starts(&device).len(),
            all_tries(),
            "{:?}",
            device.pods.heard()
        );
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        rig.finish();
    }

    #[test]
    fn setting_off_during_the_second_try_sends_no_third_start() {
        let (mut rig, device) = rig_with("always", &[MODEL_NUMBER], Profile::default());
        device.pods.uplink().never_answer(true);
        link(&rig, &device);
        wait_starts(&device, 2);
        set(rig.session(), "microphone", "off");
        rig.wait_logged(&unanswered_line(2));
        read_up_to_now(&rig, &device);
        assert_eq!(starts(&device).len(), 2, "{:?}", device.pods.heard());
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        assert_eq!(logged(&mut rig, &start_sent_line(3)), 0);
        assert_eq!(mic(&rig), "off");
        rig.finish();
    }

    #[test]
    fn a_late_running_marker_with_no_want_is_stopped_through_the_same_gate() {
        let (mut rig, device, audio) =
            rig_with_audio("always", &[MODEL_NUMBER], SinkState::Running);
        device.pods.uplink().hold_starts(true);
        link_heard(&rig, &device);
        wait_starts(&device, 1);
        set(rig.session(), "microphone", "auto");
        rig.wait_logged(&unanswered_line(1));
        device.pods.uplink().release_start();
        rig.wait_logged(STOP_WAITS_LINE);
        read_up_to_now(&rig, &device);
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        assert_eq!(mic(&rig), "live");
        let suspended = Instant::now();
        move_sink(&rig, &audio, &device, SinkState::Suspended);
        let at_once = Duration::ZERO..=REAL_TIME_SLACK;
        let stop = wait_stop(&device);
        assert_gap("the STOP after the suspend", suspended, stop.at, &at_once);
        wait_mic(&rig, "idle");
        read_up_to_now(&rig, &device);
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        audio.assert_only_reads();
        rig.finish();
    }

    /// The pods live with `always` over a playing sink, then stopped by the pods and the sink restored for it.
    fn stopped_by_the_pods() -> (Rig, Device, Rc<FakeAudio>) {
        let (mut rig, device, audio) =
            rig_with_audio("always", &[MODEL_NUMBER], SinkState::Running);
        link_heard(&rig, &device);
        wait_mic(&rig, "live");
        device.pods.uplink().stop_unasked();
        rig.wait_logged(STOPPED_BY_THE_PODS_LINE);
        wait_restore(&audio, &device);
        wait_mic(&rig, "idle");
        read_up_to_now(&rig, &device);
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        (rig, device, audio)
    }

    #[test]
    fn a_stop_nobody_asked_for_restores_and_does_not_start_again() {
        let (rig, _device, _audio) = stopped_by_the_pods();
        rig.finish();
    }

    #[test]
    fn the_bud_put_back_after_that_starts_again() {
        let (rig, device, _audio) = stopped_by_the_pods();
        device.pods.set_ear(Placed::InCase, Placed::InCase);
        wait_mic(&rig, "not_worn");
        read_up_to_now(&rig, &device);
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        device.pods.set_ear(Placed::InEar, Placed::InEar);
        wait_starts(&device, 2);
        wait_mic(&rig, "live");
        rig.finish();
    }

    #[test]
    fn sleep_while_running_sends_stop_before_the_suspend_and_no_restore() {
        let (mut rig, device, audio) =
            rig_with_audio("always", &[MODEL_NUMBER], SinkState::Running);
        link_heard(&rig, &device);
        wait_mic(&rig, "live");
        let edge = sleep(&mut rig);
        let stop = wait_stop(&device);
        assert!(stop.at > edge, "the STOP before the edge into sleep");
        rig.wait_logged("mic: stop sent (sleep)");
        wait_for("the uplink to stop", || {
            (!device.pods.uplink().running()).then_some(())
        });
        read_up_to_now(&rig, &device);
        assert_eq!(mic(&rig), "idle");
        audio.assert_only_reads();
        rig.finish();
    }

    #[test]
    fn sleep_while_starting_sends_nothing() {
        let (mut rig, device) = rig_with("always", &[MODEL_NUMBER], Profile::default());
        device.pods.uplink().hold_starts(true);
        link(&rig, &device);
        wait_starts(&device, 1);
        sleep(&mut rig);
        wait_mic(&rig, "idle");
        rig.holds_for(PAST_THE_ANSWER, "mic idle", |status| {
            status["mic"] == "idle"
        });
        read_up_to_now(&rig, &device);
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        assert_eq!(logged(&mut rig, &unanswered_line(1)), 0);
        assert_eq!(start_failures(&rig), 0);
        rig.finish();
    }

    #[test]
    fn resume_starts_again_when_the_want_still_holds() {
        let (mut rig, device) = rig_with("always", &[MODEL_NUMBER], Profile::default());
        link(&rig, &device);
        wait_mic(&rig, "live");
        sleep(&mut rig);
        wait_stop(&device);
        wait_for("the uplink to stop", || {
            (!device.pods.uplink().running()).then_some(())
        });
        read_up_to_now(&rig, &device);
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        let woke = wake(&mut rig);
        let again = wait_starts(&device, 2)[1].at;
        assert!(again > woke, "the START before the wake");
        wait_mic(&rig, "live");
        rig.finish();
    }

    #[test]
    fn the_link_dropping_in_starting_sends_nothing_more() {
        let (mut rig, device) = rig_with("always", &[MODEL_NUMBER], Profile::default());
        device.pods.uplink().hold_starts(true);
        link(&rig, &device);
        wait_starts(&device, 1);
        lose(&rig, &device);
        rig.holds_for(PAST_THE_ANSWER, "mic idle", |status| {
            status["mic"] == "idle"
        });
        link(&rig, &device);
        wait_starts(&device, 2);
        let log = rig.daemon().log();
        assert_eq!(logged(&mut rig, &start_sent_line(1)), 2, "{log}");
        assert_eq!(logged(&mut rig, &start_sent_line(2)), 0, "{log}");
        assert_eq!(logged(&mut rig, &unanswered_line(1)), 0, "{log}");
        rig.finish();
    }

    #[test]
    fn the_link_dropping_in_stopping_orders_no_restore() {
        let (rig, device, audio) = rig_with_audio("always", &[MODEL_NUMBER], SinkState::Running);
        link_heard(&rig, &device);
        wait_mic(&rig, "live");
        device.pods.uplink().hold_stops(true);
        set(rig.session(), "microphone", "off");
        let stop = wait_stop(&device);
        lose(&rig, &device);
        device.pods.uplink().release_stop();
        link(&rig, &device);
        rig.wait_sink_told();
        // A restore the lost STOP left due would go out once its answer window closed.
        let closed = STOP_ANSWER.saturating_add(REAL_TIME_SLACK);
        pause(closed.saturating_sub(stop.at.elapsed()));
        read_up_to_now(&rig, &device);
        assert!(audio.writes().is_empty(), "{:?}", audio.writes());
        rig.finish();
    }

    #[test]
    fn failed_is_forgotten_at_a_reconnect() {
        let (rig, device) = rig_with("always", &[MODEL_NUMBER], Profile::default());
        device.pods.uplink().never_answer(true);
        link(&rig, &device);
        wait_mic(&rig, "failed");
        lose(&rig, &device);
        device.pods.uplink().never_answer(false);
        link(&rig, &device);
        wait_starts(&device, all_tries().saturating_add(1));
        wait_mic(&rig, "live");
        assert_eq!(start_failures(&rig), u64::from(START_TRIES));
        rig.finish();
    }
}
