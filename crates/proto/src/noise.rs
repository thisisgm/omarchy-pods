//! Listening modes and the order the cycle steps through them.

use crate::control::{ControlCommand, ControlId};
use crate::error::ParseError;

/// A listening mode, numbered as the status document publishes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoiseMode {
    Off = 0,
    Anc = 1,
    Transparency = 2,
    Adaptive = 3,
}

impl NoiseMode {
    /// The mode after this one: Off, Anc, Transparency, Adaptive, then Off again.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Anc,
            Self::Anc => Self::Transparency,
            Self::Transparency => Self::Adaptive,
            Self::Adaptive => Self::Off,
        }
    }

    /// The control command that selects this mode; the wire value is the mode plus one.
    #[must_use]
    pub fn command(self) -> ControlCommand {
        let wire = match self {
            Self::Off => 0x01,
            Self::Anc => 0x02,
            Self::Transparency => 0x03,
            Self::Adaptive => 0x04,
        };
        ControlCommand {
            id: ControlId::NOISE_CONTROL,
            value: [wire, 0, 0, 0],
        }
    }

    /// The mode a noise control command reports.
    // Identifier 0D, value 03 00 00 00: Transparency.
    pub fn from_command(command: &ControlCommand) -> Result<Self, ParseError> {
        if command.id != ControlId::NOISE_CONTROL {
            return Err(ParseError::UnknownValue {
                field: "noise control id",
                value: command.id.0,
            });
        }
        let [wire, ..] = command.value;
        match wire {
            0x01 => Ok(Self::Off),
            0x02 => Ok(Self::Anc),
            0x03 => Ok(Self::Transparency),
            0x04 => Ok(Self::Adaptive),
            _ => Err(ParseError::UnknownValue {
                field: "noise control value",
                value: wire,
            }),
        }
    }
}

impl TryFrom<u8> for NoiseMode {
    type Error = ParseError;

    // 0 Off, 1 Anc, 2 Transparency, 3 Adaptive.
    fn try_from(value: u8) -> Result<Self, ParseError> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::Anc),
            2 => Ok(Self::Transparency),
            3 => Ok(Self::Adaptive),
            _ => Err(ParseError::UnknownValue {
                field: "noise mode",
                value,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::ControlCommand;

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
                field: "noise mode",
                value: 4
            })
        );
    }

    #[test]
    fn command_matches_the_cpp_noise_control_packets() {
        let header = [0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x0d];
        for (mode, wire) in [
            (NoiseMode::Off, 0x01),
            (NoiseMode::Anc, 0x02),
            (NoiseMode::Transparency, 0x03),
            (NoiseMode::Adaptive, 0x04),
        ] {
            let mut expected = header.to_vec();
            expected.extend([wire, 0x00, 0x00, 0x00]);
            assert_eq!(mode.command().encode(), expected);
            assert_eq!(NoiseMode::from_command(&mode.command()), Ok(mode));
        }
    }

    #[test]
    fn from_command_rejects_values_outside_the_modes_and_other_ids() {
        let off_by_zero =
            ControlCommand::parse(&[0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x0d, 0x00]).unwrap();
        assert!(NoiseMode::from_command(&off_by_zero).is_err());
        let past_adaptive =
            ControlCommand::parse(&[0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x0d, 0x05]).unwrap();
        assert!(NoiseMode::from_command(&past_adaptive).is_err());
        let other_id =
            ControlCommand::parse(&[0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x28, 0x01]).unwrap();
        assert!(NoiseMode::from_command(&other_id).is_err());
    }
}
