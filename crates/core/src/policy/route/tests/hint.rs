//! Fast-capture hints and recorder routing while the bearer arms.

use pods_proto::doap::ArmFailure;

use super::*;

#[test]
fn a_reconnect_hint_waits_for_doap_and_dictation_starts_without_uplink() {
    let (mut policy, state, reopened) = hint_on_reconnect();
    for fact in [
        Fact::Dictation(DictationFact::Armed),
        Fact::Mic(MicFact::Up(reopened)),
        Fact::Audio(AudioFact::Recorders(DICTATION)),
        Fact::Mic(MicFact::Recording(reopened, true)),
    ] {
        let effects = policy.step(&state, fact, Duration::ZERO);
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::Uplink(_)))
        );
    }
    assert_eq!(policy.edge.route(), Some(Route::Dictation));
    let settled = Duration::from_millis(MODE_SETTLE_MS);
    let effects = policy.step(&state, Fact::Tick, settled);
    assert!(effects.contains(&Effect::Dictation(DictationRequest::Start)));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Uplink(_)))
    );
}

#[test]
fn a_reconnect_hint_starts_the_uplink_after_arm_failure() {
    let (mut policy, state, _) = hint_on_reconnect();
    let failed = Fact::Dictation(DictationFact::ArmFailed(ArmFailure::NoService));
    let effects = policy.step(&state, failed, Duration::ZERO);
    assert!(effects.contains(&Effect::Uplink(UplinkRequest::Start)));
}

#[test]
fn a_call_recorder_can_start_the_uplink_while_doap_arms() {
    let (mut policy, state, reopened) = hint_on_reconnect();
    for fact in [
        Fact::Mic(MicFact::Up(reopened)),
        Fact::Mic(MicFact::Recording(reopened, true)),
    ] {
        let effects = policy.step(&state, fact, Duration::ZERO);
        assert!(!effects.contains(&Effect::Uplink(UplinkRequest::Start)));
    }
    let effects = policy.step(
        &state,
        Fact::Audio(AudioFact::Recorders(CALL)),
        Duration::ZERO,
    );
    assert_eq!(policy.edge.route(), Some(Route::Uplink));
    assert!(effects.contains(&Effect::Uplink(UplinkRequest::Start)));
}

fn hint_on_reconnect() -> (Policy, State, OpenId) {
    let state = State {
        audio_server: AudioServer::Up,
        ..linked_auto()
    };
    let mut policy = Policy::default();
    let link = Fact::Link(Signal::Up { device: device() });
    let profile = Fact::Audio(AudioFact::Headset(false));
    for fact in [
        link.clone(),
        profile.clone(),
        Fact::Dictation(DictationFact::Armed),
    ] {
        drop(policy.step(&state, fact, Duration::ZERO));
    }
    let disconnected = State {
        connected: false,
        ..State::default()
    };
    drop(policy.step(&disconnected, Fact::Link(Signal::Down), Duration::ZERO));
    let effects = policy.step(&state, link, Duration::ZERO);
    let reopened = effects
        .iter()
        .find_map(|effect| {
            if let Effect::Mic(MicRequest::Open { id, .. }) = effect {
                Some(*id)
            } else {
                None
            }
        })
        .expect("a new mic source on reconnect");
    assert!(!effects.contains(&Effect::Uplink(UplinkRequest::Start)));
    for fact in [profile, Fact::Audio(AudioFact::Hint(true))] {
        let effects = policy.step(&state, fact, Duration::ZERO);
        assert!(
            !effects.contains(&Effect::Uplink(UplinkRequest::Start)),
            "{effects:?}"
        );
    }
    assert_eq!(policy.dictation.code(&state), MicDictation::Arming);
    (policy, state, reopened)
}
