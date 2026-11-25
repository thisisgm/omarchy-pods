//! The report the pods send when Conversation Awareness detects speech.

use crate::error::{HEADER_LEN, Parser, strip_header};

pub const OPCODE: u16 = 0x004b;
const HEADER: [u8; HEADER_LEN] = {
    let [low, high] = OPCODE.to_le_bytes();
    [0x04, 0x00, 0x04, 0x00, low, high]
};
const REPORT_PREFIX: [u8; 3] = [0x02, 0x00, 0x01];
// These levels are documented and still await confirmation on our pods.
const LEVEL_SPEAKING: [u8; 2] = [0x01, 0x02];
const LEVEL_ENDED: [u8; 2] = [0x08, 0x09];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Awareness {
    Started,
    Ended,
    Ignored,
}

/// Sample input: `04 00 04 00 4b 00 02 00 01 01`, the wearer speaks.
#[must_use]
pub fn parse(sdu: &[u8]) -> Option<Awareness> {
    let body = strip_header(Parser::Awareness, sdu, HEADER, HEADER_LEN).ok()?;
    let &[level] = body.strip_prefix(&REPORT_PREFIX)? else {
        return None;
    };
    Some(if LEVEL_SPEAKING.contains(&level) {
        Awareness::Started
    } else if LEVEL_ENDED.contains(&level) {
        Awareness::Ended
    } else {
        Awareness::Ignored
    })
}

#[cfg(test)]
mod tests {
    use pods_samples::awareness_of;
    use proptest::prelude::*;

    use super::{Awareness, OPCODE, parse};
    use crate::dictation::{SESSION_ACTIVE, SESSION_IDLE};
    use crate::fixture::hex;
    use crate::packet::{Incoming, classify};

    const INPUT_BOUND: usize = 64;

    #[test]
    fn levels_one_and_two_are_started() {
        for level in [0x01, 0x02] {
            assert_eq!(parse(&awareness_of(level)), Some(Awareness::Started));
        }
    }

    #[test]
    fn levels_eight_and_nine_are_ended() {
        for level in [0x08, 0x09] {
            assert_eq!(parse(&awareness_of(level)), Some(Awareness::Ended));
        }
    }

    #[test]
    fn any_other_level_is_ignored() {
        for level in (0..=u8::MAX).filter(|level| !matches!(level, 0x01 | 0x02 | 0x08 | 0x09)) {
            assert_eq!(
                parse(&awareness_of(level)),
                Some(Awareness::Ignored),
                "level {level:#04x}"
            );
        }
    }

    #[test]
    fn the_hosts_session_packets_are_not_awareness() {
        for session in [&SESSION_ACTIVE, &SESSION_IDLE] {
            assert_eq!(parse(session), None);
            assert_eq!(classify(session), Ok(Incoming::Other { opcode: OPCODE }));
        }
    }

    #[test]
    fn a_short_or_long_packet_is_other() {
        for report in [
            hex("04 00 04 00 4b 00"),
            hex("04 00 04 00 4b 00 02 00 01"),
            hex("04 00 04 00 4b 00 02 00 01 01 00"),
        ] {
            assert_eq!(parse(&report), None);
            assert_eq!(classify(&report), Ok(Incoming::Other { opcode: OPCODE }));
        }
    }

    #[test]
    fn the_report_requires_its_header_and_fixed_body() {
        for report in [
            hex("04 00 04 00 4b 01 02 00 01 01"),
            hex("03 00 04 00 4b 00 02 00 01 01"),
        ] {
            assert_eq!(parse(&report), None);
        }
        for report in [
            hex("04 00 04 00 4b 00 03 00 01 01"),
            hex("04 00 04 00 4b 00 02 01 01 01"),
            hex("04 00 04 00 4b 00 02 00 00 01"),
        ] {
            assert_eq!(parse(&report), None);
            assert_eq!(classify(&report), Ok(Incoming::Other { opcode: OPCODE }));
        }
    }

    proptest! {
        #[test]
        fn any_bytes_never_panic(sdu in proptest::collection::vec(any::<u8>(), 0..INPUT_BOUND)) {
            let _report = parse(&sdu);
        }
    }
}
