//! Listening modes and the order the cycle steps through them.

use crate::control::{ControlCommand, ControlId};
use crate::error::{ParseError, Parser};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoiseMode {
    Off,
    Anc,
    Transparency,
    Adaptive,
}

/// The number the status document publishes and the byte the noise control command carries.
struct Codes {
    status: u8,
    wire: u8,
}

impl NoiseMode {
    const ALL: [Self; 4] = [Self::Off, Self::Anc, Self::Transparency, Self::Adaptive];

    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Anc,
            Self::Anc => Self::Transparency,
            Self::Transparency => Self::Adaptive,
            Self::Adaptive => Self::Off,
        }
    }

    fn codes(self) -> Codes {
        let (status, wire) = match self {
            Self::Off => (0, 0x01),
            Self::Anc => (1, 0x02),
            Self::Transparency => (2, 0x03),
            Self::Adaptive => (3, 0x04),
        };
        Codes { status, wire }
    }

    #[must_use]
    pub fn command(self) -> ControlCommand {
        ControlCommand {
            id: ControlId::NOISE_CONTROL,
            value: [self.codes().wire, 0, 0, 0],
        }
    }

    /// Sample input: identifier 0D, value `03 00 00 00`, read as Transparency.
    pub fn from_command(command: &ControlCommand) -> Result<Self, ParseError> {
        if command.id != ControlId::NOISE_CONTROL {
            return Err(ParseError::UnknownValue {
                parser: Parser::NoiseCommand,
                field: "noise control id",
                value: command.id.0,
            });
        }
        let [wire, ..] = command.value;
        Self::ALL
            .into_iter()
            .find(|mode| mode.codes().wire == wire)
            .ok_or(ParseError::UnknownValue {
                parser: Parser::NoiseCommand,
                field: "noise control value",
                value: wire,
            })
    }
}

impl From<NoiseMode> for u8 {
    fn from(mode: NoiseMode) -> Self {
        mode.codes().status
    }
}

impl TryFrom<u8> for NoiseMode {
    type Error = ParseError;

    // Sample input: `2`, read as Transparency.
    fn try_from(value: u8) -> Result<Self, ParseError> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.codes().status == value)
            .ok_or(ParseError::UnknownValue {
                parser: Parser::NoiseMode,
                field: "noise mode",
                value,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_follows_the_noise_cycle_order() {
        assert_eq!(NoiseMode::Off.next(), NoiseMode::Anc);
        assert_eq!(NoiseMode::Anc.next(), NoiseMode::Transparency);
        assert_eq!(NoiseMode::Transparency.next(), NoiseMode::Adaptive);
        assert_eq!(NoiseMode::Adaptive.next(), NoiseMode::Off);
    }

    #[test]
    fn try_from_takes_the_enum_values_and_rejects_the_rest() {
        assert_eq!(NoiseMode::try_from(0).unwrap(), NoiseMode::Off);
        assert_eq!(NoiseMode::try_from(3).unwrap(), NoiseMode::Adaptive);
        assert_eq!(
            NoiseMode::try_from(4),
            Err(ParseError::UnknownValue {
                parser: Parser::NoiseMode,
                field: "noise mode",
                value: 4
            })
        );
    }

    #[test]
    fn noise_commands_carry_the_wire_values() {
        for (mode, status, wire) in [
            (NoiseMode::Off, 0, 1),
            (NoiseMode::Anc, 1, 2),
            (NoiseMode::Transparency, 2, 3),
            (NoiseMode::Adaptive, 3, 4),
        ] {
            assert_eq!(u8::from(mode), status);
            assert_eq!(NoiseMode::try_from(status), Ok(mode));
            let encoded = mode.command().encode();
            assert_eq!(encoded, [4, 0, 4, 0, 9, 0, 13, wire, 0, 0, 0]);
            let parsed = ControlCommand::parse(&encoded).unwrap();
            assert_eq!(NoiseMode::from_command(&parsed), Ok(mode));
        }
    }

    #[test]
    fn from_command_rejects_values_outside_the_modes_and_other_ids() {
        let off_by_zero =
            ControlCommand::parse(&[0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x0d, 0x00]).unwrap();
        assert_eq!(
            NoiseMode::from_command(&off_by_zero),
            Err(ParseError::UnknownValue {
                parser: Parser::NoiseCommand,
                field: "noise control value",
                value: 0x00
            })
        );
        let past_adaptive =
            ControlCommand::parse(&[0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x0d, 0x05]).unwrap();
        assert_eq!(
            NoiseMode::from_command(&past_adaptive),
            Err(ParseError::UnknownValue {
                parser: Parser::NoiseCommand,
                field: "noise control value",
                value: 0x05
            })
        );
        let other_id =
            ControlCommand::parse(&[0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x28, 0x01]).unwrap();
        assert_eq!(
            NoiseMode::from_command(&other_id),
            Err(ParseError::UnknownValue {
                parser: Parser::NoiseCommand,
                field: "noise control id",
                value: 0x28
            })
        );
    }
}
