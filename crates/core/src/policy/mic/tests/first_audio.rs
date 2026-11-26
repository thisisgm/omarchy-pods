//! How first uplink audio confirms a start or reaches the stop gate.

use super::*;

#[test]
fn audio_after_the_late_start_window_reaches_the_stop_gate() {
    const OBSERVED_LATE_AUDIO: Duration = Duration::from_millis(5989);
    let state = linked(Column::Auto);
    let mut mic = Mic::default();
    let id = OpenId::default();
    drop(mic.step(&state, &Fact::Changed, SENT));
    drop(mic.step(&state, &Fact::Mic(MicFact::Up(id)), SENT));
    let recorder = Fact::Mic(MicFact::Recording(id, true));
    assert_eq!(requests(&mic.step(&state, &recorder, SENT)), START);
    let left = Fact::Mic(MicFact::Recording(id, false));
    assert_eq!(requests(&mic.step(&state, &left, SENT + SOON)), NOTHING);
    let answer = SENT + Duration::from_millis(START_ANSWER_MS);
    assert_eq!(requests(&mic.step(&state, &Fact::Tick, answer)), NOTHING);
    assert_eq!(kind(mic.phase), Kind::Stopped);

    let now = SENT + OBSERVED_LATE_AUDIO;
    let quiet = Fact::Audio(AudioFact::Sink(None));
    let mut quiet_mic = mic;
    drop(quiet_mic.step(&state, &quiet, now));
    let first = Fact::Mic(MicFact::FirstAudio);
    let effects = quiet_mic.step(&state, &first, now);
    assert_eq!(requests(&effects), STOP, "{effects:?}");

    let effects = mic.step(&state, &first, now);
    assert!(lines(&effects).contains(&STOP_WAITS_LINE));
    assert!(!mic.ready_for_dictation(MicDictation::Armed));
    assert_eq!(kind(mic.phase), Kind::Waiting);
    assert_eq!(requests(&mic.step(&state, &quiet, now)), STOP);
}

#[test]
fn first_audio_confirms_an_uplink_start_without_a_marker() {
    let state = linked(Column::AlwaysKnown);
    let mut mic = Mic::default();
    assert_eq!(requests(&mic.step(&state, &Fact::Changed, SENT)), START);
    drop(mic.step(&state, &Fact::Mic(MicFact::FirstAudio), SENT + SOON));
    assert_eq!(kind(mic.phase), Kind::Running);
    let answer = SENT + Duration::from_millis(START_ANSWER_MS);
    assert_eq!(requests(&mic.step(&state, &Fact::Tick, answer)), NOTHING);
    assert_eq!(mic.start_failures, 0);
}

#[test]
fn late_audio_cancels_a_pending_restore() {
    let state = linked(Column::Auto);
    let mut mic = mic(phase(Kind::Restoring));
    mic.armed = Some(SENT);
    let effects = mic.step(&state, &Fact::Mic(MicFact::FirstAudio), SENT + SOON);
    assert!(effects.contains(&Effect::Audio(AudioRequest::CancelRestore)));
    assert_eq!(kind(mic.phase), Kind::Waiting);
    assert!(!mic.ready_for_dictation(MicDictation::Armed));
}
