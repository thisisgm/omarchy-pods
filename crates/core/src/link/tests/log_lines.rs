//! The link's log lines, read by the hardware tests from the journal, so their text is a contract.

use std::time::Duration;

use crate::link::ladder::Attempt;
use crate::link::{Phase, STATE_POPULATED_LINE, phase_change_line};

#[test]
fn every_phase_has_its_fixed_name() {
    let at = Duration::ZERO;
    let attempt = Attempt::Held(1);
    let names = [
        (Phase::Idle, "idle"),
        (Phase::Opening { attempt }, "opening"),
        (Phase::AwaitHandshakeAck { attempt, until: at }, "handshake"),
        (Phase::AwaitFeaturesAck { attempt, until: at }, "features"),
        (Phase::Up { rerequest: None }, "up"),
        (Phase::Closing { attempt: None }, "closing"),
        (Phase::Backoff { attempt, until: at }, "backoff"),
        (Phase::Parked { until: at }, "parked"),
        (Phase::Grace { until: at }, "grace"),
        (Phase::Stopped, "stopped"),
    ];
    for (phase, name) in names {
        assert_eq!(phase.name(), name, "{phase:?}");
    }
}

#[test]
fn both_lines_keep_their_exact_text() {
    let opening = Phase::Opening {
        attempt: Attempt::Held(1),
    };
    assert_eq!(
        phase_change_line(Phase::Idle, opening),
        "link: idle to opening"
    );
    assert_eq!(STATE_POPULATED_LINE, "link: state populated");
}
