//! Fixed requests the host sends to open the control channel, and the replies that pace them.

// Request bytes are Connection and MagicPairing in daemon/airpods_packets.h, reply prefixes are Parse.

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

/// Sent once per link when the metadata arrives; answered by the proximity keys reply.
pub const REQUEST_PROXIMITY_KEYS: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x30, 0x00, 0x05, 0x00];

/// A packet starting with these bytes acknowledges `HANDSHAKE`.
pub const HANDSHAKE_ACK_PREFIX: [u8; 4] = [0x01, 0x00, 0x04, 0x00];

/// A packet starting with these bytes acknowledges `SET_SPECIFIC_FEATURES`.
pub const FEATURES_ACK_PREFIX: [u8; 6] = [0x04, 0x00, 0x04, 0x00, 0x2b, 0x00];

/// The one upstream librepods connects to.
pub const CONTROL_PSM: u16 = 0x1001;

/// `ATT` in the Bluetooth Assigned Numbers list of PSMs.
pub const ATT_PSM: u16 = 0x001F;

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
        assert_eq!(
            REQUEST_PROXIMITY_KEYS.to_vec(),
            PACKETS.hex(
                pods_samples::host::MAGIC_PAIRING,
                "REQUEST_MAGIC_CLOUD_KEYS"
            )
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

    // Bluetooth Core, Vol 3 Part A 4.2: PSMs are odd, upper octet even, dynamic ones from 0x1001.
    #[test]
    fn control_psm_is_a_valid_dynamic_psm() {
        let [upper, lower] = CONTROL_PSM.to_be_bytes();
        assert_eq!(lower % 2, 1);
        assert_eq!(upper % 2, 0);
        assert!(upper >= 0x10);
    }

    // Bluetooth Core, Vol 3 Part A 4.2: PSMs are odd, upper octet even, SIG ones end at 0x0EFF.
    #[test]
    fn att_psm_is_a_valid_assigned_psm() {
        let [upper, lower] = ATT_PSM.to_be_bytes();
        assert_eq!(lower % 2, 1);
        assert_eq!(upper % 2, 0);
        assert!(upper <= 0x0e);
    }
}
