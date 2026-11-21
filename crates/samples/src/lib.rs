//! Sample control channel packets for tests, each built from the documented layout with synthetic contents.

// Layouts are those of daemon/airpods_packets.h and parseMetadata in daemon/main.cpp; no byte comes from a real device.

/// Every packet the pods send after the handshake starts with these bytes; the opcode follows.
const ACCESSORY_HEADER: [u8; 4] = [0x04, 0x00, 0x04, 0x00];
/// `Parse::HANDSHAKE_ACK`, the start of the reply to the handshake.
const HANDSHAKE_ACK_PREFIX: [u8; 4] = [0x01, 0x00, 0x04, 0x00];
/// Synthetic bytes behind the acknowledgement prefix, which nothing reads.
const HANDSHAKE_ACK_TAIL: [u8; 12] = [0x01, 0x00, 0x02, 0x00, 0, 0, 0, 0, 0, 0, 0, 0];

const OPCODE_BATTERY: u16 = 0x0004;
const OPCODE_EAR: u16 = 0x0006;
const OPCODE_CONTROL: u16 = 0x0009;
const OPCODE_METADATA: u16 = 0x001d;
const OPCODE_FEATURES_ACK: u16 = 0x002b;

/// Synthetic body of an acknowledgement or a packet nobody parses.
const FILLER: [u8; 4] = [0x00, 0x00, 0x00, 0x00];

/// Battery count 3, then right 100% not charging, left 99% charging, case 50% not charging.
const BATTERY_BODY: [u8; 16] = [
    0x03, 0x02, 0x01, 0x64, 0x02, 0x01, 0x04, 0x01, 0x63, 0x01, 0x01, 0x08, 0x01, 0x32, 0x02, 0x01,
];
/// Primary and secondary bud both in the ear.
const EAR_BODY: [u8; 2] = [0x00, 0x00];
/// Conversation awareness 0x28, switched on, then three zero value bytes.
const CONTROL_BODY: [u8; 5] = [0x28, 0x01, 0x00, 0x00, 0x00];
/// The value bytes of every setting in the first burst: on, then three zero bytes.
const SETTING_ON: [u8; 4] = [0x01, 0x00, 0x00, 0x00];
/// The control commands of the first burst, one per setting identifier from 1 up.
const BURST_SETTINGS: u8 = 22;

/// Synthetic bytes between the metadata header and the name, which no parser reads.
const METADATA_SKIPPED: [u8; 5] = [0x01, 0x02, 0x03, 0x04, 0x05];
const METADATA_NAME: &str = "Test\u{2019}s AirPods Pro";
const METADATA_MODEL_NUMBER: &str = "A3063";
const METADATA_MANUFACTURER: &str = "Apple Inc.";
/// A synthetic serial number in the fourth string, where the real packet carries one.
const METADATA_SERIAL: &str = "SERIAL0001";
/// The length of the metadata packet measured on a real device, reached with zero bytes.
const METADATA_LEN: usize = 247;

/// Opcodes of the first burst that no parser reads, each as often as it was measured.
const BURST_OTHER_OPCODES: [u16; 11] = [
    0x02, 0x08, 0x0c, 0x0c, 0x0c, 0x0e, 0x2e, 0x2e, 0x4e, 0x53, 0x63,
];

/// One packet from the pods: the accessory header, the opcode little endian, then the body.
// Sample output: 04 00 04 00 06 00 00 00 for opcode 0x0006 and body 00 00.
fn packet(opcode: u16, body: &[u8]) -> Vec<u8> {
    let mut sdu = ACCESSORY_HEADER.to_vec();
    sdu.extend(opcode.to_le_bytes());
    sdu.extend(body);
    sdu
}

/// The reply to the handshake request.
#[must_use]
pub fn handshake_ack() -> Vec<u8> {
    [&HANDSHAKE_ACK_PREFIX[..], &HANDSHAKE_ACK_TAIL].concat()
}

/// The reply to the set specific features request.
#[must_use]
pub fn features_ack() -> Vec<u8> {
    packet(OPCODE_FEATURES_ACK, &FILLER)
}

/// A battery packet for both buds and the case, 22 bytes.
#[must_use]
pub fn battery() -> Vec<u8> {
    packet(OPCODE_BATTERY, &BATTERY_BODY)
}

/// An ear detection packet with both buds in the ear.
#[must_use]
pub fn ear() -> Vec<u8> {
    packet(OPCODE_EAR, &EAR_BODY)
}

/// A control command that switches conversation awareness on.
#[must_use]
pub fn control() -> Vec<u8> {
    packet(OPCODE_CONTROL, &CONTROL_BODY)
}

/// A metadata packet: name, model number, manufacturer and a serial number, each ended by a zero byte.
#[must_use]
pub fn metadata() -> Vec<u8> {
    let mut body = METADATA_SKIPPED.to_vec();
    for field in [
        METADATA_NAME,
        METADATA_MODEL_NUMBER,
        METADATA_MANUFACTURER,
        METADATA_SERIAL,
    ] {
        body.extend(field.as_bytes());
        body.push(0);
    }
    let mut sdu = packet(OPCODE_METADATA, &body);
    sdu.resize(METADATA_LEN, 0);
    sdu
}

/// The 38 packets that follow the handshake: only the census (which opcodes, how many of each) and the order of the first two follow a real device; the order and the bodies after them are made up.
#[must_use]
pub fn first_burst() -> Vec<Vec<u8>> {
    let mut burst = vec![
        handshake_ack(),
        features_ack(),
        battery(),
        ear(),
        metadata(),
    ];
    for id in 1..=BURST_SETTINGS {
        let body = [&[id][..], &SETTING_ON].concat();
        burst.push(packet(OPCODE_CONTROL, &body));
    }
    for opcode in BURST_OTHER_OPCODES {
        burst.push(packet(opcode, &FILLER));
    }
    burst
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The opcode of a packet that starts with the accessory header, read here without the parser under test.
    fn opcode(sdu: &[u8]) -> Option<u16> {
        let rest = sdu.strip_prefix(&ACCESSORY_HEADER)?;
        rest.first_chunk::<2>()
            .map(|bytes| u16::from_le_bytes(*bytes))
    }

    #[test]
    fn the_first_burst_has_the_measured_census() {
        let burst = first_burst();
        assert_eq!(burst.len(), 38);
        assert_eq!(burst[0], handshake_ack());
        assert_eq!(opcode(&burst[0]), None);
        let mut opcodes: Vec<u16> = burst.iter().filter_map(|sdu| opcode(sdu)).collect();
        opcodes.sort_unstable();
        opcodes.dedup();
        let measured = [
            0x02, 0x04, 0x06, 0x08, 0x09, 0x0c, 0x0e, 0x1d, 0x2b, 0x2e, 0x4e, 0x53, 0x63,
        ];
        assert_eq!(opcodes, measured);
        let count = |wanted| {
            burst
                .iter()
                .filter(|sdu| opcode(sdu) == Some(wanted))
                .count()
        };
        assert_eq!((count(0x09), count(0x0c), count(0x2e)), (22, 3, 2));
    }

    #[test]
    fn the_metadata_packet_is_the_largest_and_the_measured_length() {
        let burst = first_burst();
        let largest = burst.iter().max_by_key(|sdu| sdu.len()).unwrap();
        assert_eq!(largest, &metadata());
        assert_eq!(metadata().len(), METADATA_LEN);
    }

    #[test]
    fn the_battery_packet_has_the_length_the_cpp_daemon_accepts() {
        assert_eq!(battery().len(), 22);
    }

    #[test]
    fn the_control_command_has_the_length_the_host_sends() {
        assert_eq!(control().len(), 11);
    }
}
