//! Battery levels from the control channel packet and from the decrypted proximity payload.

use crate::error::{HEADER_LEN, ParseError, Parser, strip_header};

const HEADER: [u8; HEADER_LEN] = [0x04, 0x00, 0x04, 0x00, 0x04, 0x00];
/// Header plus the entry count byte.
const MIN_LEN: usize = 7;
/// Kind, spacer, level, status, end.
const ENTRY_LEN: usize = 5;
/// Left, right and case, or one headset.
const MAX_ENTRIES: u8 = 3;
/// The spacer and end byte of every entry.
const ENTRY_MARK: u8 = 0x01;
const STATUS_CHARGING: u8 = 0x01;
/// A part that is not connected; its entry leaves the last reading alone.
const STATUS_DISCONNECTED: u8 = 0x04;
/// Levels above this are unknown; 0x7F is the unknown sentinel of the proximity payload.
const MAX_LEVEL: u8 = 100;
const DECRYPTED_LEN: usize = 16;
const CHARGING_BIT: u8 = 0x80;
const LEVEL_MASK: u8 = 0x7F;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Component {
    Left,
    Right,
    Case,
    /// The one battery of an over ear headset.
    Headset,
}

impl TryFrom<u8> for Component {
    type Error = ParseError;

    // 01 headset, 02 right, 04 left, 08 case.
    fn try_from(value: u8) -> Result<Self, ParseError> {
        match value {
            0x01 => Ok(Self::Headset),
            0x02 => Ok(Self::Right),
            0x04 => Ok(Self::Left),
            0x08 => Ok(Self::Case),
            _ => Err(ParseError::UnknownValue {
                parser: Parser::Battery,
                field: "battery component",
                value,
            }),
        }
    }
}

/// A part with no reading is `None` wherever a `Charge` is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Charge {
    /// Percent from 0 to 100.
    pub level: u8,
    pub charging: bool,
}

/// A part left at `None` keeps its last reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BatteryReport {
    pub left: Option<Charge>,
    pub right: Option<Charge>,
    pub case: Option<Charge>,
    pub headset: Option<Charge>,
    /// The first bud or headset in the packet.
    pub primary: Option<Component>,
    /// The second bud in the packet.
    pub secondary: Option<Component>,
}

impl BatteryReport {
    /// Sample input: `04 00 04 00 04 00 01 01 01 55 02 01`, header, count, then entries.
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let body = strip_header(Parser::Battery, bytes, HEADER, MIN_LEN)?;
        let Some((&count, rest)) = body.split_first() else {
            return Err(ParseError::TooShort {
                parser: Parser::Battery,
                need: MIN_LEN,
                got: bytes.len(),
            });
        };
        if count > MAX_ENTRIES {
            return Err(ParseError::UnknownValue {
                parser: Parser::Battery,
                field: "battery count",
                value: count,
            });
        }
        let (entries, remainder) = rest.as_chunks::<ENTRY_LEN>();
        if !remainder.is_empty() || entries.len() != usize::from(count) {
            return Err(ParseError::WrongLength {
                parser: Parser::Battery,
                expected: usize::from(count)
                    .saturating_mul(ENTRY_LEN)
                    .saturating_add(MIN_LEN),
                got: bytes.len(),
            });
        }
        let mut report = Self::default();
        for &[kind, spacer, level, status, end] in entries {
            for (field, mark) in [("battery spacer", spacer), ("battery end", end)] {
                if mark != ENTRY_MARK {
                    return Err(ParseError::UnknownValue {
                        parser: Parser::Battery,
                        field,
                        value: mark,
                    });
                }
            }
            let Ok(component) = Component::try_from(kind) else {
                continue;
            };
            if component != Component::Case {
                report.note_order(component);
            }
            if status != STATUS_DISCONNECTED
                && let Some(level) = known_level(level)
            {
                *report.slot(component) = Some(Charge {
                    level,
                    charging: status == STATUS_CHARGING,
                });
            }
        }
        Ok(report)
    }

    /// Sample input: `00 50 50 D0` and 12 zero bytes, primary 80, secondary 80, case 80 charging.
    pub fn parse_decrypted(
        payload: &[u8],
        primary_left: bool,
        pod_in_case: bool,
        headset: bool,
    ) -> Result<Self, ParseError> {
        let Ok(&[_, first, second, case, ..]) = <&[u8; DECRYPTED_LEN]>::try_from(payload) else {
            return Err(ParseError::WrongLength {
                parser: Parser::DecryptedBattery,
                expected: DECRYPTED_LEN,
                got: payload.len(),
            });
        };
        if headset {
            // The primary flag only renames byte 1, which always holds the headset.
            return Ok(Self {
                headset: decrypted_charge(first),
                primary: Some(Component::Headset),
                ..Self::default()
            });
        }
        let (primary, secondary) = if primary_left {
            (Component::Left, Component::Right)
        } else {
            (Component::Right, Component::Left)
        };
        let (left, right) = if primary_left {
            (first, second)
        } else {
            (second, first)
        };
        // Only a docked bud can read the case, so a 0 with none docked is unknown.
        let case = decrypted_charge(case).filter(|c| c.level != 0 || pod_in_case);
        Ok(Self {
            left: decrypted_charge(left),
            right: decrypted_charge(right),
            case,
            headset: None,
            primary: Some(primary),
            secondary: Some(secondary),
        })
    }

    fn note_order(&mut self, component: Component) {
        if self.primary.is_none() {
            self.primary = Some(component);
        } else if self.secondary.is_none() {
            self.secondary = Some(component);
        }
    }

    fn slot(&mut self, component: Component) -> &mut Option<Charge> {
        match component {
            Component::Left => &mut self.left,
            Component::Right => &mut self.right,
            Component::Case => &mut self.case,
            Component::Headset => &mut self.headset,
        }
    }
}

/// The last known battery of every part; `None` if not read since the reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Batteries {
    pub left: Option<Charge>,
    pub right: Option<Charge>,
    pub case: Option<Charge>,
    pub headset: Option<Charge>,
}

impl Batteries {
    pub fn apply(&mut self, report: &BatteryReport) {
        let updates = [
            (&mut self.left, report.left),
            (&mut self.right, report.right),
            (&mut self.case, report.case),
            (&mut self.headset, report.headset),
        ];
        for (slot, update) in updates {
            if update.is_some() {
                *slot = update;
            }
        }
    }
}

fn known_level(level: u8) -> Option<u8> {
    (level <= MAX_LEVEL).then_some(level)
}

// 0xDA: high bit set so charging, low seven bits 0x5A so 90 percent.
fn decrypted_charge(byte: u8) -> Option<Charge> {
    known_level(byte & LEVEL_MASK).map(|level| Charge {
        level,
        charging: byte & CHARGING_BIT != 0,
    })
}

#[cfg(test)]
mod tests;
