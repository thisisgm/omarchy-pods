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
    use pods_proto::ear::EarReport;
    use pods_samples::MODEL_NUMBER;

    use super::super::dictation::MODE_SETTLE_MS;
    use super::super::tests::device;
    use super::super::{
        AudioFact, DictationFact, DictationRequest, Effect, Fact, MicFact, MicRequest, OpenId,
        Policy, Signal, State, UplinkRequest,
    };
    use super::*;
    use crate::prefs::MicPref;
    use crate::state::{AudioServer, Microphone};

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
