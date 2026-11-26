//! Conversation Awareness during Superwhisper Muffle release.

use std::time::{Duration, Instant};

use super::super::support::pause;
use super::super::support::rig::REAL_TIME_SLACK;
use super::{
    Asked, DICTATION, Device, ENDED, FakeAudio, LOWERED, Recorder, Rig, SAVED, STARTED,
    assert_writes, finish, node_name, recording_ready, sink_named, speak, wait_volume,
};

const MUFFLE_RELEASE: Duration = Duration::from_millis(1500);
const SPEECH_START: Duration = Duration::from_millis(200);
const SHORT_SPEECH: Duration = Duration::from_millis(500);
const QUIET_AFTER_RELEASE: Duration = Duration::from_millis(500);

fn start_speech_after_unlink(rig: &mut Rig, device: &Device, audio: &FakeAudio) -> Instant {
    let id = audio.add_recorder(&node_name(), Recorder::Named(DICTATION));
    rig.wait_logged("mic: recorders (dictation 1, other 0)");
    assert_writes(audio, device, &[]);

    let unlink = Instant::now();
    audio.remove_recorder(id);
    rig.wait_logged("mic: recorders (dictation 0, other 0)");
    speak(rig, device, STARTED);
    // Allow scheduling slack around SPEECH_START and still deliver it inside MUFFLE_RELEASE.
    assert!(
        unlink.elapsed() <= SPEECH_START.saturating_add(REAL_TIME_SLACK),
        "the daemon processed speech {:?} after recorder unlink",
        unlink.elapsed()
    );
    unlink
}

#[test]
fn post_recording_hold_off_defers_ongoing_speech() {
    let (mut rig, device, audio, _pipewire) = recording_ready();
    let unlink = start_speech_after_unlink(&mut rig, &device, &audio);

    // Timestamp the first write against the earliest possible MUFFLE_RELEASE deadline.
    let writes = audio.wait_writes(1, MUFFLE_RELEASE.saturating_add(REAL_TIME_SLACK));
    let [Asked::Volume { at, .. }] = writes.as_slice() else {
        panic!("expected one sink volume write, got {writes:?}");
    };
    let elapsed = at.saturating_duration_since(unlink);
    assert!(
        elapsed >= MUFFLE_RELEASE,
        "sink volume written {elapsed:?} after recorder unlink, before the {MUFFLE_RELEASE:?} hold-off"
    );
    wait_volume(&audio, &device, LOWERED);
    finish(rig, &audio);
    assert_writes(&audio, &device, &[LOWERED, SAVED]);
}

#[test]
fn post_recording_hold_off_discards_speech_that_ends_early() {
    let (mut rig, device, audio, _pipewire) = recording_ready();
    let unlink = start_speech_after_unlink(&mut rig, &device, &audio);

    // Leave time for an immediate write and allow 300 ms after SHORT_SPEECH within MUFFLE_RELEASE.
    pause(SHORT_SPEECH);
    speak(&rig, &device, ENDED);
    assert!(
        unlink.elapsed() < MUFFLE_RELEASE,
        "the daemon processed speech end {:?} after recorder unlink, outside the {MUFFLE_RELEASE:?} hold-off",
        unlink.elapsed()
    );
    pause(MUFFLE_RELEASE.saturating_add(QUIET_AFTER_RELEASE));
    assert_eq!(audio.volume(&sink_named(device.address)), SAVED);
    finish(rig, &audio);
    assert_writes(&audio, &device, &[]);
}
