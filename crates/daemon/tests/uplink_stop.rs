//! The end of an uplink through the built daemon against the fake pods and the fake audio server: `off`, the restore after a STOP and the exit.

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

    use pods_core::policy::audio::RESTORE_HOLD_MS;
    use pods_core::policy::mic::{START_ANSWER_MS, STOP_OWED_LINE, STOP_UNCONFIRMED_LINE};
    use pods_fake::Profile;
    use pods_samples::MODEL_NUMBER;
    use pulseaudio::protocol::SinkState;
    use serde_json::Value;

    use super::support::audio::{Asked, FakeAudio, Volumes};
    use super::support::bus::{Listener, changes_rule};
    use super::support::rig::{Device, REAL_TIME_SLACK, Rig, assert_gap, not_before};
    use super::support::settings::set;
    use super::support::uplink::{
        link, link_heard, lose, mic, move_sink, read_up_to_now, rig_with, rig_with_audio, starts,
        stops, wait_mic, wait_restore, wait_starts, wait_stop,
    };
    use super::support::{HANG_LIMIT, pause, wait_for};

    /// The daemon's bound on an exit while the uplink is up; the daemon is a binary, so its constant cannot be imported.
    const MIC_EXIT: Duration = Duration::from_millis(2000);
    /// From the write of `off` to the sink resumed, with the fake answering at once.
    const RESTORED_WITHIN: Duration = Duration::from_millis(2000);
    /// Into the exit when a held START is answered.
    const ANSWERED_INTO_THE_EXIT: Duration = Duration::from_millis(300);
    const HOLD: Duration = Duration::from_millis(RESTORE_HOLD_MS);
    const START_ANSWER: Duration = Duration::from_millis(START_ANSWER_MS);
    const RESTORE_SKIPPED_LINE: &str = "mic: restore skipped, sink suspended";

    fn at_once() -> RangeInclusive<Duration> {
        Duration::ZERO..=REAL_TIME_SLACK
    }

    /// The pods live with `always` and their sink in `state`.
    fn live(state: SinkState) -> (Rig, Device, Rc<FakeAudio>) {
        let (rig, device, audio) = rig_with_audio("always", &[MODEL_NUMBER], state);
        link_heard(&rig, &device);
        wait_mic(&rig, "live");
        (rig, device, audio)
    }

    #[test]
    fn setting_off_while_music_plays_sends_stop_and_one_restore_inside_the_bound() {
        let (rig, device, audio) = live(SinkState::Running);
        let written = Instant::now();
        set(rig.session(), "microphone", "off");
        assert_eq!(mic(&rig), "off");
        assert_gap(
            "the STOP after the write",
            written,
            wait_stop(&device).at,
            &at_once(),
        );
        let (suspended, resumed) = wait_restore(&audio, &device);
        assert_gap("the hold", suspended, resumed, &not_before(HOLD));
        let within = Duration::ZERO..=RESTORED_WITHIN;
        assert_gap("the resume after the write", written, resumed, &within);
        read_up_to_now(&rig, &device);
        assert_eq!(audio.writes().len(), 2, "{:?}", audio.writes());
        rig.finish();
    }

    #[test]
    fn setting_off_with_a_suspended_sink_stops_at_once_and_touches_no_sink() {
        let (mut rig, device, audio) = live(SinkState::Suspended);
        let written = Instant::now();
        set(rig.session(), "microphone", "off");
        assert_gap(
            "the STOP after the write",
            written,
            wait_stop(&device).at,
            &at_once(),
        );
        rig.wait_logged(RESTORE_SKIPPED_LINE);
        audio.assert_only_reads();
        rig.finish();
    }

    #[test]
    fn setting_off_while_starting_restores_after_the_owed_stop() {
        let (rig, device, audio) = rig_with_audio("always", &[MODEL_NUMBER], SinkState::Running);
        device.pods.uplink().hold_starts(true);
        link_heard(&rig, &device);
        wait_starts(&device, 1);
        set(rig.session(), "microphone", "off");
        device.pods.uplink().release_start();
        let stop = wait_stop(&device);
        let (suspended, _) = wait_restore(&audio, &device);
        assert!(suspended > stop.at, "the suspend before the owed STOP");
        rig.finish();
    }

    #[test]
    fn the_restore_waits_for_the_stopped_marker() {
        let (rig, device, audio) = live(SinkState::Running);
        device.pods.uplink().hold_stops(true);
        set(rig.session(), "microphone", "off");
        wait_stop(&device);
        read_up_to_now(&rig, &device);
        assert_eq!(mic(&rig), "off");
        audio.assert_only_reads();
        let released = Instant::now();
        device.pods.uplink().release_stop();
        let (suspended, _) = wait_restore(&audio, &device);
        assert_gap(
            "the suspend after the release",
            released,
            suspended,
            &at_once(),
        );
        rig.finish();
    }

    #[test]
    fn a_suspended_sink_is_left_alone_by_the_restore() {
        let (mut rig, device, audio) = live(SinkState::Running);
        device.pods.uplink().hold_stops(true);
        set(rig.session(), "microphone", "off");
        wait_stop(&device);
        move_sink(&rig, &audio, &device, SinkState::Suspended);
        device.pods.uplink().release_stop();
        rig.wait_logged(RESTORE_SKIPPED_LINE);
        audio.assert_only_reads();
        rig.finish();
    }

    #[test]
    fn an_idle_sink_is_suspended_and_resumed_with_no_hold() {
        let (rig, device, audio) = live(SinkState::Idle);
        set(rig.session(), "microphone", "off");
        let (suspended, resumed) = wait_restore(&audio, &device);
        let under_the_hold = Duration::ZERO..=HOLD.saturating_sub(Duration::from_millis(1));
        assert_gap("suspend to resume", suspended, resumed, &under_the_hold);
        rig.finish();
    }

    #[test]
    fn a_restore_with_no_audio_server_is_done_when_it_returns() {
        let (mut rig, device, audio) = live(SinkState::Running);
        audio.stop();
        rig.wait_audio_server("down");
        set(rig.session(), "microphone", "off");
        wait_stop(&device);
        rig.wait_logged("mic: restore owed, no audio server");
        wait_mic(&rig, "off");
        assert!(audio.writes().is_empty(), "{:?}", audio.writes());
        audio.restart(Volumes::Kept);
        wait_restore(&audio, &device);
        rig.finish();
    }

    #[test]
    fn a_restore_owed_while_the_server_was_down_is_dropped_by_a_newer_start() {
        let (mut rig, device, audio) = live(SinkState::Running);
        audio.stop();
        rig.wait_audio_server("down");
        set(rig.session(), "microphone", "off");
        wait_stop(&device);
        rig.wait_logged("mic: restore owed, no audio server");
        set(rig.session(), "microphone", "always");
        wait_starts(&device, 2);
        wait_mic(&rig, "live");
        audio.restart(Volumes::Kept);
        rig.wait_sink_told();
        assert!(audio.writes().is_empty(), "{:?}", audio.writes());
        rig.finish();
    }

    #[test]
    fn exit_while_running_stops_and_restores_inside_the_bound() {
        let (mut rig, device, audio) = live(SinkState::Running);
        let termed = Instant::now();
        rig.term();
        let status = rig.exited(MIC_EXIT.saturating_add(REAL_TIME_SLACK));
        assert!(status.success(), "{status}");
        assert_gap(
            "the STOP after the signal",
            termed,
            stops(&device)[0].at,
            &at_once(),
        );
        wait_restore(&audio, &device);
        let log = rig.daemon().log();
        assert!(
            log.lines().any(|line| line == "mic: stop sent (exit)"),
            "{log}"
        );
        assert!(
            !log.lines().any(|line| line == "mic: exit before restore"),
            "{log}"
        );
        rig.finish();
    }

    #[test]
    fn exit_while_starting_waits_for_the_marker_then_stops_and_restores() {
        let (mut rig, device, audio) =
            rig_with_audio("always", &[MODEL_NUMBER], SinkState::Running);
        device.pods.uplink().hold_starts(true);
        link_heard(&rig, &device);
        wait_starts(&device, 1);
        rig.term();
        rig.wait_logged(STOP_OWED_LINE);
        pause(ANSWERED_INTO_THE_EXIT);
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        let released = Instant::now();
        device.pods.uplink().release_start();
        let status = rig.exited(MIC_EXIT.saturating_add(REAL_TIME_SLACK));
        assert!(status.success(), "{status}");
        let stopped = stops(&device);
        assert_eq!(stopped.len(), 1, "{:?}", device.pods.heard());
        assert_gap(
            "the STOP after the answer",
            released,
            stopped[0].at,
            &at_once(),
        );
        wait_restore(&audio, &device);
        rig.finish();
    }

    #[test]
    fn exit_while_starting_and_never_answered_sends_nothing_and_ends_at_the_start_deadline() {
        let (mut rig, device, audio) =
            rig_with_audio("always", &[MODEL_NUMBER], SinkState::Running);
        device.pods.uplink().never_answer(true);
        link_heard(&rig, &device);
        let start = wait_starts(&device, 1)[0].at;
        rig.term();
        let status = rig.exited(MIC_EXIT.saturating_add(REAL_TIME_SLACK));
        assert!(status.success(), "{status}");
        assert_gap(
            "the exit after the START",
            start,
            Instant::now(),
            &not_before(START_ANSWER),
        );
        assert!(stops(&device).is_empty(), "{:?}", device.pods.heard());
        assert_eq!(starts(&device).len(), 1, "{:?}", device.pods.heard());
        audio.assert_only_reads();
        rig.finish();
    }

    #[test]
    fn exit_with_the_stop_and_the_restore_stalled_ends_at_the_bound() {
        let (mut rig, device, audio) = live(SinkState::Running);
        device.pods.uplink().hold_stops(true);
        audio.hold_replies(true);
        let asked = audio.requests().len();
        // The socket and the state file close at the signal, so only the bus shows the exit.
        let bus = Listener::subscribe(rig.session_bus(), &changes_rule());
        let termed = Instant::now();
        rig.term();
        wait_for("mic stopping on the bus", || {
            let status: Value = serde_json::from_str(&bus.property("StatusJson")).unwrap();
            (status["mic"] == "stopping").then_some(())
        });
        let status = rig.exited(MIC_EXIT.saturating_add(REAL_TIME_SLACK));
        assert!(status.success(), "{status}");
        assert_gap(
            "the exit after the signal",
            termed,
            Instant::now(),
            &not_before(MIC_EXIT),
        );
        let closed = device
            .pods
            .wait_closed(1, HANG_LIMIT)
            .expect("the channel closes")[0];
        assert_gap(
            "the channel close after the signal",
            termed,
            closed,
            &not_before(MIC_EXIT),
        );
        assert_eq!(stops(&device).len(), 1, "{:?}", device.pods.heard());
        assert!(audio.requests().len() > asked, "{:?}", audio.requests());
        let log = rig.daemon().log();
        let unconfirmed = log.lines().position(|line| line == STOP_UNCONFIRMED_LINE);
        let cut = log
            .lines()
            .position(|line| line == "mic: exit before restore");
        assert!(
            unconfirmed.is_some_and(|at| cut.is_some_and(|cut| at < cut)),
            "{log}"
        );
        assert!(audio.writes().is_empty(), "{:?}", audio.writes());
        rig.finish();
    }

    #[test]
    fn exit_with_the_server_lost_inside_the_restore_resumes_the_sink_before_it_ends() {
        let (mut rig, device, audio) = live(SinkState::Running);
        rig.term();
        audio.wait_writes(1, HANG_LIMIT);
        audio.restart(Volumes::Kept);
        let status = rig.exited(MIC_EXIT.saturating_add(REAL_TIME_SLACK));
        assert!(status.success(), "{status}");
        wait_restore(&audio, &device);
        let requests = audio.requests();
        let resume = requests
            .iter()
            .position(|asked| matches!(asked, Asked::Suspend { suspend: false, .. }));
        let reconnected = requests[..resume.unwrap()]
            .iter()
            .filter(|asked| matches!(asked, Asked::Auth { .. }))
            .count();
        assert_eq!(reconnected, 2, "{requests:?}");
        rig.finish();
    }

    #[test]
    fn exit_with_the_uplink_stopped_is_answered_at_once() {
        let (mut rig, device) = rig_with("auto", &[MODEL_NUMBER], Profile::default());
        link(&rig, &device);
        assert_eq!(mic(&rig), "idle");
        let termed = Instant::now();
        rig.term();
        let status = rig.exited(HANG_LIMIT);
        assert!(status.success(), "{status}");
        assert_gap(
            "the exit after the signal",
            termed,
            Instant::now(),
            &at_once(),
        );
        rig.finish();
    }

    #[test]
    fn a_link_lost_while_stopping_orders_no_restore() {
        let (rig, device, audio) = live(SinkState::Running);
        device.pods.uplink().hold_stops(true);
        set(rig.session(), "microphone", "off");
        wait_stop(&device);
        lose(&rig, &device);
        device.pods.uplink().release_stop();
        link(&rig, &device);
        rig.wait_sink_told();
        assert!(audio.writes().is_empty(), "{:?}", audio.writes());
        rig.finish();
    }

    #[test]
    fn a_resume_the_server_never_took_is_sent_when_it_returns() {
        let (rig, device, audio) = live(SinkState::Running);
        set(rig.session(), "microphone", "off");
        audio.wait_writes(1, HANG_LIMIT);
        audio.stop();
        rig.wait_audio_server("down");
        assert_eq!(audio.writes().len(), 1, "{:?}", audio.writes());
        audio.restart(Volumes::Kept);
        wait_restore(&audio, &device);
        rig.finish();
    }

    #[test]
    fn a_suspend_left_by_a_lost_resume_is_undone_after_a_newer_start() {
        let (rig, device, audio) = live(SinkState::Running);
        set(rig.session(), "microphone", "off");
        audio.wait_writes(1, HANG_LIMIT);
        audio.stop();
        rig.wait_audio_server("down");
        assert_eq!(audio.writes().len(), 1, "{:?}", audio.writes());
        set(rig.session(), "microphone", "always");
        wait_starts(&device, 2);
        wait_mic(&rig, "live");
        audio.restart(Volumes::Kept);
        wait_restore(&audio, &device);
        rig.finish();
    }

    #[test]
    fn an_event_cut_by_the_restore_is_still_read_whole() {
        let (rig, device, audio) = live(SinkState::Running);
        audio.send_split_event();
        pause(REAL_TIME_SLACK);
        let asked = audio.requests().len();
        set(rig.session(), "microphone", "off");
        audio.wait_requests(asked.saturating_add(1), HANG_LIMIT);
        audio.hold_replies(false);
        wait_restore(&audio, &device);
        let requests = audio.requests();
        let connections = requests
            .iter()
            .filter(|asked| matches!(asked, Asked::Auth { .. }))
            .count();
        assert_eq!(connections, 1, "{requests:?}");
        rig.finish();
    }
}
