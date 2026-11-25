//! Volume decisions when audio events arrive during the pre-write refresh.

use super::{
    Asked, ENDED, LAPTOP_MIC, LOWERED, Recorder, SAVED, SPEAKERS, STARTED, assert_writes, finish,
    hold_duck_server_info, ready, refresh_sink_with_events_silenced, speak, wait_for, wait_volume,
};

#[test]
fn the_duck_refresh_rechecks_the_default_sink() {
    let (rig, device, audio) = ready(SAVED);
    audio.silence_events();
    audio.add_sink(SPEAKERS, SAVED);
    audio.set_default_sink(SPEAKERS);
    let before = audio.requests().len();
    speak(&rig, &device, STARTED);
    wait_for("the duck's pre-write sink refresh", || {
        audio.requests()[before..]
            .contains(&Asked::SinkList)
            .then_some(())
    });
    refresh_sink_with_events_silenced(&rig, &audio);
    finish(rig, &audio);
    assert_writes(&audio, &device, &[]);
}

#[test]
fn a_default_sink_switch_during_the_duck_refresh_lowers_nothing() {
    let (rig, device, audio) = ready(SAVED);
    audio.add_sink(SPEAKERS, SAVED);
    rig.wait_sink_told();
    let before = hold_duck_server_info(&rig, &device, &audio);
    audio.set_default_sink(SPEAKERS);
    audio.hold_replies(false);
    wait_for(
        "a second server query after the default sink changed",
        || {
            (audio.requests()[before..]
                .iter()
                .filter(|asked| **asked == Asked::ServerInfo)
                .count()
                >= 2)
                .then_some(())
        },
    );
    rig.wait_sink_told();
    finish(rig, &audio);
    assert_writes(&audio, &device, &[]);
}

#[test]
fn a_server_event_without_a_default_sink_change_defers_ducking() {
    let (rig, device, audio) = ready(SAVED);
    let before = hold_duck_server_info(&rig, &device, &audio);
    audio.announce_server_change();
    audio.hold_replies(false);

    wait_volume(&audio, &device, LOWERED);
    let requests = &audio.requests()[before..];
    assert!(
        requests
            .iter()
            .take_while(|asked| !matches!(asked, Asked::Volume { .. }))
            .filter(|asked| **asked == Asked::ServerInfo)
            .nth(1)
            .is_some(),
        "expected a second server read before the first volume write: {requests:?}"
    );
    speak(&rig, &device, ENDED);
    wait_volume(&audio, &device, SAVED);
    finish(rig, &audio);
    assert_writes(&audio, &device, &[LOWERED, SAVED]);
}

#[test]
fn recording_starting_during_the_duck_refresh_lowers_nothing() {
    let (rig, device, audio) = ready(SAVED);
    let before = hold_duck_server_info(&rig, &device, &audio);
    // The capture events follow the held server reply and precede the sink-list reply.
    audio.add_source(LAPTOP_MIC);
    audio.add_recorder(LAPTOP_MIC, Recorder::Program);
    audio.hold_replies(false);
    wait_for("the recorder list after capture starts", || {
        audio.requests()[before..]
            .contains(&Asked::RecorderList)
            .then_some(())
    });
    rig.wait_sink_told();
    finish(rig, &audio);
    assert_writes(&audio, &device, &[]);
}

#[test]
fn a_capture_event_without_recording_defers_ducking() {
    let (rig, device, audio) = ready(SAVED);
    let before = hold_duck_server_info(&rig, &device, &audio);
    audio.add_source(LAPTOP_MIC);
    audio.hold_replies(false);

    wait_volume(&audio, &device, LOWERED);
    let requests = &audio.requests()[before..];
    assert!(
        requests
            .iter()
            .take_while(|asked| !matches!(asked, Asked::Volume { .. }))
            .skip_while(|asked| **asked != Asked::ServerInfo)
            .any(|asked| *asked == Asked::RecorderList),
        "expected a recorder read after the held server read and before the first volume write: {requests:?}"
    );
    speak(&rig, &device, ENDED);
    wait_volume(&audio, &device, SAVED);
    finish(rig, &audio);
    assert_writes(&audio, &device, &[LOWERED, SAVED]);
}
