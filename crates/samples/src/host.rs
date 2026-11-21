//! Frames the host sends, as `Connection` in `daemon/airpods_packets.h` has them.

const HANDSHAKE: [u8; 16] = [
    0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
const SET_SPECIFIC_FEATURES: [u8; 14] = [
    0x04, 0x00, 0x04, 0x00, 0x4d, 0x00, 0xd7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
const REQUEST_NOTIFICATIONS: [u8; 11] = [
    0x04, 0x00, 0x04, 0x00, 0x0f, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff,
];

#[must_use]
pub fn handshake() -> Vec<u8> {
    HANDSHAKE.to_vec()
}

#[must_use]
pub fn set_specific_features() -> Vec<u8> {
    SET_SPECIFIC_FEATURES.to_vec()
}

#[must_use]
pub fn request_notifications() -> Vec<u8> {
    REQUEST_NOTIFICATIONS.to_vec()
}

/// The same eleven bytes as the pods' report of the setting.
#[must_use]
pub fn setting(identifier: u8, value: u8) -> Vec<u8> {
    super::setting(identifier, value)
}
