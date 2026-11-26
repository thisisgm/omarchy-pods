//! Route rows for a lost `DoAP` bearer: before the start, outside a dictation route, and after a spent session.

use super::*;
use crate::policy::dictation::DOAP_START_TRIES;

#[test]
fn a_lost_bearer_before_the_doap_start_takes_the_uplink() {
    let (mut policy, mut state) = waiting_for_recorders(DICTATION, Route::Dictation);
    state.audio_server = AudioServer::Up;
    let start = Effect::Uplink(UplinkRequest::Start);
    let profile = Fact::Audio(AudioFact::Headset(false));
    drop(policy.step(&state, profile, Duration::ZERO));
    let recorders = Fact::Audio(AudioFact::Recorders(DICTATION));
    let effects = policy.step(&state, recorders, Duration::ZERO);
    assert!(effects.contains(&Effect::Dictation(DictationRequest::Mode)));
    assert!(!effects.contains(&Effect::Dictation(DictationRequest::Start)));
    assert!(policy.dictation_session_open());

    let lost = Fact::Dictation(DictationFact::BearerLost);
    let effects = policy.step(&state, lost, Duration::ZERO);
    let mode_off = Effect::Dictation(DictationRequest::ModeOff);
    assert!(effects.contains(&mode_off));
    assert!(!effects.contains(&start));
    let settled = Duration::from_millis(MODE_SETTLE_MS);
    assert!(policy.step(&state, Fact::Tick, settled).contains(&start));
    assert!(!policy.dictation_session_open());
    assert_eq!(policy.edge.route(), Some(Route::Uplink));
}

#[test]
fn a_lost_bearer_outside_a_dictation_route_is_no_fallback() {
    let fallback = Effect::Log("dictation: this recording takes the uplink".to_owned());
    let (mut policy, state) = waiting_for_recorders(CALL, Route::Uplink);
    assert_eq!(policy.edge.route(), None);
    let lost = Fact::Dictation(DictationFact::BearerLost);
    let effects = policy.step(&state, lost, Duration::ZERO);
    assert!(!effects.contains(&fallback));
    assert_eq!(policy.edge.route(), None);

    drop(policy.step(&state, Fact::Tick, Duration::from_millis(ROUTE_WAIT_MS)));
    assert_eq!(policy.edge.route(), Some(Route::Uplink));
    let at = Duration::from_millis(ROUTE_WAIT_MS);
    drop(policy.step(&state, Fact::Dictation(DictationFact::Armed), at));
    let lost = Fact::Dictation(DictationFact::BearerLost);
    let effects = policy.step(&state, lost, at);
    assert!(!effects.contains(&fallback));
    assert_eq!(policy.edge.route(), Some(Route::Uplink));
}

#[test]
fn a_spent_session_takes_the_uplink_once_and_a_later_lost_bearer_does_not_repeat_it() {
    let fallback = Effect::Log("dictation: this recording takes the uplink".to_owned());
    let (mut policy, mut state) = waiting_for_recorders(DICTATION, Route::Dictation);
    state.audio_server = AudioServer::Up;
    let profile = Fact::Audio(AudioFact::Headset(false));
    drop(policy.step(&state, profile, Duration::ZERO));
    let recorders = Fact::Audio(AudioFact::Recorders(DICTATION));
    drop(policy.step(&state, recorders, Duration::ZERO));
    let settle = Duration::from_millis(MODE_SETTLE_MS);
    let mut at = Duration::ZERO;
    let mut fallbacks = 0;
    for _ in 0..DOAP_START_TRIES {
        assert_eq!(fallbacks, 0);
        assert_eq!(policy.edge.route(), Some(Route::Dictation));
        at = at.saturating_add(settle);
        let effects = policy.step(&state, Fact::Tick, at);
        assert!(effects.contains(&Effect::Dictation(DictationRequest::Start)));
        fallbacks += effects.iter().filter(|effect| **effect == fallback).count();
        for fact in [DictationFact::Answered, DictationFact::Ended] {
            let effects = policy.step(&state, Fact::Dictation(fact), at);
            fallbacks += effects.iter().filter(|effect| **effect == fallback).count();
        }
    }
    assert!(!policy.dictation_session_open());
    assert_eq!(fallbacks, 1);
    assert_eq!(policy.edge.route(), Some(Route::Uplink));

    let lost = Fact::Dictation(DictationFact::BearerLost);
    let effects = policy.step(&state, lost, at);
    assert!(!effects.contains(&fallback));
    assert_eq!(policy.edge.route(), Some(Route::Uplink));
}
