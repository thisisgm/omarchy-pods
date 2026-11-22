//! Adverts from the rig's pods, sealed with the sample keys a session stores, and the rig that reads them.

use pods_samples::advert::{Advert, HEADER_LEN, Keys, SEALED_LEN, advert_of};
use pods_samples::{IDENTITY_KEY, PAYLOAD_KEY};

use super::rig::Rig;

/// Paired AirPods Pro 3: the left bud primary and in the case, the right one in an ear, the case at 70 percent and charging, the lid open.
pub(crate) const HEADER: [u8; HEADER_LEN] = [
    0x07, 0x19, 0x01, 0x27, 0x20, 0x62, 0x88, 0x47, 0x00, 0x00, 0x04,
];
/// Primary 80 charging, secondary 60, case 40, then twelve zero bytes.
pub(crate) const PAYLOAD: [u8; SEALED_LEN] =
    [0x00, 0xd0, 0x3c, 0x28, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
/// The synthetic random part of every advert address, made resolvable by `advert_of`.
pub(crate) const RANDOM: [u8; 3] = [0x40, 0x00, 0x01];

pub(crate) fn keys(identity: [u8; 16]) -> Keys {
    Keys {
        identity,
        payload: PAYLOAD_KEY,
    }
}

pub(crate) fn ours(header: [u8; HEADER_LEN], payload: [u8; SEALED_LEN]) -> Advert {
    advert_of(header, payload, &keys(IDENTITY_KEY), RANDOM)
}

/// The rig with the keys stored and the manager served on its adapter; no daemon yet.
pub(crate) fn keyed() -> Rig {
    let rig = Rig::new();
    rig.session().store_sample_keys();
    rig.bluez().add_monitor_manager(rig.adapter());
    rig
}
