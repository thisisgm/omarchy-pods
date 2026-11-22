//! Frames the host sends, read from `Connection` and `MagicPairing` in `daemon/airpods_packets.h`.

use cpp_source::PACKETS;

const CONNECTION: Option<&str> = Some("namespace Connection");

pub const MAGIC_PAIRING: Option<&str> = Some("namespace MagicPairing");

#[must_use]
pub fn handshake() -> Vec<u8> {
    PACKETS.hex(CONNECTION, "HANDSHAKE")
}

#[must_use]
pub fn set_specific_features() -> Vec<u8> {
    PACKETS.hex(CONNECTION, "SET_SPECIFIC_FEATURES")
}

#[must_use]
pub fn request_notifications() -> Vec<u8> {
    PACKETS.hex(CONNECTION, "REQUEST_NOTIFICATIONS")
}

#[must_use]
pub fn request_proximity_keys() -> Vec<u8> {
    PACKETS.hex(MAGIC_PAIRING, "REQUEST_MAGIC_CLOUD_KEYS")
}
