//! Sorts one control channel SDU by its header and opcode to that opcode's parser.

use crate::battery::BatteryReport;
use crate::control::ControlCommand;
use crate::ear::EarReport;
use crate::error::{HEADER_LEN, ParseError, Parser};
use crate::keys::ProximityKeys;
use crate::metadata::Metadata;

/// Starts every packet the pods send after the handshake; the opcode follows.
pub const ACCESSORY_HEADER: [u8; 4] = [0x04, 0x00, 0x04, 0x00];
/// A little endian u16 right after the accessory header.
const OPCODE_LEN: usize = 2;
const OPCODE_BATTERY: u16 = 0x0004;
const OPCODE_EAR: u16 = 0x0006;
const OPCODE_CONTROL: u16 = 0x0009;
const OPCODE_METADATA: u16 = 0x001d;
const OPCODE_PROXIMITY_KEYS: u16 = 0x0031;
const OPCODE_UPLINK: u16 = 0x0058;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming<'a> {
    Battery(BatteryReport),
    Ear(EarReport),
    Control(ControlCommand),
    Metadata(Metadata),
    Keys(ProximityKeys),
    /// The whole packet, for `UplinkPacket::parse`.
    Uplink(&'a [u8]),
    Other {
        opcode: u16,
    },
    /// An SDU without the accessory header, such as the handshake acknowledgement.
    Foreign,
}

/// Sample input: `04 00 04 00 09 00 28 01 00 00 00`, accessory header, opcode 0x0009, body.
pub fn classify(sdu: &[u8]) -> Result<Incoming<'_>, ParseError> {
    if !sdu.starts_with(&ACCESSORY_HEADER) {
        return Ok(Incoming::Foreign);
    }
    let Some(opcode) = opcode_of(sdu) else {
        return Err(ParseError::TooShort {
            parser: Parser::Packet,
            need: HEADER_LEN,
            got: sdu.len(),
        });
    };
    Ok(match opcode {
        OPCODE_BATTERY => Incoming::Battery(BatteryReport::parse(sdu)?),
        OPCODE_EAR => Incoming::Ear(EarReport::parse(sdu)?),
        OPCODE_CONTROL => Incoming::Control(ControlCommand::parse(sdu)?),
        OPCODE_METADATA => Incoming::Metadata(Metadata::parse(sdu)?),
        OPCODE_PROXIMITY_KEYS => Incoming::Keys(ProximityKeys::parse(sdu)?),
        OPCODE_UPLINK => Incoming::Uplink(sdu),
        opcode => Incoming::Other { opcode },
    })
}

/// Sample input: `04 00 04 00 04 00 03`, opcode 0x0004 whether or not its body parses; none without the accessory header or cut inside the opcode.
#[must_use]
pub fn opcode_of(sdu: &[u8]) -> Option<u16> {
    let after_header = sdu.strip_prefix(&ACCESSORY_HEADER)?;
    let opcode = after_header.first_chunk::<OPCODE_LEN>()?;
    Some(u16::from_le_bytes(*opcode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battery::BatteryReport;
    use crate::control::ControlCommand;
    use crate::ear::EarReport;
    use crate::error::Parser;
    use crate::fixture::hex;
    use crate::keys::ProximityKeys;
    use crate::metadata::Metadata;

    #[test]
    fn each_opcode_reaches_its_parser() {
        let battery = pods_samples::battery();
        let expected = Incoming::Battery(BatteryReport::parse(&battery).unwrap());
        assert_eq!(classify(&battery), Ok(expected));
        let ear = pods_samples::ear();
        let expected = Incoming::Ear(EarReport::parse(&ear).unwrap());
        assert_eq!(classify(&ear), Ok(expected));
        let control = pods_samples::control();
        let expected = Incoming::Control(ControlCommand::parse(&control).unwrap());
        assert_eq!(classify(&control), Ok(expected));
        let metadata = pods_samples::metadata();
        let expected = Incoming::Metadata(Metadata::parse(&metadata).unwrap());
        assert_eq!(classify(&metadata), Ok(expected));
        let keys = pods_samples::keys_of(pods_samples::IDENTITY_KEY, pods_samples::PAYLOAD_KEY);
        let expected = Incoming::Keys(ProximityKeys::parse(&keys).unwrap());
        assert_eq!(classify(&keys), Ok(expected));
    }

    #[test]
    fn each_packet_names_its_opcode_and_a_foreign_or_cut_one_none() {
        let packets = [
            (pods_samples::battery(), 0x04),
            (hex("04 00 04 00 04 00 03"), 0x04),
            (pods_samples::control(), 0x09),
            (hex("04 00 04 00 04 01"), 0x0104),
        ];
        for (sdu, opcode) in packets {
            assert_eq!(opcode_of(&sdu), Some(opcode), "{sdu:02x?}");
        }
        for sdu in [pods_samples::handshake_ack(), hex("04 00 04 00 09")] {
            assert_eq!(opcode_of(&sdu), None, "{sdu:02x?}");
        }
    }

    #[test]
    fn a_battery_packet_of_the_wrong_length_is_the_battery_parsers_error() {
        let mut battery = pods_samples::battery();
        battery.pop();
        let wrong_length = ParseError::WrongLength {
            parser: Parser::Battery,
            expected: 22,
            got: 21,
        };
        assert_eq!(classify(&battery), Err(wrong_length));
    }

    #[test]
    fn a_short_packet_is_the_error_of_its_opcodes_parser() {
        for (header, parser, need) in [
            ("04 00 04 00 06 00", Parser::Ear, 8),
            ("04 00 04 00 09 00", Parser::Control, 8),
            ("04 00 04 00 1d 00", Parser::Metadata, 11),
            ("04 00 04 00 31 00", Parser::ProximityKeys, 47),
        ] {
            let too_short = ParseError::TooShort {
                parser,
                need,
                got: 6,
            };
            assert_eq!(classify(&hex(header)), Err(too_short), "{header}");
        }
    }

    #[test]
    fn an_unknown_opcode_is_other_with_its_number() {
        let other = hex("04 00 04 00 4e 00 01 02 03");
        assert_eq!(classify(&other), Ok(Incoming::Other { opcode: 0x004e }));
        let high_byte_set = hex("04 00 04 00 04 01");
        assert_eq!(
            classify(&high_byte_set),
            Ok(Incoming::Other { opcode: 0x0104 })
        );
    }

    #[test]
    fn the_handshake_acknowledgement_is_foreign() {
        let ack = pods_samples::handshake_ack();
        assert_eq!(classify(&ack), Ok(Incoming::Foreign));
        for foreign in [
            "04",
            "04 00 04",
            "05 00 04 00 09 00 28 01",
            "04 00 04 01 09 00",
        ] {
            assert_eq!(classify(&hex(foreign)), Ok(Incoming::Foreign), "{foreign}");
        }
        assert_eq!(classify(&[]), Ok(Incoming::Foreign));
    }

    #[test]
    fn a_header_without_an_opcode_is_the_packet_parsers_error() {
        for (sdu, got) in [("04 00 04 00", 4), ("04 00 04 00 09", 5)] {
            let too_short = ParseError::TooShort {
                parser: Parser::Packet,
                need: 6,
                got,
            };
            assert_eq!(classify(&hex(sdu)), Err(too_short), "{sdu}");
        }
    }

    #[test]
    fn an_uplink_packet_is_handed_over_whole() {
        for sdu in ["04 00 04 00 58 00 01 00 aa bb", "04 00 04 00 58 00"] {
            let sdu = hex(sdu);
            assert_eq!(classify(&sdu), Ok(Incoming::Uplink(&sdu)));
        }
    }

    #[test]
    fn the_first_burst_classifies_without_an_error() {
        let burst = pods_samples::first_burst();
        assert_eq!(burst.len(), 38);
        let mut counts = [0_usize; 5];
        let mut others = Vec::new();
        for sdu in &burst {
            match classify(sdu).unwrap() {
                Incoming::Battery(_) => counts[0] += 1,
                Incoming::Ear(_) => counts[1] += 1,
                Incoming::Control(_) => counts[2] += 1,
                Incoming::Metadata(_) => counts[3] += 1,
                Incoming::Foreign => counts[4] += 1,
                Incoming::Other { opcode } => others.push(opcode),
                Incoming::Uplink(_) => panic!("the first burst carries no uplink packet"),
                Incoming::Keys(_) => panic!("the first burst carries no proximity keys"),
            }
        }
        assert_eq!(counts, [1, 1, 22, 1, 1]);
        others.sort_unstable();
        let expected = [
            0x02, 0x08, 0x0c, 0x0c, 0x0c, 0x0e, 0x2b, 0x2e, 0x2e, 0x4e, 0x53, 0x63,
        ];
        assert_eq!(others, expected);
    }
}
