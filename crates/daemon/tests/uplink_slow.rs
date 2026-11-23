//! The linger and the gate at real time through the built daemon against the fake pods and the fake audio server: a minute for the linger, seconds for a bud taken out.

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

    use pods_core::policy::mic::{BUD_GRACE_MS, STOP_WAITS_LINE, UPLINK_LINGER_MS};
    use pods_samples::uplink::{START, STOP};
    use pods_samples::{MODEL_NUMBER, Placed};
    use pulseaudio::protocol::SinkState;

    use super::support::audio::wire::sink_named;
    use super::support::audio::{FakeAudio, Volumes};
    use super::support::rig::{Device, REAL_TIME_SLACK, Rig, assert_gap};
    use super::support::settings::set;
    use super::support::uplink::{
        assert_hold, link_heard, move_sink, read_up_to_now, rig_with_audio, starts, stops,
        wait_mic, wait_restore, wait_stop,
    };
    use super::support::{HANG_LIMIT, wait_within};

    const LINGER: Duration = Duration::from_millis(UPLINK_LINGER_MS);
    const GRACE: Duration = Duration::from_millis(BUD_GRACE_MS);
    /// How long a shut gate is watched for a STOP that must not come.
    const STILL_SHUT: Duration = Duration::from_secs(10);

    /// The instants before and after a step the daemon applies somewhere between them.
    type Step = (Instant, Instant);

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

    /// Both buds into the case, until the daemon shows them out.
    fn take_out(rig: &Rig, device: &Device) -> Step {
        let before = Instant::now();
        device.pods.set_ear(Placed::InCase, Placed::InCase);
        rig.wait_status("both buds out", |status| {
            status["left"]["in_ear"] == false && status["right"]["in_ear"] == false
        });
        (before, Instant::now())
    }

    /// Fails unless `to` came `wanted` after `step`, wherever inside it the daemon applied it.
    fn assert_after(what: &str, step: Step, to: Instant, wanted: Duration) {
        assert_gap(what, step.0, to, &(wanted..));
        assert_gap(
            what,
            step.1,
            to,
            &(..=wanted.saturating_add(REAL_TIME_SLACK)),
        );
    }

    fn the_stop_after(device: &Device, wanted: Duration) -> Instant {
        let within = wanted.saturating_add(HANG_LIMIT);
        let heard = device.pods.wait_heard(&STOP, 1, within);
        heard.unwrap_or_else(|deadline| panic!("a STOP: {deadline:?}"))[0].at
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
        wait_mic(&rig, "idle");
        audio.assert_only_reads();
        rig.finish();
    }

    #[test]
    #[ignore = "the linger at real time; the slow job runs it"]
    fn the_linger_with_a_suspended_sink_ends_in_a_stop_and_no_restore() {
        let (rig, device, audio, unwanted) = unwanted(SinkState::Suspended);
        let stop = the_stop_after(&device, LINGER);
        assert_after("the STOP after the want went", unwanted, stop, LINGER);
        wait_mic(&rig, "idle");
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
}
