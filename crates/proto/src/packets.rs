//! Fixed requests the host sends while it opens the control channel, and the replies that pace them.

// Request bytes are Connection in daemon/airpods_packets.h, reply prefixes are Parse.

/// The first packet on a new control channel.
pub const HANDSHAKE: [u8; 16] = [
    0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Sent once the handshake is acknowledged.
pub const SET_SPECIFIC_FEATURES: [u8; 14] = [
    0x04, 0x00, 0x04, 0x00, 0x4d, 0x00, 0xd7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Asks for battery, ear and setting notifications once the features are acknowledged.
pub const REQUEST_NOTIFICATIONS: [u8; 11] = [
    0x04, 0x00, 0x04, 0x00, 0x0f, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff,
];

/// A packet starting with these bytes acknowledges `HANDSHAKE`.
pub const HANDSHAKE_ACK_PREFIX: [u8; 4] = [0x01, 0x00, 0x04, 0x00];

/// A packet starting with these bytes acknowledges `SET_SPECIFIC_FEATURES`.
pub const FEATURES_ACK_PREFIX: [u8; 6] = [0x04, 0x00, 0x04, 0x00, 0x2b, 0x00];

#[cfg(test)]
mod tests {
    use super::*;
    use cpp_source::PACKETS;

    const CONNECTION: Option<&str> = Some("namespace Connection");
    const PARSE: Option<&str> = Some("namespace Parse");

    #[test]
    fn requests_match_the_cpp_connection_packets() {
        assert_eq!(HANDSHAKE.to_vec(), PACKETS.hex(CONNECTION, "HANDSHAKE"));
        assert_eq!(
            SET_SPECIFIC_FEATURES.to_vec(),
            PACKETS.hex(CONNECTION, "SET_SPECIFIC_FEATURES")
        );
        assert_eq!(
            REQUEST_NOTIFICATIONS.to_vec(),
            PACKETS.hex(CONNECTION, "REQUEST_NOTIFICATIONS")
        );
    }

    #[test]
    fn ack_prefixes_match_the_cpp_parse_packets() {
        assert_eq!(
            HANDSHAKE_ACK_PREFIX.to_vec(),
            PACKETS.hex(PARSE, "HANDSHAKE_ACK")
        );
        assert_eq!(
            FEATURES_ACK_PREFIX.to_vec(),
            PACKETS.hex(PARSE, "FEATURES_ACK")
        );
    }
}
