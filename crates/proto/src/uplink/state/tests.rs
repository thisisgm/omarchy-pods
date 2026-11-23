//! Support tests: the uplink markers at both values, every shape that is no marker, and arbitrary bytes; the parser has no user path yet.

use pods_samples::uplink::{session_marker, state_marker};
use proptest::prelude::*;

use super::*;

/// Where the six bytes the parser never reads begin, right after the header.
const OPAQUE_AT: usize = 6;
const OPAQUE_LEN: usize = 6;
/// Where the opcode sits, low byte first, up to the end of the header.
const OPCODE_AT: usize = 4;
/// The last byte of each marker, which says the state.
const STATE_SAYS_AT: usize = 12;
const SESSION_SAYS_AT: usize = 9;
/// Where the fixed bytes of the session marker begin, right after the header; they end where it says its state.
const SESSION_FIXED_AT: usize = 6;
/// One bit, which makes a byte another byte.
const ONE_BIT: u8 = 0x01;

fn state_of(running: bool) -> UplinkState {
    if running {
        UplinkState::Running
    } else {
        UplinkState::Stopped
    }
}

/// The state a marker says whose last byte is `last`, where the sample markers end in `stopped` and `running`.
fn said_by(last: u8, stopped: u8, running: u8) -> Option<UplinkState> {
    if last == stopped {
        Some(UplinkState::Stopped)
    } else if last == running {
        Some(UplinkState::Running)
    } else {
        None
    }
}

fn markers(running: bool) -> [Vec<u8>; 2] {
    [
        state_marker(running).to_vec(),
        session_marker(running).to_vec(),
    ]
}

#[test]
fn both_markers_read_at_both_values() {
    for running in [false, true] {
        for marker in markers(running) {
            assert_eq!(parse(&marker), Some(state_of(running)), "{marker:02x?}");
        }
    }
}

#[test]
fn every_other_last_byte_is_no_marker() {
    let (stopped, running) = (state_marker(false), state_marker(true));
    let state_says = (stopped[STATE_SAYS_AT], running[STATE_SAYS_AT]);
    let (stopped, running) = (session_marker(false), session_marker(true));
    let session_says = (stopped[SESSION_SAYS_AT], running[SESSION_SAYS_AT]);
    for last in 0..=u8::MAX {
        let mut state = state_marker(false);
        state[STATE_SAYS_AT] = last;
        let expected = said_by(last, state_says.0, state_says.1);
        assert_eq!(parse(&state), expected, "state marker ending {last:#04x}");
        let mut session = session_marker(false);
        session[SESSION_SAYS_AT] = last;
        let expected = said_by(last, session_says.0, session_says.1);
        assert_eq!(
            parse(&session),
            expected,
            "session marker ending {last:#04x}"
        );
    }
}

#[test]
fn a_marker_one_byte_short_or_long_is_no_marker() {
    for marker in [markers(false), markers(true)].concat() {
        // The byte goes or comes right after the header, so the last byte is still a value the parser knows.
        let mut short = marker.clone();
        short.remove(OPAQUE_AT);
        assert_eq!(parse(&short), None, "{short:02x?}");
        let mut long = marker.clone();
        long.insert(OPAQUE_AT, 0x01);
        assert_eq!(parse(&long), None, "{long:02x?}");
    }
}

#[test]
fn a_session_marker_with_another_fixed_byte_and_a_marker_under_another_opcode_are_no_marker() {
    for fixed in SESSION_FIXED_AT..SESSION_SAYS_AT {
        let mut session = session_marker(true);
        session[fixed] ^= ONE_BIT;
        assert_eq!(parse(&session), None, "{session:02x?}");
    }
    for marker in [markers(false), markers(true)].concat() {
        for opcode_byte in OPCODE_AT..OPAQUE_AT {
            let mut other = marker.clone();
            other[opcode_byte] ^= ONE_BIT;
            assert_eq!(parse(&other), None, "{other:02x?}");
        }
    }
}

proptest! {
    #[test]
    fn parse_never_panics(sdu in proptest::collection::vec(any::<u8>(), 0..64)) {
        let _state = parse(&sdu);
    }

    #[test]
    fn a_marker_reads_as_the_state_it_was_built_for_whatever_the_six_bytes(
        running in any::<bool>(),
        opaque in any::<[u8; OPAQUE_LEN]>(),
    ) {
        let mut state = state_marker(running);
        state[OPAQUE_AT..OPAQUE_AT + OPAQUE_LEN].copy_from_slice(&opaque);
        prop_assert_eq!(parse(&state), Some(state_of(running)));
        prop_assert_eq!(parse(&session_marker(running)), Some(state_of(running)));
    }
}
