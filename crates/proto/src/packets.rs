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

    // Sample input: "040004000f00ffffffffff", a QByteArray::fromHex string from daemon/airpods_packets.h.
    fn from_hex(hex: &str) -> Vec<u8> {
        hex.as_bytes()
            .chunks(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn requests_match_the_cpp_connection_packets() {
        assert_eq!(
            HANDSHAKE.to_vec(),
            from_hex("00000400010002000000000000000000")
        );
        assert_eq!(
            SET_SPECIFIC_FEATURES.to_vec(),
            from_hex("040004004d00d700000000000000")
        );
        assert_eq!(
            REQUEST_NOTIFICATIONS.to_vec(),
            from_hex("040004000f00ffffffffff")
        );
    }
}
