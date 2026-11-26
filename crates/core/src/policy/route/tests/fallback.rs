//! Route transitions from `DoAP` to the uplink after `ModeOff`.

use super::*;
use crate::policy::dictation::BEARER_RETRY_MS;
use crate::state::MicCode;

#[test]
fn always_cannot_start_the_uplink_during_a_doap_session() {
    let (mut policy, mut state) = waiting_for_recorders(DICTATION, Route::Dictation);
    let mut effects = policy.step(
        &state,
        Fact::Audio(AudioFact::Recorders(DICTATION)),
        Duration::ZERO,
    );
    assert!(effects.contains(&Effect::Dictation(DictationRequest::Mode)));
    state.microphone.set_preference(MicPref::Always);
    let start = Effect::Uplink(UplinkRequest::Start);
    assert!(
        !policy
            .step(&state, Fact::Changed, Duration::ZERO)
            .contains(&start)
    );
    let settled = Duration::from_millis(MODE_SETTLE_MS);
    effects = policy.step(&state, Fact::Tick, settled);
    assert!(effects.contains(&Effect::Dictation(DictationRequest::Start)));
    assert!(!effects.contains(&start));
    effects = policy.step(&state, Fact::Dictation(DictationFact::Answered), settled);
    assert!(!effects.contains(&start));

    let empty = Fact::Audio(AudioFact::Recorders(Recorders::default()));
    effects = policy.step(&state, empty, settled);
    assert!(effects.contains(&Effect::Dictation(DictationRequest::Stop {
        reason: STOP_REASON_HOST,
    })));
    assert!(!effects.contains(&start));
    effects = policy.step(&state, Fact::Dictation(DictationFact::Ended), settled);
    assert!(effects.contains(&Effect::Dictation(DictationRequest::ModeOff)));
    assert!(!effects.contains(&start));
    assert!(
        policy
            .step(&state, Fact::Tick, settled * 2)
            .contains(&start)
    );
}

#[test]
fn an_unanswered_doap_stop_releases_the_uplink_for_always() {
    use crate::policy::dictation::DOAP_STOP_ANSWER_MS;

    let (mut policy, mut state) = waiting_for_recorders(DICTATION, Route::Dictation);
    let list = Fact::Audio(AudioFact::Recorders(DICTATION));
    drop(policy.step(&state, list, Duration::ZERO));
    state.microphone.set_preference(MicPref::Always);
    drop(policy.step(&state, Fact::Changed, Duration::ZERO));
    let settled = Duration::from_millis(MODE_SETTLE_MS);
    drop(policy.step(&state, Fact::Tick, settled));
    drop(policy.step(&state, Fact::Dictation(DictationFact::Answered), settled));

    let start = Effect::Uplink(UplinkRequest::Start);
    let empty = Fact::Audio(AudioFact::Recorders(Recorders::default()));
    assert!(!policy.step(&state, empty, settled).contains(&start));
    let cleared = settled + Duration::from_millis(DOAP_STOP_ANSWER_MS);
    let early = cleared.saturating_sub(Duration::from_millis(1));
    assert!(!policy.step(&state, Fact::Tick, early).contains(&start));
    let effects = policy.step(&state, Fact::Tick, cleared);
    assert!(effects.contains(&Effect::Dictation(DictationRequest::ModeOff)));
    assert!(!effects.contains(&start));
    let effects = policy.step(&state, Fact::Tick, cleared + settled);
    assert!(
        effects.contains(&start),
        "the silent STOP clear must release 0x58: {effects:?}"
    );
}

#[test]
fn a_timed_out_doap_start_sends_host_stop_before_retry() {
    use crate::policy::dictation::DOAP_START_ANSWER_MS;

    let (mut policy, state) = waiting_for_recorders(DICTATION, Route::Dictation);
    let list = Fact::Audio(AudioFact::Recorders(DICTATION));
    drop(policy.step(&state, list, Duration::ZERO));
    let settle = Duration::from_millis(MODE_SETTLE_MS);
    let answer = Duration::from_millis(DOAP_START_ANSWER_MS);
    let effects = policy.step(&state, Fact::Tick, settle);
    assert!(effects.contains(&Effect::Dictation(DictationRequest::Start)));
    let effects = policy.step(&state, Fact::Tick, settle + answer);
    let stop = Effect::Dictation(DictationRequest::Stop {
        reason: STOP_REASON_HOST,
    });
    let mode = Effect::Dictation(DictationRequest::Mode);
    let stop_at = effects.iter().position(|effect| *effect == stop);
    let mode_at = effects.iter().position(|effect| *effect == mode);
    assert!(
        matches!((stop_at, mode_at), (Some(stop), Some(mode)) if stop < mode),
        "{effects:?}"
    );
}

#[test]
fn a_refusal_after_timeout_waits_for_mode_off_to_settle() {
    use crate::policy::dictation::DOAP_START_ANSWER_MS;
    use pods_proto::doap::STOP_REASON_UNSOLICITED;

    let (mut policy, state) = waiting_for_recorders(DICTATION, Route::Dictation);
    let list = Fact::Audio(AudioFact::Recorders(DICTATION));
    drop(policy.step(&state, list, Duration::ZERO));
    let settle = Duration::from_millis(MODE_SETTLE_MS);
    let retry = settle + Duration::from_millis(DOAP_START_ANSWER_MS);
    for at in [settle, retry, retry + settle] {
        drop(policy.step(&state, Fact::Tick, at));
    }

    let refused = Fact::Dictation(DictationFact::Refused {
        reason: STOP_REASON_UNSOLICITED,
    });
    let cleared = retry + settle;
    let effects = policy.step(&state, refused, cleared);
    let start = Effect::Uplink(UplinkRequest::Start);
    assert!(effects.contains(&Effect::Dictation(DictationRequest::ModeOff)));
    assert!(!effects.contains(&start), "{effects:?}");
    assert_eq!(policy.deadline(), Some(cleared + settle));
    let early = cleared + Duration::from_millis(MODE_SETTLE_MS - 1);
    assert!(!policy.step(&state, Fact::Tick, early).contains(&start));
    assert!(
        policy
            .step(&state, Fact::Tick, cleared + settle)
            .contains(&start)
    );
}

#[test]
fn an_unanswered_doap_start_falls_back_for_the_span() {
    use crate::policy::dictation::DOAP_START_ANSWER_MS;

    let (mut policy, state) = waiting_for_recorders(DICTATION, Route::Dictation);
    let list = Fact::Audio(AudioFact::Recorders(DICTATION));
    drop(policy.step(&state, list, Duration::ZERO));
    let settle = Duration::from_millis(MODE_SETTLE_MS);
    let answer = Duration::from_millis(DOAP_START_ANSWER_MS);
    let start = Effect::Dictation(DictationRequest::Start);
    let uplink = Effect::Uplink(UplinkRequest::Start);
    for (at, request) in [
        (settle, DictationRequest::Start),
        (settle + answer, DictationRequest::Mode),
        (settle + answer + settle, DictationRequest::Start),
    ] {
        let effects = policy.step(&state, Fact::Tick, at);
        assert!(effects.contains(&Effect::Dictation(request)));
        assert!(!effects.contains(&uplink));
    }
    let spent = 2 * (settle + answer);
    let effects = policy.step(&state, Fact::Tick, spent);
    let mode_off = Effect::Dictation(DictationRequest::ModeOff);
    let stop = Effect::Dictation(DictationRequest::Stop {
        reason: STOP_REASON_HOST,
    });
    let stop_at = effects.iter().position(|effect| *effect == stop);
    let off_at = effects.iter().position(|effect| *effect == mode_off);
    assert!(
        matches!((stop_at, off_at), (Some(stop), Some(off)) if stop < off),
        "{effects:?}"
    );
    assert!(!effects.contains(&uplink));
    let fallback = Effect::Log("dictation: this recording takes the uplink".to_owned());
    assert_eq!(
        effects.iter().filter(|effect| **effect == fallback).count(),
        1
    );
    assert_eq!(policy.edge.route(), Some(Route::Uplink));
    let ready = spent + settle;
    let effects = policy.step(&state, Fact::Tick, ready);
    assert!(
        effects.contains(&uplink),
        "terminal failure must start 0x58: {effects:?}"
    );
    let effects = policy.step(&state, Fact::Audio(AudioFact::Recorders(DICTATION)), ready);
    assert!(!effects.contains(&start) && !effects.contains(&uplink));
    assert!(!effects.contains(&fallback));
    assert_eq!(policy.edge.route(), Some(Route::Uplink));
}

#[test]
fn a_doap_stream_ending_on_the_last_try_falls_back_for_the_span() {
    use crate::policy::dictation::{DOAP_START_ANSWER_MS, DOAP_START_TRIES};

    let (mut policy, state) = waiting_for_recorders(DICTATION, Route::Dictation);
    let list = Fact::Audio(AudioFact::Recorders(DICTATION));
    let mut effects = policy.step(&state, list.clone(), Duration::ZERO);
    let mode = Effect::Dictation(DictationRequest::Mode);
    let start = Effect::Dictation(DictationRequest::Start);
    let settle = Duration::from_millis(MODE_SETTLE_MS);
    let answer = Duration::from_millis(DOAP_START_ANSWER_MS);
    let mut now = Duration::ZERO;
    for attempt in 1..=DOAP_START_TRIES {
        assert!(effects.contains(&mode));
        now += settle;
        effects = policy.step(&state, Fact::Tick, now);
        assert!(effects.contains(&start));
        if attempt < DOAP_START_TRIES {
            now += answer;
            effects = policy.step(&state, Fact::Tick, now);
        }
    }
    let answered = Fact::Dictation(DictationFact::Answered);
    drop(policy.step(&state, answered, now));
    assert_eq!(policy.dictation.code(&state), MicDictation::Streaming);
    assert_eq!(policy.edge.route(), Some(Route::Dictation));
    assert!(policy.mic_recording());

    let ended = Fact::Dictation(DictationFact::Ended);
    let effects = policy.step(&state, ended, now);
    let fallback = Effect::Log("dictation: this recording takes the uplink".to_owned());
    assert!(effects.contains(&Effect::Dictation(DictationRequest::ModeOff)));
    assert_eq!(policy.edge.route(), Some(Route::Uplink));
    let uplink = Effect::Uplink(UplinkRequest::Start);
    assert!(!effects.contains(&uplink));
    assert_eq!(
        effects.iter().filter(|effect| **effect == fallback).count(),
        1
    );
    let effects = policy.step(&state, list, now);
    assert!(!effects.contains(&fallback) && !effects.contains(&mode));
    assert_eq!(policy.edge.route(), Some(Route::Uplink));
    assert!(
        policy
            .step(&state, Fact::Tick, now + settle)
            .contains(&uplink)
    );
}

#[test]
fn a_recorder_after_an_empty_list_opens_a_new_span() {
    let (mut policy, state) = streaming_dictation(DICTATION);
    let empty = || Fact::Audio(AudioFact::Recorders(Recorders::default()));
    drop(policy.step(&state, empty(), STREAMING_AT));
    drop(policy.step(&state, empty(), STREAMING_AT));
    let idle = STREAMING_AT + Duration::from_millis(ROUTE_WAIT_MS);
    drop(policy.step(&state, Fact::Tick, idle));
    assert!(!policy.mic_recording());

    let list = Fact::Audio(AudioFact::Recorders(CALL));
    let effects = policy.step(&state, list, idle);
    let route = Effect::Log("mic: route uplink (1 recorders)".to_owned());
    assert!(
        effects.contains(&route),
        "the next recorder must get a route: {effects:?}"
    );
    let start = Effect::Uplink(UplinkRequest::Start);
    assert!(!effects.contains(&start), "DoAP STOP is still pending");
    assert!(!effects.contains(&Effect::ResyncMic));
    assert_eq!(policy.edge.route(), Some(Route::Uplink));
    assert!(policy.mic_recording());

    let ended = Fact::Dictation(DictationFact::Ended);
    let effects = policy.step(&state, ended, idle);
    assert!(effects.contains(&Effect::Dictation(DictationRequest::ModeOff)));
    assert!(!effects.contains(&start));
    let settled = idle + Duration::from_millis(MODE_SETTLE_MS);
    assert!(policy.step(&state, Fact::Tick, settled).contains(&start));
}

#[test]
fn a_lost_bearer_starts_the_uplink_after_the_mode_clears() {
    let (mut policy, mut state) = waiting_for_recorders(DICTATION, Route::Dictation);
    state.audio_server = AudioServer::Up;
    let start = Effect::Uplink(UplinkRequest::Start);
    let settled = Duration::from_millis(MODE_SETTLE_MS);
    for fact in [
        Fact::Audio(AudioFact::Headset(false)),
        Fact::Audio(AudioFact::Recorders(DICTATION)),
    ] {
        assert!(!policy.step(&state, fact, Duration::ZERO).contains(&start));
    }
    let effects = policy.step(&state, Fact::Tick, settled);
    assert!(effects.contains(&Effect::Dictation(DictationRequest::Start)));
    for fact in [DictationFact::Answered, DictationFact::FirstAudio] {
        let effects = policy.step(&state, Fact::Dictation(fact), settled);
        assert!(!effects.contains(&start));
    }
    assert!(policy.dictation_session_open());
    assert!(!policy.mic_accepts_uplink());

    let lost = Fact::Dictation(DictationFact::BearerLost);
    let effects = policy.step(&state, lost, settled);
    assert!(effects.contains(&Effect::Dictation(DictationRequest::ModeOff)));
    assert!(!effects.contains(&start));
    assert!(!policy.dictation.armed());
    assert!(!policy.dictation_session_open());
    assert_eq!(policy.edge.route(), Some(Route::Uplink));
    assert!(policy.mic_recording());
    assert!(policy.mic_accepts_uplink());

    let ready = settled * 2;
    assert!(policy.step(&state, Fact::Tick, ready).contains(&start));
    let running = Fact::Link(Signal::Uplink(UplinkState::Running));
    drop(policy.step(&state, running, ready));
    let effects = policy.step(&state, Fact::Mic(MicFact::FirstAudio), ready);
    assert!(effects.contains(&Effect::Log("mic: first audio after 0 ms".to_owned())));
    policy.show_mic(&mut state);
    assert_eq!(state.microphone.code(), MicCode::Recording);

    let retry = settled.saturating_add(Duration::from_millis(BEARER_RETRY_MS[0]));
    let effects = policy.step(&state, Fact::Tick, retry);
    assert!(effects.contains(&Effect::Dictation(DictationRequest::Arm)));
    let effects = policy.step(&state, Fact::Dictation(DictationFact::Armed), retry);
    assert_eq!(policy.edge.route(), Some(Route::Uplink));
    assert!(policy.mic_accepts_uplink());
    assert!(!policy.dictation_session_open());
    assert!(!effects.contains(&Effect::Dictation(DictationRequest::Mode)));
}

fn waiting_for_fallback_settle() -> (Policy, State, Duration) {
    use crate::policy::dictation::DOAP_START_TRIES;
    use pods_proto::doap::STOP_REASON_UNSOLICITED;

    let (mut policy, state) = waiting_for_recorders(DICTATION, Route::Dictation);
    let settle = Duration::from_millis(MODE_SETTLE_MS);
    let mut effects = policy.step(
        &state,
        Fact::Audio(AudioFact::Recorders(DICTATION)),
        Duration::ZERO,
    );
    let mut cleared = Duration::ZERO;
    let uplink = Effect::Uplink(UplinkRequest::Start);
    for _ in 0..DOAP_START_TRIES {
        assert!(effects.contains(&Effect::Dictation(DictationRequest::Mode)));
        assert!(!effects.contains(&uplink));
        cleared = cleared.saturating_add(settle);
        let effects_at_start = policy.step(&state, Fact::Tick, cleared);
        assert!(effects_at_start.contains(&Effect::Dictation(DictationRequest::Start)));
        assert!(!effects_at_start.contains(&uplink));
        let refused = Fact::Dictation(DictationFact::Refused {
            reason: STOP_REASON_UNSOLICITED,
        });
        effects = policy.step(&state, refused, cleared);
    }
    assert!(effects.contains(&Effect::Dictation(DictationRequest::ModeOff)));
    assert!(!effects.contains(&uplink));
    assert_eq!(policy.edge.route(), Some(Route::Uplink));
    assert_eq!(policy.deadline(), Some(cleared.saturating_add(settle)));
    (policy, state, cleared)
}

#[test]
fn switching_the_mic_off_during_mode_off_settle_cancels_fallback() {
    let (mut policy, mut state, cleared) = waiting_for_fallback_settle();
    let settle = Duration::from_millis(MODE_SETTLE_MS);
    let uplink = Effect::Uplink(UplinkRequest::Start);
    state.microphone.set_preference(MicPref::Off);
    let effects = policy.step(&state, Fact::Changed, cleared + settle / 2);
    assert!(!effects.contains(&uplink), "{effects:?}");

    for at in [cleared + settle, cleared + settle * 2] {
        let effects = policy.step(&state, Fact::Tick, at);
        assert!(!effects.contains(&uplink), "{effects:?}");
    }
    assert!(effects.contains(&Effect::Mic(MicRequest::Close)));
    assert!(!policy.mic_recording());
    assert!(!policy.dictation_session_open());
    assert_eq!(policy.edge.route(), None);
    policy.show_mic(&mut state);
    assert_eq!(state.microphone.code(), MicCode::Off);
}

#[test]
fn a_new_dictation_recorder_during_mode_off_settle_keeps_one_route() {
    let (mut policy, mut state, cleared) = waiting_for_fallback_settle();
    let settle = Duration::from_millis(MODE_SETTLE_MS);
    let between = cleared + settle / 2;
    let uplink = Effect::Uplink(UplinkRequest::Start);
    let empty = Fact::Audio(AudioFact::Recorders(Recorders::default()));
    let effects = policy.step(&state, empty, between);
    assert!(!effects.contains(&uplink), "{effects:?}");
    assert_eq!(policy.edge.route(), None);

    let recorder = Fact::Audio(AudioFact::Recorders(DICTATION));
    let effects = policy.step(&state, recorder, between);
    assert!(effects.contains(&Effect::Dictation(DictationRequest::Mode)));
    assert!(!effects.contains(&uplink), "{effects:?}");
    assert_eq!(policy.edge.route(), Some(Route::Dictation));

    let start = Effect::Dictation(DictationRequest::Start);
    let effects = policy.step(&state, Fact::Tick, cleared + settle);
    assert!(
        !effects.contains(&start) && !effects.contains(&uplink),
        "{effects:?}"
    );
    let started = between + settle;
    let effects = policy.step(&state, Fact::Tick, started);
    assert!(effects.contains(&start));
    assert!(!effects.contains(&uplink), "{effects:?}");

    let answered = Fact::Dictation(DictationFact::Answered);
    let effects = policy.step(&state, answered, started);
    assert!(!effects.contains(&uplink), "{effects:?}");
    assert_eq!(policy.edge.route(), Some(Route::Dictation));
    assert!(!policy.mic_accepts_uplink());
    policy.show_mic(&mut state);
    assert_eq!(state.mic_dictation, MicDictation::Streaming);
}
