//! Fixed requests the host sends while it opens the control channel.

// Byte values are Connection in daemon/airpods_packets.h.

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

#[cfg(test)]
mod tests {
    use super::*;
    use cpp_source::PACKETS;

    const CONNECTION: Option<&str> = Some("namespace Connection");

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
}
