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
        (SECOND_REFUSAL + 1, Fact::Tick, Some(Stop::Left), None, None, false),
    ]);
}

#[test]
fn a_timed_out_start_then_a_refusal_on_the_last_try_falls_back() {
    use DictationRequest::{Mode, ModeOff, Start};
    use MicDictation::Starting;

    let refused = Fact::Dictation(DictationFact::Refused {
        reason: pods_proto::doap::STOP_REASON_UNSOLICITED,
    });
    let host_stop = DictationRequest::Stop {
        reason: pods_proto::doap::STOP_REASON_HOST,
    };
    let mut session = Session::default();
    for (ms, fact, why, status, wanted, gives_up) in [
        (1506, Fact::Tick, None, Some(Starting), vec![Mode], false),
        (1656, Fact::Tick, None, Some(Starting), vec![Start], false),
        (
            1657,
            refused.clone(),
            None,
            Some(Starting),
            vec![Mode],
            false,
        ),
        (1807, Fact::Tick, None, Some(Starting), vec![Start], false),
        (1808, refused.clone(), None, None, vec![ModeOff], true),
        (1809, Fact::Tick, Some(Stop::Left), None, vec![], false),
        (1810, Fact::Tick, None, Some(Starting), vec![Mode], false),
        (1960, Fact::Tick, None, Some(Starting), vec![Start], false),
        (
            2460,
            Fact::Tick,
            None,
            Some(Starting),
            vec![host_stop, Mode],
            false,
        ),
        (2610, Fact::Tick, None, Some(Starting), vec![Start], false),
        (2611, refused, None, None, vec![ModeOff], true),
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
