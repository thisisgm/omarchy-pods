//! The one error type every packet parser returns.

use std::fmt;

/// Every control channel packet this crate reads starts with a header this long.
pub const HEADER_LEN: usize = 6;

/// The parser that refused its input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Parser {
    /// `BatteryReport::parse` and the component byte it reads.
    Battery,
    /// `BatteryReport::parse_decrypted`.
    DecryptedBattery,
    /// `EarReport::parse`.
    Ear,
    /// `ControlCommand::parse` and the value byte `MicSide::try_from` reads.
    Control,
    /// `NoiseMode::from_command`.
    NoiseCommand,
    /// `NoiseMode::try_from`.
    NoiseMode,
    /// `UplinkPacket::parse` reading a 0x58 packet cut before its subtype.
    Uplink,
    /// `UplinkPacket::parse` reading an audio SDU.
    UplinkAudio,
    /// `UplinkPacket::parse` reading a control packet.
    UplinkControl,
    /// `AttPdu::parse` reading an SDU with no opcode in it.
    Att,
    /// `AttPdu::parse` reading a Find Information reply.
    AttFindInformation,
    /// `AttPdu::parse` reading a Read By Type reply.
    AttReadByType,
    /// `AttPdu::parse` reading an error response.
    AttError,
    /// `AttPdu::parse` reading a notification.
    AttNotification,
    /// `AttPdu::parse` reading an indication.
    AttIndication,
    /// `DictationPacket::parse` reading a notification on the audio handle.
    DictationAudio,
}

impl fmt::Display for Parser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Battery => "battery packet",
            Self::DecryptedBattery => "decrypted battery payload",
            Self::Ear => "ear detection packet",
            Self::Control => "control command",
            Self::NoiseCommand => "noise control command",
            Self::NoiseMode => "noise mode number",
            Self::Uplink => "uplink packet",
            Self::UplinkAudio => "uplink audio packet",
            Self::UplinkControl => "uplink control packet",
            Self::Att => "attribute protocol packet",
            Self::AttFindInformation => "attribute protocol find information response",
            Self::AttReadByType => "attribute protocol read by type response",
            Self::AttError => "attribute protocol error response",
            Self::AttNotification => "attribute protocol notification",
            Self::AttIndication => "attribute protocol indication",
            Self::DictationAudio => "dictation audio notification",
        })
    }
}

/// The first six bytes of a control channel packet in hex; the protocol puts the packet type there, never an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header(pub [u8; HEADER_LEN]);

impl fmt::Display for Header {
    // Sample output: 04 00 04 00 09 00
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (at, byte) in self.0.iter().enumerate() {
            if at > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Why a packet was refused, naming the parser that refused it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Fewer bytes than the shortest valid packet.
    #[error("{parser} too short: need {need} bytes, got {got}")]
    TooShort {
        parser: Parser,
        need: usize,
        got: usize,
    },
    /// The packet does not start with the header this parser reads.
    #[error("{parser} has the wrong header: expected {expected}, got {got}")]
    BadHeader {
        parser: Parser,
        expected: Header,
        got: Header,
    },
    /// A byte holds a value the protocol does not define for that field.
    #[error("{parser} has an unknown {field} value {value:#04x}")]
    UnknownValue {
        parser: Parser,
        field: &'static str,
        value: u8,
    },
    /// The length disagrees with the length the packet must have.
    #[error("{parser} is {got} bytes, expected {expected}")]
    WrongLength {
        parser: Parser,
        expected: usize,
        got: usize,
    },
    /// A list that must hold at least one entry holds no whole one.
    #[error("{parser} has an empty list: its {list_len} bytes hold no whole entry")]
    EmptyList { parser: Parser, list_len: usize },
}

/// Checks the minimum length, then returns the bytes after `header`.
pub(crate) fn strip_header(
    parser: Parser,
    bytes: &[u8],
    header: [u8; HEADER_LEN],
    need: usize,
) -> Result<&[u8], ParseError> {
    let too_short = ParseError::TooShort {
        parser,
        need,
        got: bytes.len(),
    };
    if bytes.len() < need {
        return Err(too_short);
    }
    match bytes.split_first_chunk::<HEADER_LEN>() {
        Some((seen, body)) if *seen == header => Ok(body),
        Some((seen, _)) => Err(ParseError::BadHeader {
            parser,
            expected: Header(header),
            got: Header(*seen),
        }),
        None => Err(too_short),
    }
}

#[cfg(test)]
mod tests {
    use crate::att::AttPdu;
    use crate::battery::BatteryReport;
    use crate::control::ControlCommand;
    use crate::noise::NoiseMode;
    use crate::uplink::UplinkPacket;

    use super::*;

    #[test]
    fn too_short_names_the_parser_and_both_lengths() {
        let err = BatteryReport::parse(&[0x04, 0x00, 0x04, 0x00, 0x04, 0x00]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "battery packet too short: need 7 bytes, got 6"
        );
        let err = UplinkPacket::parse(&[0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x01]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "uplink packet too short: need 8 bytes, got 7"
        );
    }

    #[test]
    fn bad_header_names_the_parser_and_both_headers_in_hex() {
        let err =
            ControlCommand::parse(&[0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x01, 0x01]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "control command has the wrong header: expected 04 00 04 00 09 00, got ff ee dd cc bb aa"
        );
    }

    #[test]
    fn unknown_value_names_the_parser_field_and_value() {
        let err = BatteryReport::parse(&[0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x04]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "battery packet has an unknown battery count value 0x04"
        );
    }

    #[test]
    fn wrong_length_names_the_parser_and_both_lengths() {
        let err = BatteryReport::parse_decrypted(&[0; 15], true, false, false).unwrap_err();
        assert_eq!(
            err.to_string(),
            "decrypted battery payload is 15 bytes, expected 16"
        );
    }

    #[test]
    fn empty_list_names_the_parser_and_the_bytes_it_got() {
        let err = AttPdu::parse(&[0x09, 0x07, 0x02, 0x00, 0x14]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "attribute protocol read by type response has an empty list: its 3 bytes hold no whole entry"
        );
    }

    #[test]
    fn every_parser_has_a_name() {
        for (parser, name) in [
            (Parser::Battery, "battery packet"),
            (Parser::DecryptedBattery, "decrypted battery payload"),
            (Parser::Ear, "ear detection packet"),
            (Parser::Control, "control command"),
            (Parser::NoiseCommand, "noise control command"),
            (Parser::NoiseMode, "noise mode number"),
            (Parser::Uplink, "uplink packet"),
            (Parser::UplinkAudio, "uplink audio packet"),
            (Parser::UplinkControl, "uplink control packet"),
            (Parser::Att, "attribute protocol packet"),
            (
                Parser::AttFindInformation,
                "attribute protocol find information response",
            ),
            (
                Parser::AttReadByType,
                "attribute protocol read by type response",
            ),
            (Parser::AttError, "attribute protocol error response"),
            (Parser::AttNotification, "attribute protocol notification"),
            (Parser::AttIndication, "attribute protocol indication"),
            (Parser::DictationAudio, "dictation audio notification"),
        ] {
            assert_eq!(parser.to_string(), name);
        }
        let err = NoiseMode::try_from(4).unwrap_err();
        assert_eq!(
            err.to_string(),
            "noise mode number has an unknown noise mode value 0x04"
        );
    }
}
