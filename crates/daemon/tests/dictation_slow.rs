//! Dictation through link loss, bearer loss and the logind sleep edge.

#[cfg(test)]
#[expect(dead_code, reason = "each suite uses part of the shared support")]
mod support;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::support::dictation::{CAPTURE, Fixture, HOST_STOP, ready_to_read, status};
    use super::support::pipewire::{record, wait_no_node};
    use super::support::recording::{assert_sounds, first_sound, frames, longest_zero_run};
    use super::support::sleep::sleep;
    use super::support::uplink::{assert_no_sink_call, lose};
    use super::support::wait_for;

    const AUDIO: Duration = Duration::from_millis(400);

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
        lose(&fixture.rig, &fixture.device);
        wait_no_node(&fixture.server);
        status(&fixture.rig, "off");
        assert_sounds(&recorder.finish(), "");
        assert_eq!(fixture.device.pods.doap().stops(), [0_u8; 0]);
        fixture.no_uplink();
        fixture.finish();
    }

    #[test]
    fn sleep_while_dictating_sends_stop_on_the_edge() {
        let mut fixture = Fixture::new();
        let doap = fixture.device.pods.doap();
        doap.hold_stop();
        fixture.list(CAPTURE);
        let recorder = record(&fixture.rig, &fixture.server, "dictation.wav");
        fixture.streaming(1);
        sleep(&mut fixture.rig);
        wait_for("sleep STOP", || (doap.stops() == [HOST_STOP]).then_some(()));
        status(&fixture.rig, "stopping");
        doap.release_stop();
        fixture.stopped(1);
        fixture.rig.wait_logged("dictation: stop sent (sleep)");
        assert_eq!(fixture.rig.logind().method_calls(), [""; 0]);
        assert_no_sink_call(&fixture.rig, &fixture.audio);
        assert_eq!(fixture.device.pods.doap().stops(), [HOST_STOP]);
        lose(&fixture.rig, &fixture.device);
        drop(recorder.finish());
        fixture.no_uplink();
        fixture.finish();
    }
}
