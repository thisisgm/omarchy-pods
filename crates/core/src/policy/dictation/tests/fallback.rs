//! When a `DoAP` START refusal or timeout exhausts the session.

use super::*;

#[test]
fn a_refusal_on_the_last_try_falls_back() {
    use DictationRequest::{Mode, ModeOff, Start};
    use MicDictation::Starting;

    const FIRST_START: u64 = MODE_SETTLE_MS;
    const FIRST_REFUSAL: u64 = FIRST_START + 1;
    const SECOND_START: u64 = FIRST_REFUSAL + MODE_SETTLE_MS;
    const SECOND_REFUSAL: u64 = SECOND_START + 1;

    let refused = DictationFact::Refused {
        reason: pods_proto::doap::STOP_REASON_UNSOLICITED,
    };
    session_table([
        (0, Fact::Tick, None, Some(Starting), Some(Mode), false),
        (
            FIRST_START,
            Fact::Tick,
            None,
            Some(Starting),
            Some(Start),
            false,
        ),
        (
            FIRST_REFUSAL,
            Fact::Dictation(refused.clone()),
            None,
            Some(Starting),
            Some(Mode),
            false,
        ),
        (
            SECOND_START,
            Fact::Tick,
            None,
            Some(Starting),
            Some(Start),
            false,
        ),
        (
            SECOND_REFUSAL,
            Fact::Dictation(refused),
            None,
            None,
            Some(ModeOff),
            true,
        ),
        (
            SECOND_REFUSAL + 1,
            Fact::Tick,
            Some(Stop::Left),
            None,
            None,
            false,
        ),
    ]);
}

#[test]
fn a_timed_out_start_then_a_refusal_on_the_last_try_falls_back() {
    use DictationRequest::{Mode, ModeOff, Start};
    use MicDictation::Starting;

    const FIRST_START: u64 = MODE_SETTLE_MS;
    const FIRST_REFUSAL: u64 = FIRST_START + 1;
    const SECOND_START: u64 = FIRST_REFUSAL + MODE_SETTLE_MS;
    const SECOND_REFUSAL: u64 = SECOND_START + 1;
    const LEFT: u64 = SECOND_REFUSAL + 1;
    const NEW_MODE: u64 = LEFT + 1;
    const NEW_START: u64 = NEW_MODE + MODE_SETTLE_MS;
    const RETRY_MODE: u64 = NEW_START + DOAP_START_ANSWER_MS;
    const RETRY_START: u64 = RETRY_MODE + MODE_SETTLE_MS;

    let refused = Fact::Dictation(DictationFact::Refused {
        reason: pods_proto::doap::STOP_REASON_UNSOLICITED,
    });
    let host_stop = DictationRequest::Stop {
        reason: pods_proto::doap::STOP_REASON_HOST,
    };
    let starting = |at, request| (at, Fact::Tick, None, Some(Starting), vec![request], false);
    let mut session = Session::default();
    for (ms, fact, why, status, wanted, gives_up) in [
        starting(0, Mode),
        starting(FIRST_START, Start),
        (
            FIRST_REFUSAL,
            refused.clone(),
            None,
            Some(Starting),
            vec![Mode],
            false,
        ),
        starting(SECOND_START, Start),
        (
            SECOND_REFUSAL,
            refused.clone(),
            None,
            None,
            vec![ModeOff],
            true,
        ),
        (LEFT, Fact::Tick, Some(Stop::Left), None, vec![], false),
        starting(NEW_MODE, Mode),
        starting(NEW_START, Start),
        (
            RETRY_MODE,
            Fact::Tick,
            None,
            Some(Starting),
            vec![host_stop, Mode],
            false,
        ),
        starting(RETRY_START, Start),
        (RETRY_START + 1, refused, None, None, vec![ModeOff], true),
    ] {
        let mut effects = Vec::new();
        assert_eq!(
            session.step(&fact, Duration::from_millis(ms), why, &mut effects),
            gives_up,
            "at {ms} ms"
        );
        let sent: Vec<_> = effects
            .into_iter()
            .filter_map(|effect| {
                if let Effect::Dictation(request) = effect {
                    Some(request)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(sent, wanted, "at {ms} ms");
        assert_eq!(session.code(), status, "at {ms} ms");
    }
}

#[test]
fn a_timed_out_start_with_a_stop_owed_sends_host_stop_then_mode_off() {
    use DictationRequest::{Mode, ModeOff, Start};
    use MicDictation::{Starting, Stopping};

    const START: u64 = MODE_SETTLE_MS;
    const LEFT: u64 = START + 1;
    const TIMED_OUT: u64 = START + DOAP_START_ANSWER_MS;

    let host_stop = DictationRequest::Stop {
        reason: pods_proto::doap::STOP_REASON_HOST,
    };
    let left = Some(Stop::Left);
    let mut session = Session::default();
    for (ms, why, status, wanted) in [
        (0, None, Some(Starting), vec![Mode]),
        (START, None, Some(Starting), vec![Start]),
        (LEFT, left, Some(Stopping), vec![]),
        (TIMED_OUT - 1, left, Some(Stopping), vec![]),
        (TIMED_OUT, left, None, vec![host_stop, ModeOff]),
    ] {
        let mut effects = Vec::new();
        assert!(
            !session.step(&Fact::Tick, Duration::from_millis(ms), why, &mut effects),
            "at {ms} ms"
        );
        let sent: Vec<_> = effects
            .into_iter()
            .filter_map(|effect| {
                if let Effect::Dictation(request) = effect {
                    Some(request)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(sent, wanted, "at {ms} ms");
        assert_eq!(session.code(), status, "at {ms} ms");
    }
}
