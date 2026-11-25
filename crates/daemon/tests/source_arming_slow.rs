//! Arming through the built daemon across the real-time linger, playback gate and source retry ladder.

#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use pods_core::policy::mic::{
        BUD_GRACE_MS, STOP_NOT_WORN_LINE, STOP_QUIET_LINE, STOP_WAITS_LINE, UPLINK_LINGER_MS,
    };
    use pods_pw::source::PW_OPEN_MS;
    use pods_pwrig::{SOCKET, Server};
    use pods_samples::MODEL_NUMBER;
    use pulseaudio::protocol::SinkState;

    use super::support::audio::{FakeAudio, LADDER_MS, Volumes};
    use super::support::pipewire::{start, wait_node};
    use super::support::rig::{Device, REAL_TIME_SLACK, Rig, assert_gap};
    use super::support::uplink::{
        assert_after, assert_no_sink_call, link_heard, move_sink, read_up_to_now, rig_with_audio,
        starts, stops, take_out, the_stop_after, wait_mic, wait_restore, wait_starts, wait_stop,
    };
    use super::support::{HANG_LIMIT, wait_for, wait_within};

    const LINGER: Duration = Duration::from_millis(UPLINK_LINGER_MS);
    const GRACE: Duration = Duration::from_millis(BUD_GRACE_MS);
    const PAST_LINGER: Duration = LINGER.saturating_add(REAL_TIME_SLACK);
    const ACCEPT_SLACK: Duration = Duration::from_millis(50);
    const OPEN_WITHIN: Duration = Duration::from_millis(PW_OPEN_MS);
    const RESTORE_OWED: &str = "mic: restore owed, no audio server";

    fn armed(sink: SinkState) -> (Rig, Device, Rc<FakeAudio>, Server, u32) {
        let (rig, device, audio) = rig_with_audio("auto", &[MODEL_NUMBER], sink);
        let pipewire = start(&rig);
        link_heard(&rig, &device);
        wait_node(&pipewire);
        wait_mic(&rig, "ready");
        let client = audio.add_client(Some("true"));
        wait_starts(&device, 1);
        wait_mic(&rig, "live");
        (rig, device, audio, pipewire, client)
    }

    #[test]
    #[ignore = "the full linger at real time; the slow job runs it"]
    fn the_program_leaving_lingers_then_waits_for_the_sink_to_go_quiet() {
        let (mut rig, device, audio, _pipewire, client) = armed(SinkState::Running);
        let left = Instant::now();
        audio.remove_client(client);
        wait_within(
            "the linger to end with a STOP waiting for playback",
            LINGER.saturating_add(HANG_LIMIT),
            || {
                rig.daemon()
                    .log()
                    .lines()
                    .any(|line| line == STOP_WAITS_LINE)
                    .then_some(())
            },
        );
        assert_gap("the program's linger", left, Instant::now(), &(LINGER..));
        read_up_to_now(&rig, &device);
        assert_eq!(stops(&device), []);
        move_sink(&rig, &audio, &device, SinkState::Idle);
        rig.holds_for(
            REAL_TIME_SLACK,
            "the uplink held while the sink idles",
            |state| state["mic"] == "live" && stops(&device).is_empty(),
        );
        move_sink(&rig, &audio, &device, SinkState::Suspended);
        wait_stop(&device);
        rig.wait_logged(STOP_QUIET_LINE);
        wait_mic(&rig, "ready");
        assert_no_sink_call(&rig, &audio);
        rig.finish();
        audio.assert_only_reads();
    }

    #[test]
    #[ignore = "a server outage longer than the linger at real time; the slow job runs it"]
    fn an_audio_server_gone_past_the_linger_sends_no_stop_while_a_bud_is_worn() {
        let (mut rig, device, audio, _pipewire, _) = armed(SinkState::Suspended);
        audio.stop();
        rig.wait_audio_server("down");
        rig.holds_for(
            PAST_LINGER,
            "the uplink held with the audio server down",
            |state| state["audio_server"] == "down" && stops(&device).is_empty(),
        );
        rig.wait_logged(STOP_WAITS_LINE);
        audio.restart(Volumes::Kept);
        rig.wait_audio_server("up");
        rig.wait_sink_told();
        read_up_to_now(&rig, &device);
        assert_eq!(starts(&device).len(), 1);
        assert_eq!(stops(&device), []);
        assert!(device.pods.uplink().running());
        rig.finish();
        audio.assert_only_reads();
    }

    #[test]
    #[ignore = "the bud grace and owed restore at real time; the slow job runs it"]
    fn the_same_with_no_bud_worn_stops_and_owes_the_restore() {
        let (mut rig, device, audio, _pipewire, _) = armed(SinkState::Running);
        audio.stop();
        rig.wait_audio_server("down");
        let left = take_out(&rig, &device);
        let stop = the_stop_after(&device, GRACE);
        assert_after("the STOP after the last bud left", left, stop, GRACE);
        rig.wait_logged(STOP_NOT_WORN_LINE);
        rig.wait_logged(RESTORE_OWED);
        audio.assert_only_reads();
        audio.restart(Volumes::Kept);
        rig.wait_audio_server("up");
        let (suspended, resumed) = wait_restore(&audio, &device);
        assert!(stop < suspended && suspended < resumed);
        assert_eq!(stops(&device).len(), 1);
        rig.finish();
    }

    fn accept(socket: &UnixListener) -> UnixStream {
        wait_for("a source open on the silent server", || {
            match socket.accept() {
                Ok((stream, _)) => Some(stream),
                Err(error) if error.kind() == ErrorKind::WouldBlock => None,
                Err(error) => panic!("accept on the silent server: {error}"),
            }
        })
    }

    #[test]
    #[ignore = "seven attempts up the source ladder at real time; the slow job runs it"]
    fn the_node_retries_on_the_ladder_while_pipewire_is_down() {
        let (rig, device, audio) = rig_with_audio("auto", &[MODEL_NUMBER], SinkState::Suspended);
        let socket = rig.session().runtime_dir().join(SOCKET);
        assert!(socket.is_absolute() && socket.starts_with(rig.session().dir.path()));
        let listener = UnixListener::bind(socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        audio.add_client(Some("true"));
        rig.link_up(&device);
        let first = accept(&listener);
        let mut earlier = Instant::now();
        wait_starts(&device, 1);
        wait_mic(&rig, "live");
        let [.., last] = LADDER_MS;
        let mut held = first;
        for delay in LADDER_MS.into_iter().chain([last]) {
            let next = accept(&listener);
            let later = Instant::now();
            let wanted = OPEN_WITHIN.saturating_add(Duration::from_millis(delay));
            let range =
                wanted.saturating_sub(ACCEPT_SLACK)..=wanted.saturating_add(REAL_TIME_SLACK);
            assert_gap("source retry spacing", earlier, later, &range);
            earlier = later;
            drop(std::mem::replace(&mut held, next));
        }
        read_up_to_now(&rig, &device);
        assert_eq!(starts(&device).len(), 1);
        assert_eq!(stops(&device), []);
        drop(held);
        rig.finish();
        audio.assert_only_reads();
    }
}
