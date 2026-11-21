//! Control commands: the settings packets both sides send on the control channel.

use crate::error::{HEADER_LEN, ParseError, Parser, strip_header};

const HEADER: [u8; HEADER_LEN] = [0x04, 0x00, 0x04, 0x00, 0x09, 0x00];
/// Header, identifier and one value byte: the shortest command the peer sends.
const MIN_LEN: usize = 8;
/// Header, identifier and four value bytes, the length the host always sends.
const FRAME_LEN: usize = 11;
const STATE_ENABLED: u8 = 0x01;
const STATE_DISABLED: u8 = 0x02;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ControlId(pub u8);

impl ControlId {
    /// From `docs/control_commands.md` of librepods-org/librepods; the C++ daemon lacks it.
    pub const MIC_SIDE: Self = Self(0x01);
    /// Value is the noise mode plus one.
    pub const NOISE_CONTROL: Self = Self(0x0D);
    pub const ONE_BUD_ANC: Self = Self(0x1B);
    pub const CONVERSATIONAL_AWARENESS: Self = Self(0x28);
    /// Value 0 to 100.
    pub const ADAPTIVE_NOISE_LEVEL: Self = Self(0x2E);
    pub const ALLOW_OFF_OPTION: Self = Self(0x34);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlCommand {
    pub id: ControlId,
    pub value: [u8; 4],
}

impl ControlCommand {
    #[must_use]
    pub fn switch(id: ControlId, enabled: bool) -> Self {
        let state = if enabled {
            STATE_ENABLED
        } else {
            STATE_DISABLED
        };
        Self {
            id,
            value: [state, 0, 0, 0],
        }
    }

    /// Sample input: `04 00 04 00 09 00 28 01 00 00 00`, header, id, up to four value bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let body = strip_header(Parser::Control, bytes, HEADER, MIN_LEN)?;
        let Some((&id, rest)) = body.split_first() else {
            return Err(ParseError::TooShort {
                parser: Parser::Control,
                need: MIN_LEN,
                got: bytes.len(),
            });
        };
        let mut value = [0; 4];
        for (slot, byte) in value.iter_mut().zip(rest) {
            *slot = *byte;
        }
        Ok(Self {
            id: ControlId(id),
            value,
        })
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut frame = Vec::with_capacity(FRAME_LEN);
        frame.extend_from_slice(&HEADER);
        frame.push(self.id.0);
        frame.extend_from_slice(&self.value);
        frame
    }

    #[must_use]
    pub fn state(&self) -> Option<bool> {
        match self.value {
            [STATE_ENABLED, ..] => Some(true),
            [STATE_DISABLED, ..] => Some(false),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Header;
    use cpp_source::{CONTROL, PACKETS};

    const TEST_ID: ControlId = ControlId(0x42);

    fn command(value: [u8; 4]) -> Vec<u8> {
        ControlCommand { id: TEST_ID, value }.encode()
    }

    #[test]
    fn parse_active_rejects_empty() {
        assert!(ControlCommand::parse(&[]).is_err());
    }

    #[test]
    fn parse_active_rejects_short_header_only() {
        assert!(ControlCommand::parse(&HEADER).is_err());
    }

    #[test]
    fn parse_active_rejects_seven_bytes() {
        let mut buf = HEADER.to_vec();
        buf.push(0x42);
        assert_eq!(buf.len(), 7);
        assert_eq!(
            ControlCommand::parse(&buf),
            Err(ParseError::TooShort {
                parser: Parser::Control,
                need: 8,
                got: 7
            })
        );
    }

    #[test]
    fn parse_active_rejects_wrong_header() {
        let buf = [0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x01, 0x01];
        assert_eq!(
            ControlCommand::parse(&buf),
            Err(ParseError::BadHeader {
                parser: Parser::Control,
                expected: Header(HEADER),
                got: Header([0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa]),
            })
        );
    }

    #[test]
    fn parse_active_returns_byte_at7() {
        let mut buf = HEADER.to_vec();
        buf.push(0x42);
        buf.push(0x01);
        assert_eq!(ControlCommand::parse(&buf).unwrap().value[0], 0x01);
    }

    #[test]
    fn parse_state_enabled() {
        let pkt = command([0x01, 0x00, 0x00, 0x00]);
        assert_eq!(ControlCommand::parse(&pkt).unwrap().state(), Some(true));
    }

    #[test]
    fn parse_state_disabled() {
        let pkt = command([0x02, 0x00, 0x00, 0x00]);
        assert_eq!(ControlCommand::parse(&pkt).unwrap().state(), Some(false));
    }

    #[test]
    fn parse_state_unknown_value_is_nullopt() {
        let pkt = command([0x05, 0x00, 0x00, 0x00]);
        assert_eq!(ControlCommand::parse(&pkt).unwrap().state(), None);
    }

    #[test]
    fn create_builds_expected_shape() {
        let pkt = command([0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(pkt.len(), 11);
        assert!(pkt.starts_with(&HEADER));
        assert_eq!(pkt[6], 0x42);
        assert_eq!(pkt[7], 0xAA);
        assert_eq!(pkt[8], 0xBB);
        assert_eq!(pkt[9], 0xCC);
        assert_eq!(pkt[10], 0xDD);
    }

    #[test]
    fn switch_matches_the_cpp_enabled_and_disabled_packets() {
        let header = CONTROL.hex(Some("namespace ControlCommand"), "HEADER");
        for (id, namespace) in [
            (ControlId::ONE_BUD_ANC, "namespace OneBudANCMode"),
            (
                ControlId::CONVERSATIONAL_AWARENESS,
                "namespace ConversationalAwareness",
            ),
            (ControlId::ALLOW_OFF_OPTION, "namespace AllowOffOption"),
        ] {
            let cpp_id = PACKETS.byte(Some(namespace), "BasicControlCommand<", '>');
            for (enabled, create) in [
                (true, "::ENABLED = create("),
                (false, "::DISABLED = create("),
            ] {
                // create fills the three value bytes it is not given with 0x00.
                let mut expected = header.clone();
                expected.extend([cpp_id, CONTROL.byte(None, create, ')'), 0, 0, 0]);
                let command = ControlCommand::switch(id, enabled);
                assert_eq!(command.encode(), expected, "{namespace} {create}");
                assert_eq!(
                    ControlCommand::parse(&expected).unwrap().state(),
                    Some(enabled)
                );
            }
        }
    }

    #[test]
    fn noise_and_adaptive_ids_match_the_cpp_headers() {
        let noise = Some("namespace NoiseControl");
        let cpp_noise = PACKETS.byte(noise, "HEADER = ControlCommand::HEADER +", ';');
        assert_eq!(ControlId::NOISE_CONTROL.0, cpp_noise);
        let mut adaptive = HEADER.to_vec();
        adaptive.push(ControlId::ADAPTIVE_NOISE_LEVEL.0);
        assert_eq!(
            adaptive,
            PACKETS.hex(Some("namespace AdaptiveNoise"), "HEADER")
        );
    }
}
