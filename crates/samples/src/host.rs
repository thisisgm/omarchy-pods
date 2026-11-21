//! Frames the host sends, read from `Connection` in `daemon/airpods_packets.h`.

use cpp_source::PACKETS;

const CONNECTION: Option<&str> = Some("namespace Connection");

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
