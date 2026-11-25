//! Connection packets shared by the btsnoop and `PacketLogger` fixtures.

use super::{EVENT_CONNECTION_COMPLETE, OPCODE_COMMAND, STATUS_SUCCESS, Trace};

/// The opcode of HCI Create Connection, whose parameters open with the address (Core 6.1 Vol 4 Part E 7.1.5).
const CREATE_CONNECTION: u16 = 0x0405;
/// What a Create Connection command ends with: made up packet types, page scan mode, clock offset and role switch.
const CONNECTION_PARAMETERS: [u8; 7] = [0x18, 0xcc, 0x01, 0x00, 0x00, 0x00, 0x01];
/// What a connection complete event ends with: an ACL link, not encrypted (Core 6.1 Vol 4 Part E 7.7.3).
const ACL_NOT_ENCRYPTED: [u8; 2] = [0x01, 0x00];

/// An address in the order a packet carries it.
pub(crate) fn wire(address: [u8; 6]) -> Vec<u8> {
    address.into_iter().rev().collect()
}

impl Trace {
    /// One HCI command: opcode, parameter length, parameters.
    fn command(&mut self, opcode: u16, parameters: &[u8]) {
        let mut packet = opcode.to_le_bytes().to_vec();
        packet.push(u8::try_from(parameters.len()).unwrap());
        packet.extend(parameters);
        self.record(OPCODE_COMMAND, &packet);
    }

    /// The host asks for a link to the device at `address`.
    pub(crate) fn create_connection(&mut self, address: [u8; 6]) {
        let mut parameters = wire(address);
        parameters.extend(CONNECTION_PARAMETERS);
        self.command(CREATE_CONNECTION, &parameters);
    }

    /// The answer to either: an ACL link to `address` on `handle`, or with a `status` other than success none.
    pub(crate) fn connection_complete(&mut self, status: u8, handle: u16, address: [u8; 6]) {
        let mut parameters = vec![status];
        parameters.extend(handle.to_le_bytes());
        parameters.extend(wire(address));
        parameters.extend(ACL_NOT_ENCRYPTED);
        self.event(EVENT_CONNECTION_COMPLETE, &parameters);
    }

    pub(crate) fn link_up(&mut self, handle: u16, address: [u8; 6]) {
        self.connection_complete(STATUS_SUCCESS, handle, address);
    }
}
