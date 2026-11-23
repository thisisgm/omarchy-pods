//! The uplink markers: the two packets by which the pods say whether the uplink runs.

use crate::error::HEADER_LEN;
use crate::packet::opcode_of;

/// The state marker: 13 bytes, of which only the last is read.
pub const STATE_OPCODE: u16 = 0x000e;
/// The session marker: 10 bytes, all of them fixed but the last.
pub const SESSION_OPCODE: u16 = 0x0055;
const STATE_STOPPED: u8 = 0x02;
const STATE_RUNNING: u8 = 0x22;
const SESSION_STOPPED: [u8; 4] = [0x01, 0x01, 0x00, 0x19];
const SESSION_RUNNING: [u8; 4] = [0x01, 0x01, 0x00, 0x32];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UplinkState {
    Stopped,
    Running,
}

/// Sample input: `04 00 04 00 0e 00`, six bytes that are never read, then `22`, running; or `04 00 04 00 55 00 01 01 00 19`, stopped.
#[must_use]
pub fn parse(sdu: &[u8]) -> Option<UplinkState> {
    let opcode = opcode_of(sdu)?;
    let body = sdu.get(HEADER_LEN..)?;
    match (opcode, body) {
        (STATE_OPCODE, [_, _, _, _, _, _, STATE_STOPPED]) => Some(UplinkState::Stopped),
        (STATE_OPCODE, [_, _, _, _, _, _, STATE_RUNNING]) => Some(UplinkState::Running),
        (SESSION_OPCODE, body) if body == SESSION_STOPPED => Some(UplinkState::Stopped),
        (SESSION_OPCODE, body) if body == SESSION_RUNNING => Some(UplinkState::Running),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
