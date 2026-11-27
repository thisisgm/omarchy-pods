//! The uplink's STOP only on a quiet sink, through the built daemon against the fake pods, the fake audio server, the fake logind and a private `PipeWire`: `off`, the exit and a sleep during a call over music.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::ops::RangeInclusive;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use crate::support::audio::protocol::SinkState;
    use pods_core::policy::mic::{
        BUD_GRACE_MS, STOP_NOT_WORN_LINE, STOP_OWED_PLAYING_LINE, STOP_QUIET_LINE, UPLINK_STALL_MS,
    };
    use pods_fake::Heard;
    use pods_pwrig::{Recorder, Server};
    use pods_samples::MODEL_NUMBER;
    use pods_samples::uplink::START;

    use super::support::audio::wire::sink_named;
    use super::support::audio::{Asked, FakeAudio};
    use super::support::pipewire::{record, start, wait_no_node, wait_node};
    use super::support::rig::{Device, REAL_TIME_SLACK, Rig, assert_gap, not_before};
    use super::support::settings::set;
    use super::support::sleep::{logged, sleep, wake};
    use super::support::uplink::{
        EXIT_BEFORE_RESTORE_LINE, MIC_EXIT, assert_after, assert_no_sink_call, link_heard, mic,
        move_sink, read_up_to_now, rig_with_audio, starts, stops, take_out, the_stop_after,
        wait_mic, wait_restore, wait_stop,
    };
    use super::support::{HANG_LIMIT, wait_for};

    const GRACE: Duration = Duration::from_millis(BUD_GRACE_MS);
    const STALL: Duration = Duration::from_millis(UPLINK_STALL_MS);
    /// The driver's line for an exit out of its bound with the STOP still owed; the daemon is a binary, so its constant cannot be imported.
    const EXIT_UPLINK_RUNNING_LINE: &str = "mic: exit with the uplink running, the sink is playing";

    /// What a scenario left in the pods' record and in the audio server's, and the step that opened the gate, if one did.
    struct Ended {
        stops: Vec<Heard>,
        writes: Vec<Asked>,
        opened: Option<Instant>,
    }

    /// Every scenario of this suite, for the check that runs across them all.
    const SCENARIOS: [fn() -> Ended; 9] = [
        off_over_music,
        off_then_the_sink_suspends,
        off_then_the_last_bud_leaves,
        off_then_the_last_bud_leaves_and_the_sink_suspends,
        exit_over_music,
        exit_during_a_stall,
        exit_then_the_sink_suspends,
        exit_over_a_suspended_sink,
        sleep_over_music,
    ];

    fn at_once() -> RangeInclusive<Duration> {
        Duration::ZERO..=REAL_TIME_SLACK
    }

    /// `auto` on a confirmed model with the pods' sink in `state`, and the server a call's recorder links on.
    fn before_a_call(state: SinkState) -> (Rig, Device, Rc<FakeAudio>, Server) {
        let (rig, device, audio) = rig_with_audio("auto", &[MODEL_NUMBER], state);
        let pipewire = start(&rig);
        link_heard(&rig, &device);
        wait_node(&pipewire);
        (rig, device, audio, pipewire)
    }

    /// A call: a recorder linked to the node, which alone wants the uplink.
    fn call<'server>(rig: &Rig, pipewire: &'server Server) -> Recorder<'server> {
        let recorder = record(rig, pipewire, "call.wav");
        wait_mic(rig, "recording");
        recorder
    }

    /// `off` during the call, and no STOP in the pods' record once the daemon has read past the write.
    fn set_off(rig: &Rig, device: &Device) {
        set(rig.session(), "microphone", "off");
        assert_eq!(mic(rig), "stopping");
        read_up_to_now(rig, device);
        assert!(stops(device).is_empty(), "{:?}", device.pods.heard());
    }

    /// The daemon gone inside its bound, and its channel closed, so the pods' record is whole.
    fn exited(rig: &mut Rig, device: &Device) -> Instant {
        let status = rig.exited(MIC_EXIT.saturating_add(REAL_TIME_SLACK));
        let at = Instant::now();
        assert!(status.success(), "{status}");
        let closed = device.pods.wait_closed(1, HANG_LIMIT);
        assert!(closed.is_ok(), "the channel never closed: {closed:?}");
        at
    }

    /// The call as it began: the one START and no STOP heard, the uplink running, `mic` recording and no sink call.
    fn assert_call_untouched(rig: &Rig, device: &Device, audio: &FakeAudio) {
        rig.wait_queued_inputs_applied();
        read_up_to_now(rig, device);
        assert_eq!(starts(device).len(), 1, "{:?}", device.pods.heard());
        assert!(stops(device).is_empty(), "{:?}", device.pods.heard());
        assert!(device.pods.uplink().running());
        assert_eq!(mic(rig), "recording");
        assert_no_sink_call(rig, audio);
    }

    /// The two records as the scenario left them, then the rig's own checks.
    fn end(rig: Rig, device: &Device, audio: &FakeAudio, opened: Option<Instant>) -> Ended {
        let ended = Ended {
            stops: stops(device),
            writes: audio.writes(),
            opened,
        };
        rig.finish();
        ended
    }

    fn off_over_music() -> Ended {
        let (mut rig, device, audio, pipewire) = before_a_call(SinkState::Running);
        let recorder = call(&rig, &pipewire);
        set_off(&rig, &device);
        wait_no_node(&pipewire);
        rig.wait_logged(STOP_OWED_PLAYING_LINE);
        read_up_to_now(&rig, &device);
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        assert!(device.pods.uplink().running());
        assert_eq!(mic(&rig), "stopping");
        assert_no_sink_call(&rig, &audio);
        drop(recorder.finish());
        end(rig, &device, &audio, None)
    }

    fn off_then_the_sink_suspends() -> Ended {
        let (mut rig, device, audio, pipewire) = before_a_call(SinkState::Running);
        let recorder = call(&rig, &pipewire);
        set_off(&rig, &device);
        rig.wait_logged(STOP_OWED_PLAYING_LINE);
        device.pods.uplink().hold_stops(true);
        let suspended = Instant::now();
        move_sink(&rig, &audio, &device, SinkState::Suspended);
        let stop = wait_stop(&device);
        assert_gap("the STOP after the suspend", suspended, stop.at, &at_once());
        rig.wait_logged(STOP_QUIET_LINE);
        assert_eq!(mic(&rig), "stopping");
        device.pods.uplink().release_stop();
        wait_for("the uplink to stop", || {
            (!device.pods.uplink().running()).then_some(())
        });
        wait_mic(&rig, "off");
        read_up_to_now(&rig, &device);
        assert_eq!(stops(&device).len(), 1, "{:?}", device.pods.heard());
        assert_no_sink_call(&rig, &audio);
        drop(recorder.finish());
        end(rig, &device, &audio, Some(suspended))
    }

    fn off_then_the_last_bud_leaves() -> Ended {
        let (mut rig, device, audio, pipewire) = before_a_call(SinkState::Running);
        let recorder = call(&rig, &pipewire);
        set_off(&rig, &device);
        rig.wait_logged(STOP_OWED_PLAYING_LINE);
        let left = take_out(&rig, &device);
        let stop = the_stop_after(&device, GRACE);
        assert_after("the STOP after the last bud left", left, stop, GRACE);
        rig.wait_logged(STOP_NOT_WORN_LINE);
        let (suspended, _) = wait_restore(&audio, &device);
        assert!(suspended > stop, "the sink call before the STOP");
        read_up_to_now(&rig, &device);
        assert_eq!(stops(&device).len(), 1, "{:?}", device.pods.heard());
        drop(recorder.finish());
        end(rig, &device, &audio, Some(left.0))
    }

    fn off_then_the_last_bud_leaves_and_the_sink_suspends() -> Ended {
        let (mut rig, device, audio, pipewire) = before_a_call(SinkState::Running);
        let recorder = call(&rig, &pipewire);
        set_off(&rig, &device);
        rig.wait_logged(STOP_OWED_PLAYING_LINE);
        let left = take_out(&rig, &device);
        read_up_to_now(&rig, &device);
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());

        let suspended = Instant::now();
        assert!(suspended.saturating_duration_since(left.0) < GRACE);
        move_sink(&rig, &audio, &device, SinkState::Suspended);
        let stop = wait_stop(&device);
        assert_gap("the STOP after the suspend", suspended, stop.at, &at_once());
        assert!(stop.at.saturating_duration_since(left.0) < GRACE);
        rig.wait_logged(STOP_QUIET_LINE);
        read_up_to_now(&rig, &device);
        assert_eq!(stops(&device).len(), 1, "{:?}", device.pods.heard());
        assert_no_sink_call(&rig, &audio);
        drop(recorder.finish());
        end(rig, &device, &audio, Some(suspended))
    }

    fn exit_over_music() -> Ended {
        let (mut rig, device, audio, pipewire) = before_a_call(SinkState::Running);
        let recorder = call(&rig, &pipewire);
        let termed = Instant::now();
        rig.term();
        let gone = exited(&mut rig, &device);
        assert_gap(
            "the exit after the signal",
            termed,
            gone,
            &not_before(MIC_EXIT),
        );
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        audio.assert_only_reads();
        let log = rig.daemon().log();
        assert_eq!(logged(&mut rig, EXIT_UPLINK_RUNNING_LINE), 1, "{log}");
        assert_eq!(logged(&mut rig, EXIT_BEFORE_RESTORE_LINE), 0, "{log}");
        drop(recorder.finish());
        end(rig, &device, &audio, None)
    }

    fn exit_during_a_stall() -> Ended {
        let (mut rig, device, audio) =
            rig_with_audio("always", &[MODEL_NUMBER], SinkState::Running);
        // Lose audio from the first count while leaving the running and stopped markers on the wire.
        device.pods.uplink().lose(u32::MAX);
        link_heard(&rig, &device);
        wait_mic(&rig, "live");
        let status = rig.status();
        assert_eq!(status["left"]["in_ear"], true);
        assert_eq!(status["right"]["in_ear"], true);
        assert_eq!(status["mic_start_failures"], 0);

        rig.term();
        rig.wait_logged(STOP_OWED_PLAYING_LINE);
        let window = STALL.saturating_add(REAL_TIME_SLACK);
        let retry = device.pods.wait_heard(&START, 2, window);
        assert!(
            retry.is_err(),
            "a stall sent a START during exit: {retry:?}"
        );
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());

        let suspended = Instant::now();
        audio.set_sink_state(&sink_named(device.address), SinkState::Suspended);
        exited(&mut rig, &device);
        let stopped = stops(&device);
        let log = rig.daemon().log();
        assert!(
            !log.lines().any(|line| line.starts_with("mic: stalled,")),
            "{log}"
        );
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        assert_eq!(stopped.len(), 1, "{:?}: {log}", device.pods.heard());
        assert_gap(
            "the STOP after the suspend",
            suspended,
            stopped[0].at,
            &at_once(),
        );
        assert_eq!(logged(&mut rig, STOP_QUIET_LINE), 1, "{log}");
        assert_eq!(logged(&mut rig, EXIT_UPLINK_RUNNING_LINE), 0, "{log}");
        audio.assert_only_reads();
        end(rig, &device, &audio, Some(suspended))
    }

    fn exit_then_the_sink_suspends() -> Ended {
        let (mut rig, device, audio, pipewire) = before_a_call(SinkState::Running);
        let recorder = call(&rig, &pipewire);
        let termed = Instant::now();
        rig.term();
        rig.wait_logged(STOP_OWED_PLAYING_LINE);
        let suspended = Instant::now();
        audio.set_sink_state(&sink_named(device.address), SinkState::Suspended);
        let gone = exited(&mut rig, &device);
        let log = rig.daemon().log();
        let stopped = stops(&device);
        assert_eq!(stopped.len(), 1, "{:?}: {log}", device.pods.heard());
        assert_gap(
            "the STOP after the suspend",
            suspended,
            stopped[0].at,
            &at_once(),
        );
        audio.assert_only_reads();
        assert_eq!(logged(&mut rig, STOP_QUIET_LINE), 1, "{log}");
        assert!(
            log.lines()
                .any(|line| line.starts_with("mic: uplink stopped after ")),
            "{log}"
        );
        assert_eq!(logged(&mut rig, EXIT_UPLINK_RUNNING_LINE), 0, "{log}");
        assert_eq!(logged(&mut rig, EXIT_BEFORE_RESTORE_LINE), 0, "{log}");
        let took = gone.duration_since(termed);
        assert!(took < MIC_EXIT, "the exit took {took:?}: {log}");
        drop(recorder.finish());
        end(rig, &device, &audio, Some(suspended))
    }

    fn exit_over_a_suspended_sink() -> Ended {
        let quiet = Instant::now();
        let (mut rig, device, audio, pipewire) = before_a_call(SinkState::Suspended);
        let recorder = call(&rig, &pipewire);
        let termed = Instant::now();
        rig.term();
        exited(&mut rig, &device);
        let stopped = stops(&device);
        assert_eq!(stopped.len(), 1, "{:?}", device.pods.heard());
        assert_gap(
            "the STOP after the signal",
            termed,
            stopped[0].at,
            &at_once(),
        );
        audio.assert_only_reads();
        let log = rig.daemon().log();
        assert_eq!(logged(&mut rig, STOP_QUIET_LINE), 1, "{log}");
        assert!(
            !log.lines().any(|line| line.starts_with("mic: restore")),
            "{log}"
        );
        assert_eq!(logged(&mut rig, EXIT_UPLINK_RUNNING_LINE), 0, "{log}");
        drop(recorder.finish());
        end(rig, &device, &audio, Some(quiet))
    }

    fn sleep_over_music() -> Ended {
        let (mut rig, device, audio, pipewire) = before_a_call(SinkState::Running);
        let recorder = call(&rig, &pipewire);
        sleep(&mut rig);
        assert_call_untouched(&rig, &device, &audio);
        // The wake finds the uplink it left running, so it starts nothing.
        wake(&mut rig);
        assert_call_untouched(&rig, &device, &audio);
        let log = rig.daemon().log();
        assert!(
            !log.lines().any(|line| line.starts_with("mic: stop")),
            "{log}"
        );
        drop(recorder.finish());
        end(rig, &device, &audio, None)
    }

    #[test]
    fn off_during_a_call_with_music_withdraws_the_node_and_owes_the_stop() {
        drop(off_over_music());
    }

    #[test]
    fn the_owed_stop_goes_out_when_the_sink_suspends() {
        drop(off_then_the_sink_suspends());
    }

    #[test]
    fn the_owed_stop_goes_out_when_the_last_bud_leaves() {
        drop(off_then_the_last_bud_leaves());
    }

    #[test]
    fn the_owed_stop_goes_out_when_the_sink_suspends_during_the_bud_grace() {
        drop(off_then_the_last_bud_leaves_and_the_sink_suspends());
    }

    #[test]
    fn exit_during_a_call_with_music_sends_no_stop_and_no_sink_call() {
        drop(exit_over_music());
    }

    #[test]
    fn exit_with_a_stalled_uplink_waits_for_the_quiet_sink() {
        drop(exit_during_a_stall());
    }

    #[test]
    fn exit_when_the_sink_goes_quiet_inside_the_bound_sends_the_stop() {
        drop(exit_then_the_sink_suspends());
    }

    #[test]
    fn exit_with_the_sink_suspended_sends_stop_and_no_sink_call() {
        drop(exit_over_a_suspended_sink());
    }

    #[test]
    fn sleep_during_a_call_with_music_sends_nothing() {
        drop(sleep_over_music());
    }

    #[test]
    fn no_uplink_stop_and_no_sink_call_while_the_sink_runs() {
        for (number, scenario) in SCENARIOS.into_iter().enumerate() {
            let ended = scenario();
            let shut = |at: Instant| ended.opened.is_none_or(|opened| at < opened);
            let stops: Vec<&Heard> = ended.stops.iter().filter(|heard| shut(heard.at)).collect();
            assert!(stops.is_empty(), "scenario {number}: {stops:?}");
            let after = |asked: &&Asked| !matches!(asked, Asked::Suspend { at, .. } if !shut(*at));
            let calls: Vec<&Asked> = ended.writes.iter().filter(after).collect();
            assert!(calls.is_empty(), "scenario {number}: {calls:?}");
        }
    }
}
