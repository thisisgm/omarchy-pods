//! Sample control channel packets for tests: documented layouts, synthetic contents.

// Layouts from daemon/airpods_packets.h and parseMetadata in daemon/main.cpp; no real device bytes.

pub mod advert;
pub mod host;

/// Starts every packet the pods send after the handshake; the opcode follows.
const ACCESSORY_HEADER: [u8; 4] = [0x04, 0x00, 0x04, 0x00];
/// `Parse::HANDSHAKE_ACK`.
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

/// Listening mode; value 1 off, 2 noise cancelling, 3 transparency, 4 adaptive.
pub const NOISE: u8 = 0x0d;
/// Noise cancelling with one bud in; value 1 on, 2 off.
pub const ONE_BUD_ANC: u8 = 0x1b;
/// Value 1 on, 2 off.
pub const CONVERSATION_AWARENESS: u8 = 0x28;
/// Adaptive audio level; value 0 to 100.
pub const ADAPTIVE_LEVEL: u8 = 0x2e;
/// Whether the pods offer the Off listening mode; value 1 yes, 2 no.
pub const ALLOW_OFF_OPTION: u8 = 0x34;

/// One part's battery: percent, and whether it charges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub level: u8,
    pub charging: bool,
}

/// Where a bud is: `00`, `01` and `02` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placed {
    InEar,
    OutOfEar,
    InCase,
}

impl Placed {
    fn wire(self) -> u8 {
        match self {
            Self::InEar => 0x00,
            Self::OutOfEar => 0x01,
            Self::InCase => 0x02,
        }
    }
}

pub const NAME: &str = "Test\u{2019}s AirPods Pro";
pub const MODEL_NUMBER: &str = "A3063";
pub const LEFT: Cell = Cell {
    level: 99,
    charging: true,
};
pub const RIGHT: Cell = Cell {
    level: 100,
    charging: false,
};
pub const CASE: Cell = Cell {
    level: 50,
    charging: false,
};

/// Battery entry kinds and marks: `kind 01 level status 01`.
const BATTERY_RIGHT: u8 = 0x02;
const BATTERY_LEFT: u8 = 0x04;
const BATTERY_CASE: u8 = 0x08;
const BATTERY_MARK: u8 = 0x01;
const BATTERY_CHARGING: u8 = 0x01;
const BATTERY_NOT_CHARGING: u8 = 0x02;
const BATTERY_ENTRIES: u8 = 3;

/// The three value bytes behind the first one of a setting.
const SETTING_TAIL: [u8; 3] = [0x00, 0x00, 0x00];
const BURST_SETTINGS: u8 = 22;
/// The value of every setting of the first burst that is not overridden.
const BURST_SETTING_VALUE: u8 = 0x01;

/// Synthetic bytes between the metadata header and the name, which no parser reads.
const METADATA_SKIPPED: [u8; 5] = [0x01, 0x02, 0x03, 0x04, 0x05];
const METADATA_MANUFACTURER: &str = "Apple Inc.";
/// A synthetic serial number in the fourth string, where the real packet carries one.
pub const METADATA_SERIAL: &str = "SERIAL0001";
/// The length of the metadata packet measured on a real device, reached with zero bytes.
const METADATA_LEN: usize = 247;

/// Opcodes of the first burst that no parser reads, each as often as it was measured.
const BURST_OTHER_OPCODES: [u16; 11] = [
    0x02, 0x08, 0x0c, 0x0c, 0x0c, 0x0e, 0x2e, 0x2e, 0x4e, 0x53, 0x63,
];

/// Opcode 0x0006 with body `00 00` gives `04 00 04 00 06 00 00 00`.
fn packet(opcode: u16, body: &[u8]) -> Vec<u8> {
    let mut sdu = ACCESSORY_HEADER.to_vec();
    sdu.extend(opcode.to_le_bytes());
    sdu.extend(body);
    sdu
}

#[must_use]
pub fn handshake_ack() -> Vec<u8> {
    [&HANDSHAKE_ACK_PREFIX[..], &HANDSHAKE_ACK_TAIL].concat()
}

/// The reply to the set specific features request.
#[must_use]
pub fn features_ack() -> Vec<u8> {
    packet(OPCODE_FEATURES_ACK, &FILLER)
}

#[must_use]
pub fn battery() -> Vec<u8> {
    battery_of(LEFT, RIGHT, CASE)
}

/// Entries in the order right, left, case.
#[must_use]
pub fn battery_of(left: Cell, right: Cell, case: Cell) -> Vec<u8> {
    let mut body = vec![BATTERY_ENTRIES];
    for (kind, cell) in [
        (BATTERY_RIGHT, right),
        (BATTERY_LEFT, left),
        (BATTERY_CASE, case),
    ] {
        let status = if cell.charging {
            BATTERY_CHARGING
        } else {
            BATTERY_NOT_CHARGING
        };
        body.extend([kind, BATTERY_MARK, cell.level, status, BATTERY_MARK]);
    }
    packet(OPCODE_BATTERY, &body)
}

#[must_use]
pub fn ear() -> Vec<u8> {
    ear_of(Placed::InEar, Placed::InEar)
}

#[must_use]
pub fn ear_of(primary: Placed, secondary: Placed) -> Vec<u8> {
    packet(OPCODE_EAR, &[primary.wire(), secondary.wire()])
}

/// Conversation awareness switched on.
#[must_use]
pub fn control() -> Vec<u8> {
    setting(CONVERSATION_AWARENESS, BURST_SETTING_VALUE)
}

/// `04 00 04 00 09 00 <identifier> <value> 00 00 00`, from either side.
#[must_use]
pub fn setting(identifier: u8, value: u8) -> Vec<u8> {
    let body = [&[identifier, value][..], &SETTING_TAIL].concat();
    packet(OPCODE_CONTROL, &body)
}

#[must_use]
pub fn metadata() -> Vec<u8> {
    metadata_of(NAME, MODEL_NUMBER)
}

/// Padded with zero bytes to the measured length, never cut below the fields it carries.
#[must_use]
pub fn metadata_of(name: &str, model_number: &str) -> Vec<u8> {
    let mut body = METADATA_SKIPPED.to_vec();
    for field in [name, model_number, METADATA_MANUFACTURER, METADATA_SERIAL] {
        body.extend(field.as_bytes());
        body.push(0);
    }
    let mut sdu = packet(OPCODE_METADATA, &body);
    sdu.resize(sdu.len().max(METADATA_LEN), 0);
    sdu
}

/// Identifiers 1 to 22 in order, each with value 1 unless `overrides` names it.
#[must_use]
pub fn burst_settings(overrides: &[(u8, u8)]) -> Vec<Vec<u8>> {
    (1..=BURST_SETTINGS)
        .map(|identifier| {
            let value = overrides
                .iter()
                .rev()
                .find(|(overridden, _)| *overridden == identifier)
                .map_or(BURST_SETTING_VALUE, |(_, value)| *value);
            setting(identifier, value)
        })
        .collect()
}

/// The 11 packets of the first burst that no parser reads.
#[must_use]
pub fn burst_others() -> Vec<Vec<u8>> {
    BURST_OTHER_OPCODES
        .iter()
        .map(|opcode| packet(*opcode, &FILLER))
        .collect()
}

/// 38 packets: opcode counts and first two match a real device; later order and bodies are made up.
#[must_use]
pub fn first_burst() -> Vec<Vec<u8>> {
    let mut burst = vec![
        handshake_ack(),
        features_ack(),
        battery(),
        ear(),
        metadata(),
    ];
    burst.extend(burst_settings(&[]));
    burst.extend(burst_others());
    burst
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPCODE_LEN: usize = 2;

    /// Not the parser under test: 0x0006 for `04 00 04 00 06 00 00 00`, none for `01 00 04 00 ..`.
    fn opcode(sdu: &[u8]) -> Option<u16> {
        let rest = sdu.strip_prefix(&ACCESSORY_HEADER)?;
        rest.first_chunk::<OPCODE_LEN>()
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

    #[test]
    fn a_setting_is_the_eleven_bytes_the_cpp_daemon_writes() {
        // What `ControlCommand::createCommand` in daemon/BasicControlCommand.hpp returns.
        let written = [
            [
                0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x28, 0x02, 0x00, 0x00, 0x00,
            ],
            [
                0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x1b, 0x01, 0x00, 0x00, 0x00,
            ],
        ];
        assert_eq!(setting(CONVERSATION_AWARENESS, 2), written[0]);
        assert_eq!(setting(ONE_BUD_ANC, 1), written[1]);
    }

    #[test]
    fn no_opening_frame_carries_the_setting_opcode() {
        for frame in [
            host::handshake(),
            host::set_specific_features(),
            host::request_notifications(),
        ] {
            assert_ne!(opcode(&frame), Some(OPCODE_CONTROL));
        }
    }

    #[test]
    fn burst_settings_take_overrides_inside_their_identifiers_only() {
        let plain = burst_settings(&[]);
        let overridden = burst_settings(&[(NOISE, 3), (ADAPTIVE_LEVEL, 50), (ALLOW_OFF_OPTION, 1)]);
        let noise = usize::from(NOISE) - 1;
        assert_eq!(overridden[noise], setting(NOISE, 3));
        assert_eq!(plain[noise], setting(NOISE, 1));
        let mut rest = overridden.clone();
        rest[noise] = plain[noise].clone();
        assert_eq!(rest, plain);
    }

    #[test]
    fn the_off_option_identifier_is_the_cpp_one() {
        let cpp = cpp_source::PACKETS.byte(
            Some("namespace AllowOffOption"),
            "BasicControlCommand<",
            '>',
        );
        assert_eq!(ALLOW_OFF_OPTION, cpp);
    }
}
