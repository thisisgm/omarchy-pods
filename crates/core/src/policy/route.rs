//! Chooses the microphone path once for each span with a recorder on the node.

use std::time::Duration;

use super::audio::Recorders;

pub const DICTATION_RECORDERS: [&str; 2] = ["superwhisper-capture", "superwhisper-history"];
/// The audio server can report its streams just after the node starts running.
pub const ROUTE_WAIT_MS: u64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Dictation,
    Uplink,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) enum Edge {
    #[default]
    None,
    Waiting(Duration),
    Decided(Route),
}

impl Edge {
    pub(super) fn step(
        &mut self,
        recording: bool,
        recorders: Recorders,
        fresh: bool,
        armed: bool,
        now: Duration,
    ) -> Option<Route> {
        if !recording {
            *self = Self::None;
            return None;
        }
        if let Self::Decided(route) = *self {
            return Some(route);
        }
        if !fresh && self.deadline().is_none_or(|at| now < at) {
            if matches!(self, Self::None) {
                *self = Self::Waiting(now);
            }
            return None;
        }
        let route = if recorders.other == 0 && armed {
            Route::Dictation
        } else {
            Route::Uplink
        };
        *self = Self::Decided(route);
        Some(route)
    }

    pub(super) fn deadline(self) -> Option<Duration> {
        match self {
            Self::Waiting(since) => {
                Some(since.saturating_add(Duration::from_millis(ROUTE_WAIT_MS)))
            }
            Self::None | Self::Decided(_) => None,
        }
    }

    pub(super) fn route(self) -> Option<Route> {
        match self {
            Self::Decided(route) => Some(route),
            Self::None | Self::Waiting(_) => None,
        }
    }

    pub(super) fn fallback(&mut self) {
        if matches!(self, Self::Decided(Route::Dictation)) {
            *self = Self::Decided(Route::Uplink);
        }
    }
}

#[cfg(test)]
mod tests {
    use pods_proto::doap::STOP_REASON_HOST;
    use pods_proto::ear::EarReport;
    use pods_proto::uplink::UplinkState;
    use pods_samples::MODEL_NUMBER;

    use super::super::dictation::{BEARER_RETRY_MS, MODE_SETTLE_MS};
    use super::super::tests::device;
    use super::super::{
        AudioFact, DictationFact, DictationRequest, Effect, Fact, MicFact, MicRequest, OpenId,
        Policy, Signal, State, UplinkRequest,
    };
    use super::*;
    use crate::prefs::MicPref;
    use crate::state::{AudioServer, MicCode, MicDictation, Microphone};

    #[test]
    fn edge_cells() {
        use Route::{Dictation, Uplink};
        assert_eq!(
            DICTATION_RECORDERS,
            ["superwhisper-capture", "superwhisper-history"]
        );
        let none = Recorders::default();
        let dictation = Recorders {
            dictation: 1,
            other: 0,
        };
        let mixed = Recorders {
            dictation: 1,
            other: 1,
        };
        let mut edge = Edge::default();
        for (ms, recording, recorders, armed, route, deadline) in [
            (0, false, dictation, true, None, None),
            (1, true, none, true, None, Some(101)),
            (100, true, none, true, None, Some(101)),
            (101, true, none, true, Some(Dictation), None),
            (102, true, mixed, true, Some(Dictation), None),
            (103, true, mixed, false, Some(Dictation), None),
            (104, false, mixed, true, None, None),
            (105, true, mixed, true, Some(Uplink), None),
            (106, true, dictation, true, Some(Uplink), None),
            (107, false, none, true, None, None),
            (108, true, none, false, None, Some(208)),
            (110, true, dictation, false, Some(Uplink), None),
            (111, true, dictation, true, Some(Uplink), None),
        ] {
            assert_eq!(
                edge.step(
                    recording,
                    recorders,
                    recorders != none,
                    armed,
                    Duration::from_millis(ms),
                ),
                route,
                "at {ms} ms"
            );
            assert_eq!(
                edge.deadline(),
                deadline.map(Duration::from_millis),
                "at {ms} ms"
            );
        }
        edge.step(false, none, false, true, Duration::ZERO);
        edge.step(true, dictation, true, true, Duration::ZERO);
        edge.fallback();
        assert_eq!(edge.route(), Some(Uplink));
    }

    const DICTATION: Recorders = Recorders {
        dictation: 1,
        other: 0,
    };
    const CALL: Recorders = Recorders {
        dictation: 0,
        other: 1,
    };
    const STREAMING_AT: Duration = Duration::from_millis(ROUTE_WAIT_MS + MODE_SETTLE_MS);

    fn streaming_dictation(recorders: Recorders) -> (Policy, State) {
        let mut state = linked_auto();
        let mut policy = Policy::default();
        let id = OpenId::default();
        for fact in [
            Fact::Link(Signal::Up { device: device() }),
            Fact::Dictation(DictationFact::Armed),
            Fact::Mic(MicFact::Up(id)),
            Fact::Mic(MicFact::Recording(id, true)),
            Fact::Audio(AudioFact::Recorders(recorders)),
        ] {
            drop(policy.step(&state, fact, Duration::ZERO));
        }
        drop(policy.step(&state, Fact::Tick, Duration::from_millis(ROUTE_WAIT_MS)));
        let effects = policy.step(&state, Fact::Tick, STREAMING_AT);
        assert!(effects.contains(&Effect::Dictation(DictationRequest::Start)));
        let answered = Fact::Dictation(DictationFact::Answered);
        drop(policy.step(&state, answered, STREAMING_AT));
        policy.show_mic(&mut state);
        assert_eq!(state.mic_dictation, MicDictation::Streaming);
        assert_eq!(policy.edge.route(), Some(Route::Dictation));
        (policy, state)
    }

    #[test]
    fn dictation_waits_for_uplink_stop_and_restore() {
        use super::super::audio::{AudioRequest, RestoreId};
        use super::super::mic::STOP_ANSWER_MS;
        use pods_proto::uplink::UplinkState;

        for confirmed in [true, false] {
            let (mut policy, state) = waiting_for_recorders(CALL, Route::Uplink);
            let running = Fact::Link(Signal::Uplink(UplinkState::Running));
            drop(policy.step(&state, running, Duration::ZERO));
            let id = OpenId::default();
            drop(policy.step(
                &state,
                Fact::Mic(MicFact::Recording(id, false)),
                Duration::ZERO,
            ));
            drop(policy.step(
                &state,
                Fact::Mic(MicFact::Recording(id, true)),
                Duration::ZERO,
            ));

            let stop = Effect::Uplink(UplinkRequest::Stop);
            let mode = Effect::Dictation(DictationRequest::Mode);
            let start = Effect::Dictation(DictationRequest::Start);
            let list = Fact::Audio(AudioFact::Recorders(DICTATION));
            let effects = policy.step(&state, list, Duration::ZERO);
            assert!(
                effects.contains(&stop),
                "the handoff must send 0x58 STOP: {effects:?}"
            );
            assert!(!effects.contains(&mode));

            let after = if confirmed {
                Duration::from_millis(STOP_ANSWER_MS / 2)
            } else {
                Duration::from_millis(STOP_ANSWER_MS)
            };
            let before_answer = after.saturating_sub(Duration::from_millis(1));
            let effects = policy.step(&state, Fact::Tick, before_answer);
            assert!(!effects.contains(&mode) && !effects.contains(&start));

            let answer = if confirmed {
                Fact::Link(Signal::Uplink(UplinkState::Stopped))
            } else {
                Fact::Tick
            };
            let effects = policy.step(&state, answer, after);
            let restore = RestoreId(0);
            assert!(effects.contains(&Effect::Audio(AudioRequest::Restore(restore))));
            assert!(!effects.contains(&mode));

            let restored = Fact::Audio(AudioFact::Restored(restore));
            let effects = policy.step(&state, restored, after);
            assert!(effects.contains(&mode));
            assert!(!effects.contains(&start));
            let settled = after + Duration::from_millis(MODE_SETTLE_MS);
            let effects = policy.step(&state, Fact::Tick, settled);
            assert!(effects.contains(&start));
            if !confirmed {
                drop(policy.step(&state, Fact::Dictation(DictationFact::Answered), settled));
                let late = Fact::Link(Signal::Uplink(UplinkState::Stopped));
                let effects = policy.step(&state, late, settled);
                assert_eq!(
                    effects,
                    [Effect::Audio(AudioRequest::Restore(RestoreId(1)))]
                );
                assert_eq!(policy.dictation.code(&state), MicDictation::Streaming);
            }
        }
    }

    #[test]
    fn dictation_starts_after_uplink_stop_on_a_quiet_sink() {
        use super::super::audio::{AudioRequest, SinkState};
        use super::super::tests::sink_in;
        use pods_proto::uplink::UplinkState;

        let (mut policy, state) = waiting_for_recorders(CALL, Route::Uplink);
        let running = Fact::Link(Signal::Uplink(UplinkState::Running));
        drop(policy.step(&state, running, Duration::ZERO));
        drop(policy.step(&state, sink_in(true, SinkState::Suspended), Duration::ZERO));

        let mode = Effect::Dictation(DictationRequest::Mode);
        let list = Fact::Audio(AudioFact::Recorders(DICTATION));
        let effects = policy.step(&state, list, Duration::ZERO);
        assert!(effects.contains(&Effect::Uplink(UplinkRequest::Stop)));
        assert!(!effects.contains(&mode));

        let stopped = Fact::Link(Signal::Uplink(UplinkState::Stopped));
        let effects = policy.step(&state, stopped, Duration::ZERO);
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::Audio(AudioRequest::Restore(_)))),
            "a quiet sink needs no restore: {effects:?}"
        );
        assert!(
            effects.contains(&mode),
            "mode must follow STOP: {effects:?}"
        );
    }

    #[test]
    fn dictation_waits_out_an_unanswered_uplink_start() {
        use super::super::audio::AudioRequest;
        use super::super::mic::START_ANSWER_MS;
        use pods_proto::uplink::UplinkState;

        let (mut policy, state) = waiting_for_recorders(CALL, Route::Uplink);
        let stop = Effect::Uplink(UplinkRequest::Stop);
        let mode = Effect::Dictation(DictationRequest::Mode);
        let list = Fact::Audio(AudioFact::Recorders(DICTATION));
        let effects = policy.step(&state, list, Duration::ZERO);
        assert!(!effects.contains(&stop) && !effects.contains(&mode));

        let window = Duration::from_millis(START_ANSWER_MS);
        let early = window.saturating_sub(Duration::from_millis(1));
        let effects = policy.step(&state, Fact::Tick, early);
        assert!(!effects.contains(&stop) && !effects.contains(&mode));
        let effects = policy.step(&state, Fact::Tick, window);
        assert!(
            effects.contains(&stop),
            "the START window must end in the handoff STOP: {effects:?}"
        );
        assert!(!effects.contains(&mode));

        let stopped = Fact::Link(Signal::Uplink(UplinkState::Stopped));
        let mut effects = policy.step(&state, stopped, window);
        if let Some(Effect::Audio(AudioRequest::Restore(id))) = effects
            .iter()
            .find(|effect| matches!(effect, Effect::Audio(AudioRequest::Restore(_))))
            .cloned()
        {
            assert!(!effects.contains(&mode));
            effects = policy.step(&state, Fact::Audio(AudioFact::Restored(id)), window);
        }
        assert!(
            effects.contains(&mode),
            "mode must follow STOP: {effects:?}"
        );
    }

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
        assert!(effects.contains(&start));
    }

    #[test]
    fn an_unanswered_doap_stop_releases_the_uplink_for_always() {
        use super::super::dictation::DOAP_STOP_ANSWER_MS;

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
        assert!(
            effects.contains(&start),
            "the silent STOP clear must release 0x58: {effects:?}"
        );
    }

    #[test]
    fn an_unanswered_doap_start_falls_back_for_the_span() {
        use super::super::dictation::DOAP_START_ANSWER_MS;

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
        assert!(effects.contains(&mode_off));
        assert!(
            effects.contains(&uplink),
            "terminal failure must start 0x58: {effects:?}"
        );
        assert!(
            effects.iter().position(|effect| *effect == mode_off)
                < effects.iter().position(|effect| *effect == uplink)
        );
        let fallback = Effect::Log("dictation: this recording takes the uplink".to_owned());
        assert_eq!(
            effects.iter().filter(|effect| **effect == fallback).count(),
            1
        );
        assert_eq!(policy.edge.route(), Some(Route::Uplink));
        let effects = policy.step(&state, Fact::Audio(AudioFact::Recorders(DICTATION)), spent);
        assert!(!effects.contains(&start) && !effects.contains(&uplink));
        assert!(!effects.contains(&fallback));
        assert_eq!(policy.edge.route(), Some(Route::Uplink));
    }

    #[test]
    fn a_doap_stream_ending_on_the_last_try_falls_back_for_the_span() {
        use super::super::dictation::{DOAP_START_ANSWER_MS, DOAP_START_TRIES};

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
        assert!(effects.contains(&Effect::Uplink(UplinkRequest::Start)));
        assert_eq!(
            effects.iter().filter(|effect| **effect == fallback).count(),
            1
        );
        let effects = policy.step(&state, list, now);
        assert!(!effects.contains(&fallback) && !effects.contains(&mode));
        assert_eq!(policy.edge.route(), Some(Route::Uplink));
    }

    #[test]
    fn the_recorder_list_emptying_ends_the_dictation_span() {
        let empty = || Fact::Audio(AudioFact::Recorders(Recorders::default()));
        let stop = Effect::Dictation(DictationRequest::Stop {
            reason: STOP_REASON_HOST,
        });

        let (mut policy, mut state) = streaming_dictation(Recorders::default());
        assert!(!policy.step(&state, empty(), STREAMING_AT).contains(&stop));
        let later = STREAMING_AT + Duration::from_secs(1);
        assert!(!policy.step(&state, Fact::Tick, later).contains(&stop));
        policy.show_mic(&mut state);
        assert_eq!(state.mic_dictation, MicDictation::Streaming);
        assert_eq!(policy.edge.route(), Some(Route::Dictation));

        let (mut policy, state) = streaming_dictation(DICTATION);
        let effects = policy.step(&state, empty(), STREAMING_AT);
        assert!(
            effects.contains(&stop),
            "the empty list must send STOP: {effects:?}"
        );
        assert_eq!(policy.edge.route(), None);
        assert!(!policy.mic_recording());
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
        assert!(policy.step(&state, ended, idle).contains(&start));
    }

    #[test]
    fn only_a_reopened_dictation_span_requests_mic_resync() {
        let state = linked_auto();
        let id = OpenId::default();
        let list = || Fact::Audio(AudioFact::Recorders(DICTATION));
        let recording = || Fact::Mic(MicFact::Recording(id, true));
        for (order, facts) in [
            ("list before recording", [list(), recording()]),
            ("recording before list", [recording(), list()]),
        ] {
            let mut policy = Policy::default();
            for fact in [
                Fact::Link(Signal::Up { device: device() }),
                Fact::Dictation(DictationFact::Armed),
                Fact::Mic(MicFact::Up(id)),
            ] {
                drop(policy.step(&state, fact, Duration::ZERO));
            }
            // Late enough that a time gate alone cannot tell the first span from a reopened one.
            for fact in facts {
                let effects = policy.step(&state, fact, STREAMING_AT);
                assert!(
                    !effects.contains(&Effect::ResyncMic),
                    "first dictation, {order}: {effects:?}"
                );
            }
            assert_eq!(policy.edge.route(), Some(Route::Dictation), "{order}");

            let empty = Fact::Audio(AudioFact::Recorders(Recorders::default()));
            let reopen = STREAMING_AT.saturating_mul(2);
            drop(policy.step(&state, empty, reopen));
            assert!(!policy.mic_recording());
            assert_eq!(policy.edge.route(), None);

            let effects = policy.step(&state, list(), reopen);
            assert!(
                effects.contains(&Effect::ResyncMic),
                "reopened dictation, {order}: {effects:?}"
            );
            assert_eq!(policy.edge.route(), Some(Route::Dictation));
        }
    }

    #[test]
    fn fast_capture_starts_the_uplink_only_while_dictation_is_unarmed() {
        let state = State {
            audio_server: AudioServer::Up,
            ..linked_auto()
        };
        let start = Effect::Uplink(UplinkRequest::Start);
        for armed in [false, true] {
            let mut policy = Policy::default();
            let link = Fact::Link(Signal::Up { device: device() });
            drop(policy.step(&state, link, Duration::ZERO));
            if armed {
                let ready = Fact::Dictation(DictationFact::Armed);
                drop(policy.step(&state, ready, Duration::ZERO));
            }
            assert_eq!(policy.dictation.armed(), armed);
            assert!(!policy.mic_recording());
            let profile = Fact::Audio(AudioFact::Headset(false));
            let effects = policy.step(&state, profile, Duration::ZERO);
            assert!(!effects.contains(&start));
            let hint = Fact::Audio(AudioFact::Hint(true));
            let effects = policy.step(&state, hint, Duration::ZERO);
            assert_eq!(effects.contains(&start), !armed, "armed: {armed}");
        }
    }

    #[test]
    fn fast_capture_stays_ignored_while_the_route_is_dictation() {
        let start = Effect::Uplink(UplinkRequest::Start);
        let (mut policy, mut state) = waiting_for_recorders(DICTATION, Route::Dictation);
        state.audio_server = AudioServer::Up;
        for fact in [
            Fact::Audio(AudioFact::Headset(false)),
            Fact::Audio(AudioFact::Recorders(DICTATION)),
            Fact::Audio(AudioFact::Hint(true)),
        ] {
            let effects = policy.step(&state, fact, Duration::ZERO);
            assert!(!effects.contains(&start));
        }
        assert_eq!(policy.edge.route(), Some(Route::Dictation));
    }

    #[test]
    fn a_lost_bearer_starts_the_uplink_in_the_same_step() {
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
        let uplink = effects
            .iter()
            .position(|effect| *effect == start)
            .expect("BearerLost must send 0x58 START in the same policy step");
        let mode_off = Effect::Dictation(DictationRequest::ModeOff);
        assert!(effects[..uplink].contains(&mode_off));
        assert!(!policy.dictation.armed());
        assert!(!policy.dictation_session_open());
        assert_eq!(policy.edge.route(), Some(Route::Uplink));
        assert!(policy.mic_recording());
        assert!(policy.mic_accepts_uplink());

        let running = Fact::Link(Signal::Uplink(UplinkState::Running));
        drop(policy.step(&state, running, settled));
        let effects = policy.step(&state, Fact::Mic(MicFact::FirstAudio), settled);
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

    #[test]
    fn a_headset_profile_prevents_a_due_doap_start() {
        let start = Effect::Dictation(DictationRequest::Start);
        for headset in [false, true] {
            let (mut policy, mut state) = waiting_for_recorders(DICTATION, Route::Dictation);
            state.audio_server = AudioServer::Up;
            let profile = Fact::Audio(AudioFact::Headset(false));
            drop(policy.step(&state, profile, Duration::ZERO));
            let list = Fact::Audio(AudioFact::Recorders(DICTATION));
            let effects = policy.step(&state, list, Duration::ZERO);
            assert!(effects.contains(&Effect::Dictation(DictationRequest::Mode)));
            let profile = Fact::Audio(AudioFact::Headset(headset));
            let effects = policy.step(&state, profile, Duration::from_millis(MODE_SETTLE_MS));
            assert_eq!(effects.contains(&start), !headset, "headset: {headset}");
            if headset {
                assert!(effects.contains(&Effect::Mic(MicRequest::Close)));
                assert!(effects.contains(&Effect::Dictation(DictationRequest::ModeOff)));
                assert_eq!(policy.edge.route(), None);
            }
        }
    }

    #[test]
    fn an_unsolicited_uplink_marker_leaves_a_doap_start_alone() {
        let (mut policy, state) = waiting_for_recorders(DICTATION, Route::Dictation);
        let list = Fact::Audio(AudioFact::Recorders(DICTATION));
        drop(policy.step(&state, list, Duration::ZERO));
        let settled = Duration::from_millis(MODE_SETTLE_MS);
        let effects = policy.step(&state, Fact::Tick, settled);
        assert!(effects.contains(&Effect::Dictation(DictationRequest::Start)));

        let marker_delay = Duration::from_millis(21);
        let at = settled.saturating_add(marker_delay);
        let running = Fact::Link(Signal::Uplink(UplinkState::Running));
        let quiet = Fact::Audio(AudioFact::Sink(None));
        for fact in [running, quiet] {
            let effects = policy.step(&state, fact, at);
            assert!(
                !effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::Uplink(_))),
                "DoAP start produced uplink traffic: {effects:?}"
            );
            assert_eq!(policy.dictation.code(&state), MicDictation::Starting);
        }

        let first_audio = Fact::Dictation(DictationFact::FirstAudio);
        let effects = policy.step(&state, first_audio, at);
        assert!(!effects.iter().any(|effect| matches!(
            effect,
            Effect::Uplink(_) | Effect::Dictation(DictationRequest::Stop { .. })
        )));
        assert_eq!(policy.dictation.code(&state), MicDictation::Streaming);
    }

    #[test]
    fn a_call_after_dictation_waits_for_its_recorder_list() {
        recording_spans(DICTATION, Route::Dictation, CALL, Route::Uplink);
    }

    #[test]
    fn a_dictation_after_a_call_waits_for_its_recorder_list() {
        recording_spans(CALL, Route::Uplink, DICTATION, Route::Dictation);
    }

    #[test]
    fn a_call_resumes_with_its_held_list_at_the_deadline() {
        let (mut policy, state) = waiting_for_recorders(CALL, Route::Uplink);
        let deadline = Duration::from_millis(ROUTE_WAIT_MS);
        let before_deadline = deadline.saturating_sub(Duration::from_millis(1));
        drop(policy.step(&state, Fact::Tick, before_deadline));
        assert_eq!(policy.edge.route(), None);
        drop(policy.step(&state, Fact::Tick, deadline));
        assert_eq!(policy.edge.route(), Some(Route::Uplink));
    }

    #[test]
    fn dictation_resumes_with_its_held_list_at_the_deadline() {
        let (mut policy, state) = waiting_for_recorders(DICTATION, Route::Dictation);
        let effects = policy.step(&state, Fact::Tick, Duration::from_millis(ROUTE_WAIT_MS));
        assert_eq!(policy.edge.route(), Some(Route::Dictation));
        assert!(effects.contains(&Effect::Log(
            "mic: route dictation (1 recorders)".to_owned()
        )));
    }

    #[test]
    fn an_empty_list_keeps_the_wait() {
        let (mut policy, state) = waiting_for_recorders(CALL, Route::Uplink);
        let empty = Fact::Audio(AudioFact::Recorders(Recorders::default()));
        drop(policy.step(&state, empty, Duration::from_millis(1)));
        assert_eq!(policy.edge.route(), None);
        assert_eq!(
            policy.edge.deadline(),
            Some(Duration::from_millis(ROUTE_WAIT_MS))
        );
        drop(policy.step(&state, Fact::Tick, Duration::from_millis(ROUTE_WAIT_MS)));
        assert_eq!(policy.edge.route(), Some(Route::Dictation));
    }

    #[test]
    fn the_removal_list_does_not_decide_the_next_span() {
        let (mut policy, state) = spans(DICTATION, Route::Dictation, &[Recorders::default()]);
        let list = Fact::Audio(AudioFact::Recorders(CALL));
        drop(policy.step(&state, list, Duration::from_millis(1)));
        assert_eq!(policy.edge.route(), Some(Route::Uplink));
    }

    fn recording_spans(
        first: Recorders,
        first_route: Route,
        second: Recorders,
        second_route: Route,
    ) {
        let (mut policy, state) = waiting_for_recorders(first, first_route);
        let list = Fact::Audio(AudioFact::Recorders(second));
        drop(policy.step(&state, list, Duration::ZERO));
        assert_eq!(policy.edge.route(), Some(second_route), "second list");
        assert_eq!(policy.edge.deadline(), None, "second list");
    }

    fn waiting_for_recorders(first: Recorders, first_route: Route) -> (Policy, State) {
        spans(first, first_route, &[])
    }

    fn linked_auto() -> State {
        let mut state = State {
            connected: true,
            model_number: MODEL_NUMBER.to_owned(),
            ear: EarReport::from_in_ear(true, true),
            microphone: Microphone::new(MicPref::Auto),
            ..State::default()
        };
        state.mic_models.confirm(MODEL_NUMBER);
        state
    }

    /// Runs one span on `first`, tells `idle_lists` between the spans, and starts the second span.
    fn spans(first: Recorders, first_route: Route, idle_lists: &[Recorders]) -> (Policy, State) {
        let state = linked_auto();
        let mut policy = Policy::default();
        let id = OpenId::default();
        let listed = |recorders| Fact::Audio(AudioFact::Recorders(recorders));
        let recording = |running| Fact::Mic(MicFact::Recording(id, running));
        for fact in [
            Fact::Link(Signal::Up { device: device() }),
            Fact::Dictation(DictationFact::Armed),
            Fact::Mic(MicFact::Up(id)),
            listed(first),
        ] {
            drop(policy.step(&state, fact, Duration::ZERO));
        }
        let wait = Duration::from_millis(ROUTE_WAIT_MS);
        let mut cells = vec![
            (
                "first recorder starts",
                recording(true),
                Some(first_route),
                None,
            ),
            ("first recorder leaves", recording(false), None, None),
        ];
        for idle in idle_lists {
            cells.push(("a list while idle", listed(*idle), None, None));
        }
        cells.push((
            "second recorder starts before its list",
            recording(true),
            None,
            Some(wait),
        ));
        for (cell, fact, route, deadline) in cells {
            drop(policy.step(&state, fact, Duration::ZERO));
            assert_eq!(policy.edge.route(), route, "{cell}");
            assert_eq!(policy.edge.deadline(), deadline, "{cell}");
        }
        (policy, state)
    }
}
