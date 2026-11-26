//! The linger, the gate and the answer windows at real time through the built daemon against the fake pods and the fake audio server: a minute for the linger, seconds for a bud taken out or a try.

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
        BUD_GRACE_MS, FAILED_LINE, START_TRIES, STOP_UNCONFIRMED_LINE, STOP_WAITS_LINE,
        UPLINK_LINGER_MS, unanswered_line,
    };
    use pods_fake::Profile;
    use pods_samples::uplink::START;
    use pods_samples::{MODEL_NUMBER, Placed};
    use pulseaudio::protocol::SinkState;

    use super::support::audio::wire::sink_named;
    use super::support::audio::{FakeAudio, Volumes};
    use super::support::rig::{Device, REAL_TIME_SLACK, Rig, assert_gap};
    use super::support::settings::set;
    use super::support::sleep::logged;
    use super::support::uplink::{
        START_ANSWER, STOP_ANSWER, Step, TRY_SPACING, all_tries, assert_after, assert_hold, link,
        link_heard, move_sink, put_in, read_up_to_now, rig_with, rig_with_audio, set_off_unworn,
        start_failures, starts, stops, take_out, the_stop_after, wait_mic, wait_restore,
        wait_starts, wait_stop,
    };
    use super::support::{HANG_LIMIT, wait_within};

    const LINGER: Duration = Duration::from_millis(UPLINK_LINGER_MS);
    const GRACE: Duration = Duration::from_millis(BUD_GRACE_MS);
    /// How long a shut gate is watched for a STOP that must not come.
    const STILL_SHUT: Duration = Duration::from_secs(10);

    /// The pods live with `always` on a confirmed model, their sink in `state`, then `auto`: nothing wants the uplink from the step returned.
    fn unwanted(state: SinkState) -> (Rig, Device, Rc<FakeAudio>, Step) {
        let (rig, device, audio) = rig_with_audio("always", &[MODEL_NUMBER], state);
        link_heard(&rig, &device);
        wait_mic(&rig, "live");
        let before = Instant::now();
        set(rig.session(), "microphone", "auto");
        (rig, device, audio, (before, Instant::now()))
    }

    /// The linger passed with the gate shut, so the STOP waits.
    fn waiting(state: SinkState) -> (Rig, Device, Rc<FakeAudio>) {
        let (mut rig, device, audio, _) = unwanted(state);
        wait_waits(&mut rig);
        (rig, device, audio)
    }

    fn wait_waits(rig: &mut Rig) {
        let daemon = rig.daemon();
        wait_within(
            "the STOP to wait",
            LINGER.saturating_add(HANG_LIMIT),
            || {
                daemon
                    .log()
                    .lines()
                    .any(|line| line == STOP_WAITS_LINE)
                    .then_some(())
            },
        );
    }

    /// No STOP and `live` for `STILL_SHUT`, then the pods' packets read.
    fn holds_shut(rig: &Rig, device: &Device) {
        rig.holds_for(STILL_SHUT, "mic live", |status| status["mic"] == "live");
        read_up_to_now(rig, device);
        assert!(stops(device).is_empty(), "{:?}", device.pods.heard());
    }

    #[test]
    #[ignore = "the linger at real time; the slow job runs it"]
    fn the_stop_waits_while_the_sink_is_not_suspended_and_a_bud_is_worn() {
        let (mut rig, device, audio) = waiting(SinkState::Running);
        holds_shut(&rig, &device);
        audio.assert_only_reads();
        let log = rig.daemon().log();
        let waits = log.lines().filter(|line| *line == STOP_WAITS_LINE).count();
        assert_eq!(waits, 1, "{log}");
        rig.finish();
    }

    #[test]
    #[ignore = "the linger at real time; the slow job runs it"]
    fn an_idle_sink_still_holds_the_stop() {
        let (rig, device, audio) = waiting(SinkState::Running);
        move_sink(&rig, &audio, &device, SinkState::Idle);
        holds_shut(&rig, &device);
        rig.finish();
    }

    #[test]
    #[ignore = "the linger at real time; the slow job runs it"]
    fn the_stop_goes_out_when_the_sink_is_suspended_and_no_restore_follows() {
        let (rig, device, audio) = waiting(SinkState::Idle);
        let suspended = Instant::now();
        move_sink(&rig, &audio, &device, SinkState::Suspended);
        let at_once = Duration::ZERO..=REAL_TIME_SLACK;
        assert_gap(
            "the STOP after the suspend",
            suspended,
            wait_stop(&device).at,
            &at_once,
        );
        wait_mic(&rig, "no_audio_server");
        audio.assert_only_reads();
        rig.finish();
    }

    #[test]
    #[ignore = "the linger at real time; the slow job runs it"]
    fn the_linger_with_a_suspended_sink_ends_in_a_stop_and_no_restore() {
        let (rig, device, audio, unwanted) = unwanted(SinkState::Suspended);
        let stop = the_stop_after(&device, LINGER);
        assert_after("the STOP after the want went", unwanted, stop, LINGER);
        wait_mic(&rig, "no_audio_server");
        audio.assert_only_reads();
        rig.finish();
    }

    #[test]
    #[ignore = "the linger at real time; the slow job runs it"]
    fn a_want_that_returns_while_the_stop_waits_sends_nothing() {
        let (rig, device, audio) = waiting(SinkState::Running);
        set(rig.session(), "microphone", "always");
        move_sink(&rig, &audio, &device, SinkState::Suspended);
        holds_shut(&rig, &device);
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        rig.finish();
    }

    #[test]
    #[ignore = "the linger at real time; the slow job runs it"]
    fn an_unknown_sink_state_holds_the_stop() {
        let (mut rig, device, audio, _) = unwanted(SinkState::Suspended);
        audio.stop();
        rig.wait_audio_server("down");
        wait_waits(&mut rig);
        holds_shut(&rig, &device);
        audio.set_sink_state(&sink_named(device.address), SinkState::Running);
        audio.restart(Volumes::Kept);
        rig.wait_sink_told();
        read_up_to_now(&rig, &device);
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        move_sink(&rig, &audio, &device, SinkState::Suspended);
        wait_stop(&device);
        audio.assert_only_reads();
        rig.finish();
    }

    #[test]
    #[ignore = "the grace of a bud at real time; the slow job runs it"]
    fn the_stop_goes_out_with_a_restore_when_the_last_bud_leaves() {
        let (rig, device, audio) = rig_with_audio("always", &[MODEL_NUMBER], SinkState::Running);
        link_heard(&rig, &device);
        wait_mic(&rig, "live");
        let left = take_out(&rig, &device);
        let stop = the_stop_after(&device, GRACE);
        assert_after("the STOP after the last bud left", left, stop, GRACE);
        let (suspended, resumed) = wait_restore(&audio, &device);
        assert_hold(suspended, resumed);
        read_up_to_now(&rig, &device);
        assert_eq!(audio.writes().len(), 2, "{:?}", audio.writes());
        rig.finish();
    }

    #[test]
    #[ignore = "the linger at real time; the slow job runs it"]
    fn a_bud_taken_out_while_the_stop_waits_gets_the_grace() {
        let (rig, device, audio) = waiting(SinkState::Running);
        let left = take_out(&rig, &device);
        let stop = the_stop_after(&device, GRACE);
        assert_after("the STOP after the last bud left", left, stop, GRACE);
        wait_restore(&audio, &device);
        rig.finish();
    }

    #[test]
    #[ignore = "the grace of a bud at real time; the slow job runs it"]
    fn a_bud_back_in_during_the_restore_starts_after_it() {
        let (rig, device, audio) = rig_with_audio("always", &[MODEL_NUMBER], SinkState::Running);
        link_heard(&rig, &device);
        wait_mic(&rig, "live");
        device.pods.set_ear(Placed::InCase, Placed::InCase);
        the_stop_after(&device, GRACE);
        audio.wait_writes(1, HANG_LIMIT);
        device.pods.set_ear(Placed::InEar, Placed::InEar);
        rig.wait_status("a bud back in", |status| status["left"]["in_ear"] == true);
        let seen = Instant::now();
        let (_, resumed) = wait_restore(&audio, &device);
        assert!(seen < resumed, "the bud back in after the resume");
        let again = device.pods.wait_heard(&START, 2, HANG_LIMIT).unwrap()[1].at;
        assert!(again > resumed, "the START before the resume");
        wait_mic(&rig, "live");
        rig.finish();
    }

    #[test]
    #[ignore = "the spacing of the tries at real time; the slow job runs it"]
    #[expect(
        clippy::print_stderr,
        reason = "the spacings the run saw go into the test's output"
    )]
    fn an_unanswered_start_is_tried_three_times_then_failed() {
        let (mut rig, device) = rig_with("always", &[MODEL_NUMBER], Profile::default());
        device.pods.uplink().never_answer(true);
        let linked = Instant::now();
        link(&rig, &device);
        let tries = wait_starts(&device, all_tries());
        wait_mic(&rig, "failed");
        let failed = Instant::now();
        let mut ends: Vec<Instant> = tries.iter().skip(1).map(|heard| heard.at).collect();
        ends.push(failed);
        let spacings: Vec<Duration> = tries
            .iter()
            .zip(&ends)
            .map(|(heard, end)| end.saturating_duration_since(heard.at))
            .collect();
        let (shortest, longest) = (spacings.iter().min(), spacings.iter().max());
        eprintln!("the three spacings: {shortest:?} to {longest:?}");
        for (tried, end) in (1..=START_TRIES).zip(&ends) {
            let window = START_ANSWER.saturating_mul(tried)..;
            assert_gap("the end of a try after the link", linked, *end, &window);
        }
        assert_eq!(spacings.len(), all_tries(), "{spacings:?}");
        for (tried, spacing) in (1..=START_TRIES).zip(&spacings) {
            assert!(*spacing >= TRY_SPACING, "try {tried} lasted {spacing:?}");
        }
        read_up_to_now(&rig, &device);
        let sent = starts(&device).len();
        assert_eq!(sent, all_tries(), "{:?}", device.pods.heard());
        assert_eq!(start_failures(&rig), u64::from(START_TRIES));
        for tried in 1..=START_TRIES {
            assert_eq!(logged(&mut rig, &unanswered_line(tried)), 1, "try {tried}");
        }
        assert_eq!(logged(&mut rig, FAILED_LINE), 1);
        rig.finish();
    }

    #[test]
    #[ignore = "the STOP's answer window at real time; the slow job runs it"]
    fn a_stop_never_confirmed_after_off_still_restores() {
        let (mut rig, device, audio) =
            rig_with_audio("always", &[MODEL_NUMBER], SinkState::Running);
        link_heard(&rig, &device);
        wait_mic(&rig, "live");
        device.pods.uplink().hold_stops(true);
        take_out(&rig, &device);
        let asked = Instant::now();
        set(rig.session(), "microphone", "off");
        wait_stop(&device);
        let (suspended, resumed) = wait_restore(&audio, &device);
        assert_gap(
            "the suspend after off was set",
            asked,
            suspended,
            &(STOP_ANSWER..),
        );
        assert_hold(suspended, resumed);
        rig.wait_logged(STOP_UNCONFIRMED_LINE);
        read_up_to_now(&rig, &device);
        assert_eq!(audio.writes().len(), 2, "{:?}", audio.writes());
        assert_eq!(stops(&device).len(), 1, "{:?}", device.pods.heard());
        rig.finish();
    }

    #[test]
    #[ignore = "the STOP's answer window at real time; the slow job runs it"]
    fn a_stop_never_confirmed_through_the_gate_restores_nothing() {
        let (mut rig, device, audio) =
            rig_with_audio("always", &[MODEL_NUMBER], SinkState::Suspended);
        device.pods.uplink().hold_starts(true);
        link_heard(&rig, &device);
        wait_starts(&device, 1);
        set(rig.session(), "microphone", "auto");
        rig.wait_logged(&unanswered_line(1));
        device.pods.uplink().hold_stops(true);
        let released = Instant::now();
        device.pods.uplink().release_start();
        wait_stop(&device);
        rig.wait_logged("mic: stop sent (quiet)");
        rig.wait_logged(STOP_UNCONFIRMED_LINE);
        assert_gap(
            "the unconfirmed line after the START was released",
            released,
            Instant::now(),
            &(STOP_ANSWER..),
        );
        wait_mic(&rig, "no_audio_server");
        read_up_to_now(&rig, &device);
        audio.assert_only_reads();
        let log = rig.daemon().log();
        assert!(
            !log.lines().any(|line| line.starts_with("mic: restore")),
            "{log}"
        );
        rig.finish();
    }

    #[test]
    #[ignore = "a START's answer window at real time; the slow job runs it"]
    fn a_start_is_still_sent_after_a_stop_never_confirmed() {
        let (mut rig, device, audio) =
            rig_with_audio("always", &[MODEL_NUMBER], SinkState::Running);
        link_heard(&rig, &device);
        wait_mic(&rig, "live");
        device.pods.uplink().hold_stops(true);
        set_off_unworn(&rig, &device);
        wait_stop(&device);
        let (_, resumed) = wait_restore(&audio, &device);
        rig.wait_logged(STOP_UNCONFIRMED_LINE);
        let uplink = device.pods.uplink();
        // The old run ends without audio that could confirm the new START.
        uplink.stall(HANG_LIMIT);
        put_in(&rig, &device);
        let asked = Instant::now();
        set(rig.session(), "microphone", "always");
        let second = wait_starts(&device, 2)[1].at;
        assert!(second > resumed, "the START before the resume");
        uplink.stop_unasked();
        wait_within("the old uplink to end", HANG_LIMIT, || {
            (!uplink.running()).then_some(())
        });
        uplink.stall(Duration::ZERO);
        let third = wait_starts(&device, 3)[2].at;
        assert_gap(
            "the next try after always was set",
            asked,
            third,
            &(START_ANSWER..),
        );
        wait_mic(&rig, "live");
        read_up_to_now(&rig, &device);
        assert_eq!(stops(&device).len(), 1, "{:?}", device.pods.heard());
        assert_eq!(start_failures(&rig), 1);
        rig.finish();
    }
}
