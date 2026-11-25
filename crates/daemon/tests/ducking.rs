//! Conversation Awareness through the built daemon, fake pods and fake audio server.

#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::time::Duration;

    use pods_fake::Profile;
    use pods_pwrig::Server;
    use pods_samples::MODEL_NUMBER;
    use pulseaudio::protocol::SinkState;

    use super::support::audio::wire::sink_named;
    use super::support::audio::{Asked, FakeAudio, Recorder, Volumes};
    use super::support::pipewire::{node_name, start, wait_node};
    use super::support::rig::{Device, Rig, connected};
    use super::support::uplink::{link_heard, read_up_to_now, rig_with_audio, wait_mic};
    use super::support::{HANG_LIMIT, wait_for};

    const SAVED: u8 = 80;
    const LOWERED: u8 = 15;
    const USER_VOLUME: u8 = 55;
    const OTHER_SAVED: u8 = 50;
    const OTHER_LOWERED: u8 = 10;
    const AFTER_RESTORE: u8 = 30;
    const AFTER_DUCK: u8 = 35;
    const BEFORE_DUCK_WRITE: u8 = 45;
    const STARTED: u8 = 0x01;
    const CONTINUED: u8 = 0x02;
    const ENDED: u8 = 0x08;
    const ALSO_ENDED: u8 = 0x09;
    const UNKNOWN: u8 = 0xff;
    const DICTATION: &str = "superwhisper-capture";
    const SPEAKERS: &str = "alsa_output.speakers";
    const STOP_WAIT: Duration = Duration::from_millis(2500);

    fn enable(rig: &Rig, enabled: bool) {
        let word = if enabled { "true" } else { "false" };
        let output = rig.client(&["set", "conversational_awareness", word]);
        assert!(output.status.success(), "set awareness {word}: {output:?}");
        rig.wait_status("the requested awareness setting", |status| {
            status["conversational_awareness"] == enabled
        });
    }

    fn ready(volume: u8) -> (Rig, Device, Rc<FakeAudio>) {
        let (rig, device, audio) = Rig::with_audio(volume);
        rig.wait_sink_told();
        enable(&rig, true);
        (rig, device, audio)
    }

    fn recording_ready() -> (Rig, Device, Rc<FakeAudio>, Server) {
        let (rig, device, audio) = rig_with_audio("auto", &[MODEL_NUMBER], SinkState::Running);
        let pipewire = start(&rig);
        link_heard(&rig, &device);
        wait_node(&pipewire);
        wait_mic(&rig, "ready");
        audio.add_source(&node_name());
        let sink = sink_named(device.address);
        audio.set_volume(&sink, SAVED);
        rig.wait_sink_told();
        enable(&rig, true);
        (rig, device, audio, pipewire)
    }

    fn speak(rig: &Rig, device: &Device, level: u8) {
        device.pods.speak(level);
        read_up_to_now(rig, device);
    }

    fn wait_volume(audio: &FakeAudio, device: &Device, percent: u8) {
        let sink = sink_named(device.address);
        wait_for(&format!("the pods' sink at {percent}%"), || {
            (audio.volume(&sink) == percent).then_some(())
        });
    }

    fn change_volume(rig: &Rig, audio: &FakeAudio, name: &str, volume: u8) {
        let before = audio.requests().len();
        audio.set_volume(name, volume);
        wait_for("the refresh after a volume change", || {
            audio.requests()[before..]
                .contains(&Asked::SinkList)
                .then_some(())
        });
        rig.wait_sink_told();
    }

    fn duck(rig: &Rig, device: &Device, audio: &FakeAudio) {
        speak(rig, device, STARTED);
        wait_volume(audio, device, LOWERED);
    }

    fn hold_sink_refresh(audio: &FakeAudio) {
        let before = audio.requests().len();
        audio.request_sink_refresh_and_hold_replies();
        wait_for("a sink refresh with its reply held", || {
            audio.requests()[before..]
                .contains(&Asked::SinkList)
                .then_some(())
        });
    }

    fn tell_sink_with_events_silenced(rig: &Rig, audio: &FakeAudio) {
        hold_sink_refresh(audio);
        audio.hold_replies(false);
        // The next query proves the first reading was queued ahead of the driver's barrier.
        hold_sink_refresh(audio);
        rig.wait_queued_inputs_applied();
        audio.hold_replies(false);
    }

    fn assert_writes(audio: &FakeAudio, device: &Device, expected: &[u8]) {
        let sink = sink_named(device.address);
        let volumes: Vec<u8> = audio
            .writes()
            .into_iter()
            .map(|write| {
                let Asked::Volume {
                    name,
                    index: Some(_),
                    percent,
                } = write
                else {
                    panic!("a write other than indexed sink volume");
                };
                assert!(name == sink, "volume written on a different sink");
                percent
            })
            .collect();
        assert_eq!(volumes, expected, "volume writes to the pods' sink");
    }

    fn finish(rig: Rig, audio: &FakeAudio) {
        rig.finish();
        assert!(
            audio
                .writes()
                .iter()
                .all(|write| matches!(write, Asked::Volume { index: Some(_), .. })),
            "Conversation Awareness may only write volume by sink index"
        );
    }

    #[test]
    fn speech_lowers_the_volume_to_a_fifth_on_the_grid() {
        let cases = [(SAVED, LOWERED), (50, 10), (12, 0)];
        for (saved, lowered) in cases {
            let (rig, device, audio) = ready(saved);
            speak(&rig, &device, STARTED);
            wait_volume(&audio, &device, lowered);
            finish(rig, &audio);
            assert_writes(&audio, &device, &[lowered, saved]);
        }
    }

    #[test]
    fn the_second_level_neither_lowers_again_nor_restores() {
        let (rig, device, audio) = ready(SAVED);
        duck(&rig, &device, &audio);
        speak(&rig, &device, CONTINUED);
        rig.wait_sink_told();
        assert_writes(&audio, &device, &[LOWERED]);
        finish(rig, &audio);
        assert_writes(&audio, &device, &[LOWERED, SAVED]);
    }

    #[test]
    fn the_end_restores_the_saved_volume() {
        for level in [ENDED, ALSO_ENDED] {
            let (rig, device, audio) = ready(SAVED);
            duck(&rig, &device, &audio);
            speak(&rig, &device, level);
            wait_volume(&audio, &device, SAVED);
            finish(rig, &audio);
        }
    }

    #[test]
    fn a_new_conversation_uses_the_unconfirmed_restore_target() {
        let (rig, device, audio) = ready(OTHER_SAVED);
        speak(&rig, &device, STARTED);
        wait_volume(&audio, &device, OTHER_LOWERED);
        rig.wait_sink_told();

        audio.hold_replies(true);
        speak(&rig, &device, ENDED);
        speak(&rig, &device, STARTED);
        audio.hold_replies(false);
        audio.wait_writes(3, HANG_LIMIT);
        rig.wait_sink_told();

        speak(&rig, &device, ENDED);
        finish(rig, &audio);
        assert_writes(
            &audio,
            &device,
            &[OTHER_LOWERED, OTHER_SAVED, OTHER_LOWERED, OTHER_SAVED],
        );
        assert_eq!(audio.volume(&sink_named(device.address)), OTHER_SAVED);
    }

    #[test]
    fn a_late_duck_echo_cannot_replace_the_unconfirmed_restore_target() {
        let (rig, device, audio) = ready(OTHER_SAVED);
        audio.silence_events();
        speak(&rig, &device, STARTED);
        wait_volume(&audio, &device, OTHER_LOWERED);

        speak(&rig, &device, ENDED);
        wait_volume(&audio, &device, OTHER_SAVED);
        // The restore sampled the lowered volume before writing. The next read is held at 50%.
        hold_sink_refresh(&audio);
        rig.wait_queued_inputs_applied();
        speak(&rig, &device, STARTED);
        audio.hold_replies(false);
        audio.wait_writes(3, HANG_LIMIT);

        hold_sink_refresh(&audio);
        rig.wait_queued_inputs_applied();
        audio.hold_replies(false);
        speak(&rig, &device, ENDED);
        finish(rig, &audio);
        assert_writes(
            &audio,
            &device,
            &[OTHER_LOWERED, OTHER_SAVED, OTHER_LOWERED, OTHER_SAVED],
        );
        assert_eq!(audio.volume(&sink_named(device.address)), OTHER_SAVED);
    }

    #[test]
    fn a_wearer_change_before_the_restore_echo_becomes_the_next_saved_volume() {
        let (rig, device, audio) = ready(OTHER_SAVED);
        let sink = sink_named(device.address);
        audio.silence_events();
        speak(&rig, &device, STARTED);
        wait_volume(&audio, &device, OTHER_LOWERED);
        tell_sink_with_events_silenced(&rig, &audio);

        speak(&rig, &device, ENDED);
        wait_volume(&audio, &device, OTHER_SAVED);
        audio.set_volume(&sink, AFTER_RESTORE);
        tell_sink_with_events_silenced(&rig, &audio);

        speak(&rig, &device, STARTED);
        audio.wait_writes(3, HANG_LIMIT);
        tell_sink_with_events_silenced(&rig, &audio);
        speak(&rig, &device, ENDED);
        finish(rig, &audio);
        assert_eq!(audio.volume(&sink), AFTER_RESTORE);
    }

    #[test]
    fn a_wearer_change_before_the_duck_echo_is_kept_after_the_end() {
        let (rig, device, audio) = ready(OTHER_SAVED);
        let sink = sink_named(device.address);
        audio.silence_events();
        speak(&rig, &device, STARTED);
        wait_volume(&audio, &device, OTHER_LOWERED);
        audio.set_volume(&sink, AFTER_DUCK);
        tell_sink_with_events_silenced(&rig, &audio);

        speak(&rig, &device, ENDED);
        finish(rig, &audio);
        assert_eq!(audio.volume(&sink), AFTER_DUCK);
        assert_writes(&audio, &device, &[OTHER_LOWERED]);
    }

    #[test]
    fn a_wearer_change_sampled_by_the_duck_refresh_is_restored() {
        let (rig, device, audio) = ready(OTHER_SAVED);
        let sink = sink_named(device.address);
        audio.silence_events();
        audio.hold_replies(true);
        let before = audio.requests().len();
        speak(&rig, &device, STARTED);
        wait_for(
            "the server query before the duck refresh samples the sink",
            || {
                audio.requests()[before..]
                    .contains(&Asked::ServerInfo)
                    .then_some(())
            },
        );
        audio.set_volume(&sink, BEFORE_DUCK_WRITE);
        audio.hold_replies(false);
        wait_volume(&audio, &device, OTHER_LOWERED);
        tell_sink_with_events_silenced(&rig, &audio);

        speak(&rig, &device, ENDED);
        finish(rig, &audio);
        assert_eq!(audio.volume(&sink), BEFORE_DUCK_WRITE);
        assert_writes(&audio, &device, &[OTHER_LOWERED, BEFORE_DUCK_WRITE]);
    }

    #[test]
    fn an_unknown_level_byte_changes_nothing() {
        let (rig, device, audio) = ready(SAVED);
        speak(&rig, &device, UNKNOWN);
        rig.wait_sink_told();
        assert_writes(&audio, &device, &[]);
        duck(&rig, &device, &audio);
        speak(&rig, &device, UNKNOWN);
        rig.wait_sink_told();
        assert_writes(&audio, &device, &[LOWERED]);
        finish(rig, &audio);
    }

    #[test]
    fn a_user_change_while_lowered_is_kept_after_the_end() {
        let (rig, device, audio) = ready(SAVED);
        duck(&rig, &device, &audio);
        let sink = sink_named(device.address);
        change_volume(&rig, &audio, &sink, USER_VOLUME);
        speak(&rig, &device, CONTINUED);
        speak(&rig, &device, ENDED);
        finish(rig, &audio);
        assert_eq!(audio.volume(&sink), USER_VOLUME);
        assert_writes(&audio, &device, &[LOWERED]);
    }

    #[test]
    fn awareness_off_changes_nothing() {
        let (rig, device, audio) = Rig::with_audio(SAVED);
        rig.wait_sink_told();
        enable(&rig, false);
        speak(&rig, &device, STARTED);
        speak(&rig, &device, ENDED);
        rig.wait_sink_told();
        assert_writes(&audio, &device, &[]);
        enable(&rig, true);
        duck(&rig, &device, &audio);
        finish(rig, &audio);
        assert_writes(&audio, &device, &[LOWERED, SAVED]);
    }

    #[test]
    fn turning_awareness_off_while_lowered_restores() {
        let (rig, device, audio) = ready(SAVED);
        duck(&rig, &device, &audio);
        enable(&rig, false);
        wait_volume(&audio, &device, SAVED);
        finish(rig, &audio);
        assert_writes(&audio, &device, &[LOWERED, SAVED]);
    }

    #[test]
    fn pods_that_are_not_the_default_sink_change_nothing() {
        let (rig, device, audio) = ready(SAVED);
        audio.add_sink(SPEAKERS, SAVED);
        audio.set_default_sink(SPEAKERS);
        rig.wait_sink_told();
        speak(&rig, &device, STARTED);
        speak(&rig, &device, ENDED);
        rig.wait_sink_told();
        assert_writes(&audio, &device, &[]);
        audio.set_default_sink(&sink_named(device.address));
        rig.wait_sink_told();
        duck(&rig, &device, &audio);
        finish(rig, &audio);
        assert_writes(&audio, &device, &[LOWERED, SAVED]);
    }

    #[test]
    fn a_link_lost_while_lowered_restores_on_the_next_connect() {
        let (rig, device, audio) = ready(SAVED);
        duck(&rig, &device, &audio);
        rig.link_down(&device);
        rig.wait_status("the link down", |status| !connected(status));
        audio.wait_clients(0, HANG_LIMIT);
        assert_writes(&audio, &device, &[LOWERED]);
        rig.relink(&device);
        wait_volume(&audio, &device, SAVED);
        finish(rig, &audio);
        assert_writes(&audio, &device, &[LOWERED, SAVED]);
    }

    #[test]
    fn a_link_lost_with_a_held_duck_echo_restores_on_the_next_connect() {
        let (rig, device, audio) = ready(SAVED);
        audio.silence_events();
        duck(&rig, &device, &audio);
        hold_sink_refresh(&audio);

        rig.link_down(&device);
        rig.wait_status("the link down", |status| !connected(status));
        audio.wait_clients(0, HANG_LIMIT);
        audio.hold_replies(false);
        rig.relink(&device);
        wait_volume(&audio, &device, SAVED);
        finish(rig, &audio);
        assert_writes(&audio, &device, &[LOWERED, SAVED]);
    }

    #[test]
    fn a_replaced_sink_index_receives_the_saved_volume() {
        let (rig, device, audio) = ready(SAVED);
        duck(&rig, &device, &audio);
        audio.replace_default_sink(&sink_named(device.address), LOWERED);
        wait_volume(&audio, &device, SAVED);
        finish(rig, &audio);
        assert_writes(&audio, &device, &[LOWERED, SAVED]);
        let writes = audio.writes();
        let [
            Asked::Volume { index: lowered, .. },
            Asked::Volume {
                index: restored, ..
            },
        ] = writes.as_slice()
        else {
            panic!("expected two indexed volume writes");
        };
        assert_ne!(lowered, restored, "the restore uses the new sink index");
    }

    #[test]
    fn a_replaced_sink_index_can_be_the_first_to_report_the_duck_volume() {
        let (rig, device, audio) = ready(SAVED);
        audio.silence_events();
        duck(&rig, &device, &audio);
        audio.replace_default_sink(&sink_named(device.address), LOWERED);
        hold_sink_refresh(&audio);
        rig.wait_queued_inputs_applied();
        audio.hold_replies(false);

        wait_volume(&audio, &device, SAVED);
        finish(rig, &audio);
        assert_writes(&audio, &device, &[LOWERED, SAVED]);
    }

    #[test]
    fn a_different_pair_taking_over_keeps_each_restore_with_its_sink() {
        let (mut rig, first, audio) = ready(SAVED);
        let second = rig.add_pods(Profile::default());
        let first_sink = sink_named(first.address);
        let second_sink = sink_named(second.address);
        audio.add_sink(&second_sink, OTHER_SAVED);
        duck(&rig, &first, &audio);

        rig.link_down(&first);
        rig.wait_status("the first link down", |status| !connected(status));
        audio.wait_clients(0, HANG_LIMIT);
        audio.set_default_sink(&second_sink);
        rig.relink(&second);
        rig.wait_sink_told();
        speak(&rig, &second, STARTED);
        wait_volume(&audio, &second, OTHER_LOWERED);
        assert_eq!(audio.volume(&first_sink), LOWERED);

        rig.link_down(&second);
        rig.wait_status("the second link down", |status| !connected(status));
        audio.wait_clients(0, HANG_LIMIT);
        audio.set_default_sink(&first_sink);
        rig.relink(&first);
        wait_volume(&audio, &first, SAVED);
        assert_eq!(audio.volume(&second_sink), OTHER_LOWERED);

        rig.link_down(&first);
        rig.wait_status("the first link down again", |status| !connected(status));
        audio.wait_clients(0, HANG_LIMIT);
        audio.set_default_sink(&second_sink);
        rig.relink(&second);
        wait_volume(&audio, &second, OTHER_SAVED);
        finish(rig, &audio);
        assert_eq!(audio.writes().len(), 4);
    }

    #[test]
    fn stop_while_lowered_restores_before_exit() {
        let (mut rig, device, audio) = ready(SAVED);
        duck(&rig, &device, &audio);
        assert!(rig.stop(STOP_WAIT).success(), "the daemon exits cleanly");
        assert_eq!(audio.volume(&sink_named(device.address)), SAVED);
        finish(rig, &audio);
        assert_writes(&audio, &device, &[LOWERED, SAVED]);
    }

    #[test]
    fn the_audio_server_restarting_while_lowered_gets_the_saved_volume_back() {
        let (rig, device, audio) = ready(SAVED);
        duck(&rig, &device, &audio);
        audio.stop();
        rig.wait_audio_server("down");
        assert_writes(&audio, &device, &[LOWERED]);
        audio.restart(Volumes::Kept);
        wait_volume(&audio, &device, SAVED);

        rig.wait_sink_told();
        duck(&rig, &device, &audio);
        audio.stop();
        rig.wait_audio_server("down");
        let sink = sink_named(device.address);
        audio.set_volume(&sink, USER_VOLUME);
        audio.restart(Volumes::Kept);
        rig.wait_sink_told();
        speak(&rig, &device, ENDED);
        finish(rig, &audio);
        assert_eq!(audio.volume(&sink), USER_VOLUME);
        assert_writes(&audio, &device, &[LOWERED, SAVED, LOWERED]);
    }

    #[test]
    fn only_two_volume_writes_are_made_per_conversation() {
        let (rig, device, audio) = ready(SAVED);
        duck(&rig, &device, &audio);
        speak(&rig, &device, CONTINUED);
        speak(&rig, &device, UNKNOWN);
        speak(&rig, &device, ENDED);
        speak(&rig, &device, ALSO_ENDED);
        wait_volume(&audio, &device, SAVED);
        finish(rig, &audio);
        assert_writes(&audio, &device, &[LOWERED, SAVED]);
    }

    #[test]
    fn a_recorder_linking_while_lowered_restores() {
        let (mut rig, device, audio, _pipewire) = recording_ready();
        duck(&rig, &device, &audio);
        audio.add_recorder(&node_name(), Recorder::Program);
        rig.wait_logged("mic: recorders (dictation 0, other 1)");
        wait_volume(&audio, &device, SAVED);
        finish(rig, &audio);
        assert_writes(&audio, &device, &[LOWERED, SAVED]);
    }

    #[test]
    fn a_recording_keeps_the_wearers_adjustment_until_the_conversation_ends() {
        let (mut rig, device, audio, _pipewire) = recording_ready();
        duck(&rig, &device, &audio);
        let sink = sink_named(device.address);
        change_volume(&rig, &audio, &sink, USER_VOLUME);
        let id = audio.add_recorder(&node_name(), Recorder::Program);
        rig.wait_logged("mic: recorders (dictation 0, other 1)");
        audio.remove_recorder(id);
        rig.wait_logged("mic: recorders (dictation 0, other 0)");

        speak(&rig, &device, CONTINUED);
        rig.wait_sink_told();
        assert_eq!(audio.volume(&sink), USER_VOLUME);
        assert_writes(&audio, &device, &[LOWERED]);

        speak(&rig, &device, ENDED);
        speak(&rig, &device, STARTED);
        let lowered_user_volume = 10;
        wait_volume(&audio, &device, lowered_user_volume);
        speak(&rig, &device, ENDED);
        wait_volume(&audio, &device, USER_VOLUME);
        finish(rig, &audio);
        assert_writes(
            &audio,
            &device,
            &[LOWERED, lowered_user_volume, USER_VOLUME],
        );
    }

    #[test]
    fn speech_during_a_recording_lowers_nothing() {
        let (mut rig, device, audio, _pipewire) = recording_ready();
        let id = audio.add_recorder(&node_name(), Recorder::Named(DICTATION));
        rig.wait_logged("mic: recorders (dictation 1, other 0)");
        speak(&rig, &device, STARTED);
        speak(&rig, &device, ENDED);
        rig.wait_sink_told();
        assert_writes(&audio, &device, &[]);
        audio.remove_recorder(id);
        rig.wait_logged("mic: recorders (dictation 0, other 0)");
        duck(&rig, &device, &audio);
        finish(rig, &audio);
        assert_writes(&audio, &device, &[LOWERED, SAVED]);
    }

    #[test]
    fn no_sink_suspend_is_ever_written() {
        let (rig, device, audio) = ready(SAVED);
        duck(&rig, &device, &audio);
        speak(&rig, &device, ENDED);
        wait_volume(&audio, &device, SAVED);
        finish(rig, &audio);
        assert_writes(&audio, &device, &[LOWERED, SAVED]);
        assert!(
            audio
                .requests()
                .iter()
                .all(|asked| !matches!(asked, Asked::Suspend { .. })),
            "Conversation Awareness wrote a sink suspend"
        );
    }
}
