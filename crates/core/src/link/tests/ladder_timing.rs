//! Recovery timing contracts in milliseconds.

use crate::link::ladder::{ABSENT_GRACE_MS, FIRST_RETRY_MS, HELD_DELAYS_MS, NOTIFY_REREQUEST_MS};

#[test]
fn the_connected_ladder_doubles_until_the_cap() {
    assert_eq!(
        HELD_DELAYS_MS,
        [
            1000, 2000, 4000, 8000, 16000, 16000, 16000, 16000, 16000, 16000
        ]
    );
}

#[test]
fn the_absent_grace_covers_three_probes_after_the_first_delay() {
    assert_eq!(FIRST_RETRY_MS, 750);
    assert_eq!(ABSENT_GRACE_MS, 750 + 1000 + 2000 + 4000);
}

#[test]
fn notifications_are_requested_again_after_two_seconds() {
    assert_eq!(NOTIFY_REREQUEST_MS, 2000);
}
