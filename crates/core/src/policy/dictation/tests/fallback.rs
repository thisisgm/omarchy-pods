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
