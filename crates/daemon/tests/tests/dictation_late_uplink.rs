//! First uplink audio arrives after a START has already timed out during fallback.

use std::time::Duration;

use pods_proto::doap::doap_mode_off;
use pods_pwrig::Recorder;
use pulseaudio::protocol::SinkState;

use super::super::support::audio::RecorderId;
use super::super::support::dictation::{CAPTURE, Fixture};
use super::super::support::recording::{frames_at, marked, wait_packets};
use super::super::support::settings::set;
use super::super::support::uplink::{
    all_tries, assert_no_sink_call, mic, move_sink, read_up_to_now, start_failures, starts, stops,
    wait_mic, wait_starts, wait_stop,
};
use super::super::support::{HANG_LIMIT, wait_for};

fn fallback(fixture: &Fixture) -> (Recorder<'_>, RecorderId) {
    fixture.device.pods.doap().refuse_starts(2);
    fixture.device.pods.uplink().hold_starts(true);
    let listed = fixture.list(CAPTURE);
    let recorder = fixture.capture();
    let first = &wait_starts(&fixture.device, 1)[0];
    let doap = fixture.device.pods.doap();
    assert_eq!(doap.starts(), 2);
    assert!(!doap.mode_seen() && !doap.streaming());
    assert!(
        fixture
            .device
            .pods
            .heard()
            .iter()
            .any(|heard| { heard.sdu == doap_mode_off() && heard.at <= first.at })
    );
    (recorder, listed)
}

fn release_record_and_stop(
    fixture: &Fixture,
    recorder: Recorder<'_>,
    listed: RecorderId,
    expected_starts: usize,
) {
    const MARKED_PACKETS: u32 = 20;
    const CAPTURE_SPAN: Duration = Duration::from_millis(400);
    let uplink = fixture.device.pods.uplink();
    assert_eq!(uplink.sent_packets(), 0, "audio before the late answer");
    uplink.hold_running_markers();
    uplink.mark(MARKED_PACKETS);
    uplink.release_start();
    wait_packets(&uplink, 1);
    read_up_to_now(&fixture.rig, &fixture.device);
    let frames = usize::try_from(recorder.frames_on_disk()).unwrap();
    recorder.wait_frames(
        frames.saturating_add(frames_at(CAPTURE_SPAN, recorder.rate())),
        HANG_LIMIT,
    );
    let recording = mic(&fixture.rig);
    assert!(stops(&fixture.device).is_empty(), "STOP while recording");
    let samples = recorder.finish();
    fixture.audio.remove_recorder(listed);
    assert!(!marked(&samples).is_empty(), "missing late uplink audio");
    assert_eq!(recording, "recording", "mic after the markerless answer");
    fixture.audio.hold_replies(false);

    // Auto normally lingers after a recording. Switching off owes a STOP through the playback gate.
    wait_mic(&fixture.rig, "live");
    set(fixture.rig.session(), "microphone", "off");
    wait_mic(&fixture.rig, "stopping");
    assert!(stops(&fixture.device).is_empty(), "STOP during playback");
    move_sink(
        &fixture.rig,
        &fixture.audio,
        &fixture.device,
        SinkState::Suspended,
    );
    wait_stop(&fixture.device);
    wait_for("the fake uplink to stop", || {
        (!uplink.running()).then_some(())
    });
    assert_eq!(stops(&fixture.device).len(), 1);
    assert_eq!(starts(&fixture.device).len(), expected_starts);
    assert_no_sink_call(&fixture.rig, &fixture.audio);
}

#[test]
fn late_uplink_audio_during_starting_confirms_the_retry() {
    let fixture = Fixture::new();
    let (recorder, listed) = fallback(&fixture);
    wait_starts(&fixture.device, 2);
    fixture.rig.wait_status("the retry starting", |status| {
        status["mic"] == "starting" && status["mic_start_failures"] == 1
    });
    release_record_and_stop(&fixture, recorder, listed, 2);
    fixture.finish();
}

#[test]
fn late_uplink_audio_after_stopped_revives_the_recording() {
    let fixture = Fixture::new();
    let (recorder, listed) = fallback(&fixture);
    wait_mic(&fixture.rig, "failed");
    assert_eq!(
        start_failures(&fixture.rig),
        u64::try_from(all_tries()).unwrap()
    );
    release_record_and_stop(&fixture, recorder, listed, all_tries());
    fixture.finish();
}

#[test]
fn late_uplink_audio_during_restoring_cancels_the_restore() {
    let fixture = Fixture::new();
    let (recorder, listed) = fallback(&fixture);
    wait_starts(&fixture.device, all_tries());
    let uplink = fixture.device.pods.uplink();

    // The first held START answers with markers alone. The second has already timed out.
    uplink.stall(HANG_LIMIT);
    uplink.release_start();
    wait_mic(&fixture.rig, "recording");
    fixture.audio.hold_replies(true);
    uplink.stop_unasked();
    wait_mic(&fixture.rig, "stopping");
    uplink.stall(Duration::ZERO);

    release_record_and_stop(&fixture, recorder, listed, all_tries());
    fixture.finish();
}
