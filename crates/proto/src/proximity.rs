//! The proximity advert the pods broadcast while no link is up: a header in the clear, then one block sealed under the payload key.

use aes::Aes128;
use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use zeroize::Zeroizing;

use crate::addr::BdAddr;
use crate::battery::Charge;
use crate::ear::EarReport;
use crate::error::{ParseError, Parser};
use crate::keys::Key;

/// Apple's Bluetooth SIG company identifier, carried first in manufacturer-specific data.
pub const APPLE_COMPANY_ID: u16 = 0x004c;
/// The advertising data type for manufacturer-specific data.
pub const MANUFACTURER_DATA: u8 = 0xff;

/// The C++ `proximityPairingBytes`: a longer advert is read by its first `ADVERT_LEN` bytes, a shorter one is refused.
pub const ADVERT_LEN: usize = 27;
/// One AES block: the sealed end of the `ADVERT_LEN` bytes, and what `resolves` encrypts.
const SEALED_LEN: usize = 16;
/// `data[0]`, Apple's proximity pairing type.
pub const PROXIMITY_TYPE: u8 = 0x07;
/// `data[2]` of an advert in pairing mode, which the C++ skips as laid out differently.
const PAIRING_MODE: u8 = 0x00;
const STATUS_PRIMARY_LEFT: u8 = 0x20;
const STATUS_THIS_POD_IN_CASE: u8 = 0x40;
/// The primary bud's in ear bit when the pod sending is in the case, else the secondary's.
const STATUS_EAR_HIGH: u8 = 0x08;
const STATUS_EAR_LOW: u8 = 0x02;
/// `data[7]`: the case's tenths of charge in the low nibble, its charging flag in the high one.
const CASE_TENTHS: u8 = 0x0f;
const CASE_CHARGING: u8 = 0x40;
const PERCENT_PER_TENTH: u8 = 10;
const FULL: u8 = 100;
/// `data[8]`: the lid, read only from a pod in the case.
const LID_CLOSED: u8 = 0x08;
/// The two top bits of a resolvable private address, read first; theirs are `01`.
const ADDRESS_KIND: u8 = 0b1100_0000;
const RESOLVABLE: u8 = 0b0100_0000;

/// The fields the C++ `bleDeviceFound` reads from an advert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advert {
    /// The number `Modalias` prints for the product, which `model::product` takes.
    pub product: u16,
    pub primary_left: bool,
    pub this_pod_in_case: bool,
    pub ear: EarReport,
    pub case: Option<Charge>,
    pub lid: Lid,
    pub connection: ConnectionState,
    pub sealed: [u8; SEALED_LEN],
}

/// Numbered as the C++ `BleInfo::LidState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lid {
    Open,
    Closed,
    Unknown,
}

/// The advertising pods' connection state, including connections to other hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Idle,
    Music,
    Call,
    Ringing,
    HangingUp,
    Unknown(u8),
}

/// Sample input: `07 19 01 27 20 21 88 8F 11 00 04` then 16 sealed bytes, with `04` meaning connected and idle.
pub fn parse(data: &[u8]) -> Result<Advert, ParseError> {
    let Some((advert, _)) = data.split_first_chunk::<ADVERT_LEN>() else {
        return Err(ParseError::TooShort {
            parser: Parser::Proximity,
            need: ADVERT_LEN,
            got: data.len(),
        });
    };
    let [
        kind,
        _length,
        paired,
        product_low,
        product_high,
        status,
        _pods,
        case,
        lid,
        _color,
        connection,
        sealed @ ..,
    ] = *advert;
    let refused = |field, value| ParseError::UnknownValue {
        parser: Parser::Proximity,
        field,
        value,
    };
    if kind != PROXIMITY_TYPE {
        return Err(refused("type", kind));
    }
    if paired == PAIRING_MODE {
        return Err(refused("pairing state", paired));
    }
    let this_pod_in_case = status & STATUS_THIS_POD_IN_CASE != 0;
    let (primary_bit, secondary_bit) = if this_pod_in_case {
        (STATUS_EAR_HIGH, STATUS_EAR_LOW)
    } else {
        (STATUS_EAR_LOW, STATUS_EAR_HIGH)
    };
    let lid = match (this_pod_in_case, lid & LID_CLOSED != 0) {
        (false, _) => Lid::Unknown,
        (true, false) => Lid::Open,
        (true, true) => Lid::Closed,
    };
    let connection = match connection {
        0x00 => ConnectionState::Disconnected,
        0x04 => ConnectionState::Idle,
        0x05 => ConnectionState::Music,
        0x06 => ConnectionState::Call,
        0x07 => ConnectionState::Ringing,
        0x09 => ConnectionState::HangingUp,
        other => ConnectionState::Unknown(other),
    };
    Ok(Advert {
        product: u16::from_le_bytes([product_low, product_high]),
        primary_left: status & STATUS_PRIMARY_LEFT != 0,
        this_pod_in_case,
        ear: EarReport::from_in_ear(status & primary_bit != 0, status & secondary_bit != 0),
        case: case_charge(case),
        lid,
        connection,
        sealed,
    })
}

/// Whether `identity`, as the pods hand it over, made `address`: its hash is `ah(identity, prand)` of the Bluetooth core specification.
#[must_use]
pub fn resolves(address: BdAddr, identity: &Key) -> bool {
    let [r0, r1, r2, h0, h1, h2] = address.octets();
    if r0 & ADDRESS_KIND != RESOLVABLE {
        return false;
    }
    let mut key = Zeroizing::new(*identity.expose());
    key.reverse();
    let mut block = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, r0, r1, r2].into();
    Aes128::new((&*key).into()).encrypt_block(&mut block);
    let [.., a, b, c]: [u8; SEALED_LEN] = block.into();
    // A plain compare: the pods broadcast valid pairs, and an advert gets no reply to time.
    [a, b, c] == [h0, h1, h2]
}

/// The sealed block opened as `decryptLastBytes` opens it, one zero IV block, so one AES-128 decryption.
#[must_use]
pub fn open(sealed: &[u8; SEALED_LEN], payload: &Key) -> [u8; SEALED_LEN] {
    let mut block = (*sealed).into();
    Aes128::new(payload.expose().into()).decrypt_block(&mut block);
    block.into()
}

// 0x44: charging, four tenths, so 40 percent; a nibble past ten tenths, 15 the unknown one, reads as none.
fn case_charge(byte: u8) -> Option<Charge> {
    let level = (byte & CASE_TENTHS).saturating_mul(PERCENT_PER_TENTH);
    (level <= FULL).then_some(Charge {
        level,
        charging: byte & CASE_CHARGING != 0,
    })
}

#[cfg(test)]
mod tests;
